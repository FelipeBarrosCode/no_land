import { useEffect, useId, useRef, useState } from "react";
import { Button } from "../../components/ui/Button";
import { feedbackReviewEligibility, feedbackStageReview } from "../../lib/feedbackBackend";
import { feedbackInstanceId, feedbackViewerKey, normalizeFeedbackReview, stageFeedbackThenDestroy } from "../../lib/feedbackModel";
import { useFeedbackStore } from "./feedbackStore";

export function DestroyFeedbackConfirmation({ instanceId, disabled, onDestroy, onCancel }: {
  instanceId: number;
  disabled: boolean;
  onDestroy: (instanceId: number) => Promise<void>;
  onCancel: () => void;
}) {
  const status = useFeedbackStore((s) => s.status);
  const setWarning = useFeedbackStore((s) => s.setWarning);
  const invalidate = useFeedbackStore((s) => s.invalidate);
  const [eligibilityKey, setEligibilityKey] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [score, setScore] = useState<number | null>(null);
  const [comment, setComment] = useState("");
  const [running, setRunning] = useState(false);
  const [destroyError, setDestroyError] = useState<string | null>(null);
  const [staged, setStaged] = useState(false);
  const stagedFingerprint = useRef<string | null>(null);
  const started = useRef(false);
  const radioGroup = useId();
  const commentId = useId();
  const providerInstanceId = feedbackInstanceId(instanceId);
  const viewer = feedbackViewerKey(status);
  const reviewKey = viewer && providerInstanceId ? JSON.stringify([viewer, providerInstanceId]) : null;

  useEffect(() => {
    let disposed = false;
    setEligibilityKey(null); setScore(null); setComment(""); setStaged(false);
    stagedFingerprint.current = null;
    if (!viewer || !providerInstanceId) { setChecking(false); return; }
    setChecking(true);
    // Local journal/frame evidence only; paired/connected UI flags are not evidence.
    void feedbackReviewEligibility(providerInstanceId).then((value) => {
      if (!disposed) setEligibilityKey(value ? reviewKey : null);
    }).catch(() => { if (!disposed) setEligibilityKey(null); })
      .finally(() => { if (!disposed) setChecking(false); });
    return () => { disposed = true; };
  }, [viewer, providerInstanceId, reviewKey]);

  const showReview = status.enabled && reviewKey !== null && eligibilityKey === reviewKey;
  const draft = normalizeFeedbackReview(score, comment);
  async function destroy(submit: boolean) {
    if (started.current || disabled || (submit && (!showReview || !draft))) return;
    started.current = true;
    setRunning(true); setDestroyError(null);
    const performDestroy = async () => { await onDestroy(instanceId); onCancel(); };
    try {
      if (showReview && providerInstanceId && feedbackViewerKey(useFeedbackStore.getState().status) === viewer) {
        // Native defines both nulls as an independent revisioned tombstone.
        // Skip must not accidentally publish a draft from a failed prior destroy.
        const review = submit && draft ? draft : { score: null, comment: null };
        const fingerprint = JSON.stringify(review);
        await stageFeedbackThenDestroy(async () => {
          if (stagedFingerprint.current === fingerprint) return;
          await feedbackStageReview(providerInstanceId, review.score, review.comment);
          stagedFingerprint.current = fingerprint; setStaged(submit);
        }, performDestroy, (message) => setWarning(submit ? message :
          "A previous review draft could not be confirmed removed. Destruction will continue; an earlier native-staged review may still publish after confirmed deletion."));
      } else {
        await performDestroy();
      }
    } catch {
      invalidate();
      setDestroyError("Backup or destruction could not be confirmed. The rental has not been marked destroyed by this UI. Any native-staged review is retained for retry and will publish only after confirmed provider deletion.");
    } finally { started.current = false; setRunning(false); }
  }

  return (
    <div id={`destroy-confirm-${instanceId}`} role="group" aria-label="Confirm rental destruction"
      className="min-w-0 space-y-3 rounded border border-red-500/40 bg-red-900/20 p-3 text-[1.05rem] leading-snug text-red-200 [overflow-wrap:anywhere] [&_button]:max-w-full [&_button>span]:min-w-0 [&_button>span]:flex-wrap"
      onKeyDown={(event) => { if (event.key === "Escape" && !started.current) { event.stopPropagation(); onCancel(); } }}>
      <p><strong>Destroy rental {instanceId}?</strong> This runs the existing backup/destruction flow. Confirm only when you are ready to end this rental.</p>
      {checking && <p role="status" className="text-[#9ec4df]">Checking local review eligibility. You can destroy without waiting.</p>}
      {showReview && <fieldset disabled={running || disabled} className="space-y-3 border border-[#495188] p-3 text-[#c6dbf4]">
        <legend>Optional feedback · actual stream-frame evidence recorded</legend>
        <p>How smooth was streaming on this machine?</p>
        <div role="radiogroup" aria-label="Streaming smoothness rating, optional" className="grid grid-cols-[repeat(auto-fit,minmax(3.5rem,1fr))] gap-2">
          {Array.from({ length: 10 }, (_, index) => index + 1).map((value) => <label key={value} className={`flex cursor-pointer items-center justify-center gap-1 rounded border p-2 ${score === value ? "border-neon-cyan bg-neon-cyan/10" : "border-[#495188]"}`}>
            <input type="radio" name={radioGroup} value={value} checked={score === value} onChange={() => setScore(value)} aria-label={`${value} out of 10`} />{value}
          </label>)}
        </div>
        <p className="text-sm">1 — Very choppy · 10 — Very smooth. No score is selected by default.</p>
        {score !== null && <Button variant="ghost" onClick={() => setScore(null)}>Clear rating</Button>}
        <label htmlFor={commentId} className="block">Comment (optional, up to 2,000 characters)</label>
        <textarea id={commentId} maxLength={2000} rows={4} className="w-full border border-[#495188] bg-[#0b0f23] p-2 text-[#dff8ff]"
          value={comment} onChange={(event) => setComment(event.target.value)} aria-describedby={`${commentId}-help`} />
        <p id={`${commentId}-help`} className="text-sm">{comment.length}/2000. Published reviews are visible to enrolled participants. Do not include personal information, exact locations, credentials, or other secrets.</p>
        <p className="text-sm">Submit saves a local draft before destruction; no feedback network response is required. Native publishes only after confirmed deletion. Skip stages no score/comment and clears any prior draft from a failed destroy. Cancel before starting saves nothing and keeps the rental.</p>
      </fieldset>}
      {staged && destroyError && <p role="status">A review was already saved for this rental. Retry will not add another vote. Skip clears that draft before trying destruction again. Cancel keeps the rental but does not retract an earlier staged draft; it remains unpublished until confirmed destruction.</p>}
      {destroyError && <p role="alert">{destroyError}</p>}
      <div className="flex flex-wrap gap-2">
        {showReview && <Button variant="danger" disabled={disabled || running || !draft} onClick={() => void destroy(true)}>Submit and destroy</Button>}
        <Button variant="danger" disabled={disabled || running} loading={running} loadingText="Destroying…" onClick={() => void destroy(false)}>{showReview ? "Skip and destroy" : "Confirm destroy"}</Button>
        <Button variant="ghost" disabled={running || disabled} onClick={onCancel}>Cancel</Button>
      </div>
      {running && <p role="status">Destruction has started. Cancellation is no longer available here; feedback errors will not block the provider action.</p>}
    </div>
  );
}
