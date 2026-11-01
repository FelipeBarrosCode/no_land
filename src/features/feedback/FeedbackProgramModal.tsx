import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../../components/ui/Button";
import { feedbackProgramNotices } from "../../lib/feedbackBackend";
import { canJoinFeedback } from "../../lib/feedbackModel";
import type { FeedbackOptionalProfile, FeedbackProgramDocument } from "../../lib/feedbackTypes";
import { FeedbackDialog } from "./FeedbackDialog";
import { useFeedbackStore } from "./feedbackStore";

const inputClass = "mt-1 w-full border border-[#495188] bg-[#0b0f23] p-2 text-[#dff8ff]";

export function FeedbackProgramModal({ onClose }: { onClose: () => void }) {
  const status = useFeedbackStore((s) => s.status);
  const statusError = useFeedbackStore((s) => s.statusError);
  const controlError = useFeedbackStore((s) => s.controlError);
  const busy = useFeedbackStore((s) => s.controlBusy);
  const refreshStatus = useFeedbackStore((s) => s.refreshStatus);
  const enroll = useFeedbackStore((s) => s.enroll);
  const consent = useFeedbackStore((s) => s.consent);
  const deleteContributions = useFeedbackStore((s) => s.deleteContributions);
  const [notices, setNotices] = useState<FeedbackProgramDocument | null>(null);
  const [noticeError, setNoticeError] = useState<string | null>(null);
  const [loadingNotices, setLoadingNotices] = useState(false);
  const [noticeAttempt, setNoticeAttempt] = useState(0);
  const [accepted, setAccepted] = useState(false);
  const [optionalContext, setOptionalContext] = useState(false);
  const [profile, setProfile] = useState<FeedbackOptionalProfile>({});
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [linkError, setLinkError] = useState<string | null>(null);
  const [resultMessage, setResultMessage] = useState<string | null>(null);

  // Mounting follows an explicit click on Feedback program. This is the sole
  // off-state program-document read, not a startup/background API request.
  useEffect(() => {
    let disposed = false;
    setLoadingNotices(true);
    setNoticeError(null);
    setNotices(null);
    setAccepted(false);
    void feedbackProgramNotices().then(async (document) => {
      if (!disposed) {
        setNotices(document);
        // Reading notices may change native's cached collection-pause gate.
        await refreshStatus();
      }
    }).catch(() => {
      if (!disposed) setNoticeError("Program information is unavailable. No enrollment was started. You can still stop or request deletion using local settings.");
    }).finally(() => { if (!disposed) setLoadingNotices(false); });
    void refreshStatus();
    return () => { disposed = true; };
  }, [noticeAttempt, refreshStatus]);

  useEffect(() => {
    setOptionalContext(status.enabled && status.optionalContextEnabled);
    setAccepted(false);
    setProfile({});
  }, [status.enabled, status.optionalContextEnabled, status.consentEpoch]);

  const hasIdentity = status.userId !== null || status.installationId !== null;
  const deletionAcknowledged = status.enrollmentStatus === "deleted" && status.pendingControlAction === null &&
    !status.userId && !status.installationId && !status.credentialRef && !status.deletionCodeRef;
  const deleting = status.pendingControlAction?.kind === "delete" ||
    ((status.enrollmentStatus.includes("delet") || !!status.receiptId) && !deletionAcknowledged);
  const rejoining = hasIdentity && !!status.credentialRef && !deleting;
  const policyMatches = !rejoining || (notices?.privacyVersion === status.privacyVersion && notices?.termsVersion === status.termsVersion);
  const profileValid = !optionalContext || !profile.country || profile.country.length === 2;
  const canJoin = canJoinFeedback(status, notices) && policyMatches && profileValid && !status.enabled && !deleting;
  async function join() {
    if (!accepted || !canJoin || !notices) return;
    const optionalProfile = optionalContext
      ? Object.fromEntries(Object.entries(profile).filter(([, value]) => value?.trim())) as FeedbackOptionalProfile : {};
    const ok = rejoining
      ? await consent(true, optionalContext)
      : await enroll({ privacyVersion: notices.privacyVersion, termsVersion: notices.termsVersion,
        participationAccepted: true, optionalContextEnabled: optionalContext, optionalProfile });
    if (ok) {
      setAccepted(false);
      setProfile({});
      setResultMessage("Local status refreshed. Contributions begin only with the next new rental, once consent is usable.");
    }
  }
  async function showPolicy(url: string) {
    setLinkError(null);
    try { await openUrl(url); }
    catch { setLinkError("The policy link could not be opened. Enrollment remains optional; try again before agreeing."); }
  }
  const setProfileValue = (key: keyof FeedbackOptionalProfile, value: string) => setProfile((old) => ({ ...old, [key]: value }));

  return (
    <FeedbackDialog title="Feedback program" onClose={onClose}>
      <section className="space-y-2 rounded border border-[#495188] bg-[#0b0f23] p-3">
        <h3 className="font-display text-xs text-neon-cyan">Help improve machine recommendations</h3>
        <p>Joining is optional. Renting, pairing, streaming, backup, and destruction work without joining. Contributions start with your next new rental, not one already running.</p>
        <p>When enabled, No Land contributes machine and rental identifiers, setup milestones, outcomes and timestamps, streaming-jitter summaries, and the app/platform versions needed to interpret them. Jitter can reflect your connection, route, and client as well as the machine.</p>
        <p>A random installation identifier links your contributions. No name or email is required, and no MAC address is collected. This is <strong>pseudonymous, not fully anonymous</strong>. Requests expose an IP address to the service infrastructure.</p>
        <p>Machine results and optional published ratings/comments are visible to enrolled participants; detailed records are available to authorized operators/processors. No gameplay, microphone content, files, payment details, or Vast credentials are contributed.</p>
        <p>Optional connection context and broad profile details are a separate choice. Reviews are optional. Stop contributing clears unsent contributions, but previously accepted history remains until retention expiry or deletion. An already received request may have been accepted.</p>
        <p>Read the current server-published policies below for the operator, processors, retention periods, security, moderation, and deletion commitments before joining.</p>
      </section>

      <section className="space-y-2" aria-label="Program status" aria-live="polite">
        <p><strong>{status.enabled ? "Contributing · next-new-rental scope" : "Not contributing locally"}</strong></p>
        <p>{!status.configured ? "Unavailable: this build has no configured feedback service." :
          !status.supported ? "Unavailable: native feedback or protected storage support is not ready on this platform." :
          `Enrollment: ${status.enrollmentStatus.replace(/_/g, " ")}`}</p>
        {status.collectionPaused && <p className="text-[#ffd78a]">Collection is paused by the service. Feedback reads and enrollment are off; rental actions are unaffected.</p>}
        {status.statusMessage && <p className="break-words text-[#ffd78a]">{status.statusMessage}</p>}
        {status.pendingControlAction && <p className="rounded border border-[#ffd78a] p-2 text-[#ffd78a]">
          Pending control: {status.pendingControlAction.kind === "delete" ? "deletion" : status.pendingControlAction.enabled ? "consent/context change" : "withdrawal"}. Local collection is off while consent is unresolved. A withdrawal/deletion is not complete until the service acknowledges it. Native retains the minimal protected control data needed to synchronize; rejoining is disabled meanwhile.
        </p>}
        <p>Pending rental summaries: {status.pendingSessions}</p>
        {status.receiptId && <p className="break-all">Deletion receipt: {status.receiptId}{status.receiptStatus ? ` · ${status.receiptStatus}` : " · completion not yet confirmed"}</p>}
        {statusError && <p role="alert" className="text-[#ffd78a]">{statusError}</p>}
        {controlError && <p role="alert" className="text-[#ffd78a]">{controlError}</p>}
        {resultMessage && <p role="status">{resultMessage}</p>}
        {busy && <p role="status">A native program change is in progress. You can close this dialog to return to your rental; closing does not cancel an enrollment or privacy request already started.</p>}
        <Button variant="ghost" disabled={busy} onClick={() => void refreshStatus()}>Refresh local status</Button>
      </section>

      <section className="space-y-2" aria-label="Service policies">
        {loadingNotices && <p role="status">Loading server-published program information…</p>}
        {noticeError && <p role="alert" className="text-[#ffd78a]">{noticeError}</p>}
        {notices && <>
          {!notices.available && <p className="text-[#ffd78a]">The operator has not made enrollment available.</p>}
          {notices.collectionPaused && <p className="text-[#ffd78a]">The service has paused collection. New enrollment is unavailable.</p>}
          <p className="whitespace-pre-wrap break-words">{notices.disclosure}</p>
          <p className="break-words">Privacy/reporting contact: {notices.contact || "Not published — enrollment unavailable"}</p>
          <div className="flex flex-wrap gap-2">
            {[["Privacy Policy", notices.privacyUrl], ["Feedback Program Terms", notices.termsUrl], ["External deletion resource", notices.deletionUrl]].map(([label, url]) =>
              url ? <Button key={label} variant="ghost" onClick={() => void showPolicy(url)}>{label}</Button> : <span key={label}>{label}: unavailable</span>)}
          </div>
          <p className="text-sm">Privacy version {notices.privacyVersion} · Terms version {notices.termsVersion}. Links open in your browser.</p>
        </>}
        {linkError && <p role="alert" className="text-[#ffd78a]">{linkError}</p>}
        <Button variant="ghost" disabled={loadingNotices || busy} onClick={() => { setAccepted(false); setNoticeAttempt((n) => n + 1); }}>Reload service information</Button>
      </section>

      <fieldset disabled={busy} className="space-y-3 border border-[#495188] p-3">
        <legend>Optional connection context</legend>
        <label className="flex items-start gap-2">
          <input type="checkbox" className="mt-1 h-5 w-5 shrink-0" checked={optionalContext}
            onChange={(e) => { setOptionalContext(e.target.checked); if (!e.target.checked) setProfile({}); }} />
          <span>I also choose to share broad connection/profile context. This is not required to join or rent.</span>
        </label>
        {!status.enabled && !rejoining && optionalContext && <div className="grid gap-3 sm:grid-cols-2">
          <label>Country code (optional)
            <input className={inputClass} maxLength={2} placeholder="e.g. CA" value={profile.country ?? ""}
              onChange={(e) => setProfileValue("country", e.target.value.toUpperCase().replace(/[^A-Z]/g, ""))} />
          </label>
          <label>Broad region (optional)
            <select className={inputClass} value={profile.region ?? ""} onChange={(e) => setProfileValue("region", e.target.value)}>
              <option value="">Prefer not to say</option>
              {["Africa", "Asia", "Europe", "North America", "South America", "Oceania"].map((value) => <option key={value}>{value}</option>)}
            </select>
          </label>
          <label>Connection category (optional)
            <select className={inputClass} value={profile.connectionCategory ?? ""} onChange={(e) => setProfileValue("connectionCategory", e.target.value)}>
              <option value="">Prefer not to say</option>
              {["ethernet", "wifi", "cellular", "other", "unknown"].map((value) => <option key={value} value={value}>{value}</option>)}
            </select>
          </label>
          <label>Bandwidth tier (optional)
            <select className={inputClass} value={profile.bandwidthTier ?? ""} onChange={(e) => setProfileValue("bandwidthTier", e.target.value)}>
              <option value="">Prefer not to say</option>
              {["under_25_mbps", "25_100_mbps", "100_300_mbps", "over_300_mbps", "unknown"].map((value) => <option key={value} value={value}>{value.replace(/_/g, " ")}</option>)}
            </select>
          </label>
        </div>}
        {status.enabled && <>
          <p>Turning this off also removes saved optional profile/context from pending and retained contributions through native/server synchronization.</p>
          <Button variant="secondary" disabled={optionalContext === status.optionalContextEnabled || status.pendingControlAction !== null}
            onClick={() => void consent(true, optionalContext)}>Save context preference</Button>
        </>}
      </fieldset>

      {!status.enabled && !deleting && <section className="space-y-3">
        {deletionAcknowledged && <p>Your deletion request has been acknowledged. Joining again creates a new participant identity for future rentals, without restoring deleted contributions. Note your receipt above before starting again.</p>}
        {status.enrollmentStatus === "pending" && <p className="text-[#ffd78a]">Enrollment recovery is pending. Retry with the same optional-context and profile choices as the original request. The native protected proof is retained for retry; this dialog cannot recover forgotten optional choices. Contact the published operator if recovery has expired.</p>}
        {!profileValid && <p role="alert" className="text-[#ffd78a]">Leave country blank or enter a two-letter country code.</p>}
        {rejoining && <p>Rejoining starts a new consent epoch with your next new rental. Old unsent snapshots are not revived. Profile details are not restored.</p>}
        {!policyMatches && <p role="alert" className="text-[#ffd78a]">Published policy versions have changed. This native build cannot renew those versions through rejoin; update the app before joining again.</p>}
        <label className="flex items-start gap-2">
          <input type="checkbox" className="mt-1 h-5 w-5 shrink-0" checked={accepted} disabled={busy || !canJoin}
            onChange={(e) => setAccepted(e.target.checked)} />
          <span>I agree to contribute the measurements and setup outcomes described above and accept the server-published Feedback Program Terms. I have read the Privacy Policy.</span>
        </label>
        <div className="flex flex-wrap gap-2">
          <Button disabled={!canJoin || !accepted || busy} loading={busy} onClick={() => void join()}>{rejoining ? "Rejoin feedback program" : deletionAcknowledged ? "Join feedback program again" : "Join feedback program"}</Button>
          <Button variant="ghost" disabled={busy} onClick={onClose}>Not now</Button>
        </div>
      </section>}

      {hasIdentity && <section className="space-y-3 border-t border-[#495188] pt-3">
        <h3 className="font-display text-xs">Stop or delete</h3>
        {!deleting && status.enrollmentStatus === "enrolled" && <Button variant="ghost" disabled={busy} onClick={() => void consent(false, false)}>Stop contributing</Button>}
        <p>Stopping does not delete accepted history. Deletion disables locally immediately and requests removal of your participant data, sessions, ratings, and comments. Offline deletion remains pending until acknowledged; keep this installation’s protected credentials until then.</p>
        {!confirmDelete ? <Button variant="danger" disabled={busy} onClick={() => setConfirmDelete(true)}>Delete my contributions…</Button> : <div className="space-y-2 rounded border border-[#ff8ca2] p-3">
          <p>This requests deletion of all contributions from this installation’s participant identity. It does not destroy rentals. Continue?</p>
          <div className="flex flex-wrap gap-2">
            <Button variant="danger" disabled={busy} onClick={() => { void deleteContributions().then(() => setConfirmDelete(false)); }}>Confirm deletion request</Button>
            <Button variant="ghost" disabled={busy} onClick={() => setConfirmDelete(false)}>Cancel</Button>
          </div>
        </div>}
      </section>}
    </FeedbackDialog>
  );
}
