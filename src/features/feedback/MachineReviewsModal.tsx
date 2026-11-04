import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/ui/Button";
import { translate } from "../../lib/i18n";
import { feedbackViewerKey } from "../../lib/feedbackModel";
import type { FeedbackReview } from "../../lib/feedbackTypes";
import type { OfferCandidate } from "../../lib/types";
import { FeedbackDialog } from "./FeedbackDialog";
import { FeedbackReviewContent } from "./FeedbackReviewContent";
import { useFeedbackStore } from "./feedbackStore";
import { useFeedbackSummaries } from "./useFeedbackSummaries";

export function MachineReviewsModal({ machineId, offer, onClose }: { machineId: string; offer: OfferCandidate; onClose: () => void }) {
  const status = useFeedbackStore((s) => s.status);
  const revision = useFeedbackStore((s) => s.dataRevision);
  const error = useFeedbackStore((s) => s.dataError);
  const load = useFeedbackStore((s) => s.loadReviews);
  const retry = useFeedbackStore((s) => s.retryReads);
  const report = useFeedbackStore((s) => s.report);
  const block = useFeedbackStore((s) => s.block);
  const [reviews, setReviews] = useState<FeedbackReview[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [expiresAt, setExpiresAt] = useState(0);
  const [readKey, setReadKey] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const [actionReview, setActionReview] = useState<string | null>(null);
  const [action, setAction] = useState<"report" | "block" | null>(null);
  const [reason, setReason] = useState("");
  const [actionBusy, setActionBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const viewer = feedbackViewerKey(status);
  const key = JSON.stringify([viewer, machineId, revision, tick]);
  const liveKey = useRef(key);
  liveKey.current = key;
  const seenCursors = useRef(new Set<string>());
  const pageInFlight = useRef<string | null>(null);
  const summaryResult = useFeedbackSummaries([machineId], status.enabled);
  const summary = summaryResult.summaries[machineId];

  async function loadPage(next: string | null, append: boolean, expected: string) {
    if (pageInFlight.current === expected) return;
    pageInFlight.current = expected;
    setLoading(true);
    try {
      const page = await load(machineId, next);
      if (liveKey.current !== expected) return;
      if (next !== null) seenCursors.current.add(next);
      setReviews((old) => [...new Map([...(append ? old : []), ...page.value.reviews].map((review) => [review.reviewId, review])).values()]);
      const nextCursor = page.value.nextCursor;
      setCursor(nextCursor && !seenCursors.current.has(nextCursor) ? nextCursor : null);
      setExpiresAt((old) => append ? Math.min(old, page.expiresAt) : page.expiresAt);
      setReadKey(expected);
    } catch { if (liveKey.current === expected) { setReadKey(null); setReviews([]); setCursor(null); } }
    finally {
      if (pageInFlight.current === expected) pageInFlight.current = null;
      if (liveKey.current === expected) setLoading(false);
    }
  }
  useEffect(() => {
    liveKey.current = key;
    setReadKey(null); setReviews([]); setCursor(null); setExpiresAt(0); setAction(null); setActionReview(null);
    seenCursors.current = new Set();
    if (viewer && !error) void loadPage(null, false, key);
    else setLoading(false);
    return () => { if (liveKey.current === key) liveKey.current = "disposed"; };
    // key captures the viewer, invalidation generation, machine and expiry cycle.
  }, [key, viewer, error, load]);
  useEffect(() => {
    if (!expiresAt) return;
    const timer = setTimeout(() => { setReviews([]); setTick((n) => n + 1); }, Math.max(1, expiresAt - Date.now()));
    return () => clearTimeout(timer);
  }, [expiresAt]);

  async function moderate() {
    if (!actionReview || !action || actionBusy || !status.enabled || (action === "report" && !reason)) return;
    const expected = key;
    const expectedViewer = viewer;
    const chosenAction = action;
    setActionBusy(true);
    setMessage(null);
    try {
      if (action === "report") await report(actionReview, reason);
      else await block(actionReview);
      // Successful moderation intentionally changes the data revision, but a
      // different authenticated viewer must not inherit this action's message.
      if (feedbackViewerKey(useFeedbackStore.getState().status) !== expectedViewer || liveKey.current === "disposed") return;
      setMessage(chosenAction === "report" ? translate("feedback.reviews.reported") : translate("feedback.reviews.blocked"));
      setAction(null); setActionReview(null); setReason("");
    } catch {
      if (liveKey.current === expected) setMessage(translate("feedback.reviews.error"));
    }
    finally { setActionBusy(false); }
  }
  // Guard during render, not only in the effect: effects run after the new
  // viewer renders and must never expose the previous viewer's cached page.
  const currentPage = !!viewer && !error && readKey === key && expiresAt > Date.now();
  const visibleReviews = currentPage ? reviews : [];
  return (
    <FeedbackDialog title={translate("feedback.reviews.title")} busy={actionBusy} onClose={onClose}>
      <div className="min-w-0 space-y-1 break-words [overflow-wrap:anywhere]">
        <p className="font-display text-xs text-neon-cyan">{offer.gpuName}</p>
        <p>{offer.hostLabel} · {offer.locationLabel} · {translate("feedback.reviews.machine", { machineId })}</p>
        <p>{translate("feedback.reviews.description")}</p>
        <p>{translate("feedback.reviews.summary", { score: summary?.reviews.averageScore?.toFixed(1) ?? translate("feedback.na"), ratings: summary?.reviews.ratingCount ?? translate("feedback.na"), comments: summary?.reviews.commentCount ?? translate("feedback.na") })}</p>
        <p className="text-sm">{translate("feedback.reviews.countHelp")}</p>
      </div>
      {!status.enabled && <p role="status">{translate("feedback.reviews.disabled")}</p>}
      {error && <div role="alert" className="space-y-2 text-[#ffd78a]"><p>{error}</p><Button variant="ghost" disabled={!status.enabled} onClick={retry}>{translate("feedback.retry")}</Button></div>}
      {loading && <p role="status">{translate("feedback.reviews.loading")}</p>}
      {currentPage && !loading && visibleReviews.length === 0 && <p>{translate("feedback.reviews.empty")}</p>}
      {currentPage && summary?.reviews.commentCount === 0 && visibleReviews.length > 0 && <p>{translate("feedback.reviews.noComments")}</p>}
      <ul className="space-y-3" aria-label={translate("feedback.reviews.published")}>
        {visibleReviews.map((review) => <li key={review.reviewId} className="space-y-2 rounded border border-[#495188] bg-[#0b0f23] p-3">
          <FeedbackReviewContent review={review} />
          <div className="flex flex-wrap gap-2">
            <Button variant="ghost" disabled={actionBusy} aria-label={translate("feedback.reviews.reportLabel")} onClick={() => { setActionReview(review.reviewId); setAction("report"); setReason(""); }}>{translate("feedback.reviews.report")}</Button>
            <Button variant="ghost" disabled={actionBusy} aria-label={translate("feedback.reviews.blockLabel")} onClick={() => { setActionReview(review.reviewId); setAction("block"); }}>{translate("feedback.reviews.block")}</Button>
          </div>
          {actionReview === review.reviewId && action && <div className="space-y-2 border-t border-[#495188] pt-2">
            {action === "report" ? <label>{translate("feedback.reviews.reason")}
              <select className="mt-1 block w-full border border-[#495188] bg-[#0b0f23] p-2" value={reason} onChange={(e) => setReason(e.target.value)} disabled={actionBusy}>
                <option value="">{translate("feedback.reviews.chooseReason")}</option><option value="spam">{translate("feedback.reviews.spam")}</option><option value="harassment">{translate("feedback.reviews.harassment")}</option><option value="private_information">{translate("feedback.reviews.private")}</option><option value="unlawful_content">{translate("feedback.reviews.unlawful")}</option><option value="other">{translate("feedback.reviews.other")}</option>
              </select>
            </label> : <p>{translate("feedback.reviews.blockHelp")}</p>}
            <div className="flex flex-wrap gap-2"><Button variant="secondary" disabled={actionBusy || (action === "report" && !reason)} onClick={() => void moderate()}>{action === "report" ? translate("feedback.reviews.send") : translate("feedback.reviews.confirmBlock")}</Button>
              <Button variant="ghost" disabled={actionBusy} onClick={() => { setAction(null); setActionReview(null); }}>{translate("common.cancel")}</Button></div>
          </div>}
        </li>)}
      </ul>
      {message && <p role="status">{message}</p>}
      {currentPage && cursor && <Button variant="secondary" loading={loading} onClick={() => void loadPage(cursor, true, key)}>{translate("feedback.reviews.more")}</Button>}
      <p className="text-sm text-[#9ec4df]">{translate("feedback.reviews.privacy")}</p>
    </FeedbackDialog>
  );
}
