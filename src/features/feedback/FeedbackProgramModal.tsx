import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../../components/ui/Button";
import { translate } from "../../lib/i18n";
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
      if (!disposed) setNoticeError(translate("feedback.program.noticeError"));
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
      setResultMessage(translate("feedback.program.joined"));
    }
  }
  async function showPolicy(url: string) {
    setLinkError(null);
    try { await openUrl(url); }
    catch { setLinkError(translate("feedback.program.linkError")); }
  }
  const setProfileValue = (key: keyof FeedbackOptionalProfile, value: string) => setProfile((old) => ({ ...old, [key]: value }));

  return (
    <FeedbackDialog title={translate("feedback.program")} onClose={onClose}>
      <section className="space-y-2 rounded border border-[#495188] bg-[#0b0f23] p-3">
        <h3 className="font-display text-xs text-neon-cyan">{translate("feedback.program.heading")}</h3>
        <p>{translate("feedback.program.optional")}</p>
        <p>{translate("feedback.program.collected")}</p>
        <p>{translate("feedback.program.identityBefore")} <strong>{translate("feedback.program.pseudonymous")}</strong>{translate("feedback.program.identityAfter")}</p>
        <p>{translate("feedback.program.visibility")}</p>
        <p>{translate("feedback.program.retention")}</p>
        <p>{translate("feedback.program.readPolicies")}</p>
      </section>

      <section className="space-y-2" aria-label={translate("feedback.program.status")} aria-live="polite">
        <p><strong>{status.enabled ? translate("feedback.program.contributing") : translate("feedback.program.notContributing")}</strong></p>
        <p>{!status.configured ? translate("feedback.program.notConfigured") :
          !status.supported ? translate("feedback.program.notSupported") :
          translate("feedback.program.enrollment", { status: status.enrollmentStatus.replace(/_/g, " ") })}</p>
        {status.collectionPaused && <p className="text-[#ffd78a]">{translate("feedback.program.paused")}</p>}
        {status.statusMessage && <p className="break-words text-[#ffd78a]">{status.statusMessage}</p>}
        {status.pendingControlAction && <p className="rounded border border-[#ffd78a] p-2 text-[#ffd78a]">
          {translate("feedback.program.pendingControl", { action: status.pendingControlAction.kind === "delete" ? translate("feedback.program.deletion") : status.pendingControlAction.enabled ? translate("feedback.program.consentChange") : translate("feedback.program.withdrawal") })}
        </p>}
        <p>{translate("feedback.program.pendingSummaries", { count: status.pendingSessions })}</p>
        {status.receiptId && <p className="break-all">{translate("feedback.program.receipt", { receipt: status.receiptId, status: status.receiptStatus || translate("feedback.program.unconfirmed") })}</p>}
        {statusError && <p role="alert" className="text-[#ffd78a]">{statusError}</p>}
        {controlError && <p role="alert" className="text-[#ffd78a]">{controlError}</p>}
        {resultMessage && <p role="status">{resultMessage}</p>}
        {busy && <p role="status">{translate("feedback.program.busy")}</p>}
        <Button variant="ghost" disabled={busy} onClick={() => void refreshStatus()}>{translate("feedback.program.refresh")}</Button>
      </section>

      <section className="space-y-2" aria-label={translate("feedback.program.policies")}>
        {loadingNotices && <p role="status">{translate("feedback.program.loading")}</p>}
        {noticeError && <p role="alert" className="text-[#ffd78a]">{noticeError}</p>}
        {notices && <>
          {!notices.available && <p className="text-[#ffd78a]">{translate("feedback.program.unavailable")}</p>}
          {notices.collectionPaused && <p className="text-[#ffd78a]">{translate("feedback.program.enrollmentPaused")}</p>}
          <p className="whitespace-pre-wrap break-words">{notices.disclosure}</p>
          <p className="break-words">{translate("feedback.program.contact", { contact: notices.contact || translate("feedback.program.noContact") })}</p>
          <div className="flex flex-wrap gap-2">
            {[[translate("feedback.program.privacy"), notices.privacyUrl], [translate("feedback.program.terms"), notices.termsUrl], [translate("feedback.program.deletionResource"), notices.deletionUrl]].map(([label, url]) =>
              url ? <Button key={label} variant="ghost" onClick={() => void showPolicy(url)}>{label}</Button> : <span key={label}>{translate("feedback.program.policyUnavailable", { label })}</span>)}
          </div>
          <p className="text-sm">{translate("feedback.program.versions", { privacy: notices.privacyVersion, terms: notices.termsVersion })}</p>
        </>}
        {linkError && <p role="alert" className="text-[#ffd78a]">{linkError}</p>}
        <Button variant="ghost" disabled={loadingNotices || busy} onClick={() => { setAccepted(false); setNoticeAttempt((n) => n + 1); }}>{translate("feedback.program.reload")}</Button>
      </section>

      <fieldset disabled={busy} className="space-y-3 border border-[#495188] p-3">
        <legend>{translate("feedback.program.context")}</legend>
        <label className="flex items-start gap-2">
          <input type="checkbox" className="mt-1 h-5 w-5 shrink-0" checked={optionalContext}
            onChange={(e) => { setOptionalContext(e.target.checked); if (!e.target.checked) setProfile({}); }} />
          <span>{translate("feedback.program.contextConsent")}</span>
        </label>
        {!status.enabled && !rejoining && optionalContext && <div className="grid gap-3 sm:grid-cols-2">
          <label>{translate("feedback.program.country")}
            <input className={inputClass} maxLength={2} placeholder={translate("feedback.program.countryExample")} value={profile.country ?? ""}
              onChange={(e) => setProfileValue("country", e.target.value.toUpperCase().replace(/[^A-Z]/g, ""))} />
          </label>
          <label>{translate("feedback.program.region")}
            <select className={inputClass} value={profile.region ?? ""} onChange={(e) => setProfileValue("region", e.target.value)}>
              <option value="">{translate("feedback.program.preferNot")}</option>
              {["Africa", "Asia", "Europe", "North America", "South America", "Oceania"].map((value) => <option key={value}>{value}</option>)}
            </select>
          </label>
          <label>{translate("feedback.program.connection")}
            <select className={inputClass} value={profile.connectionCategory ?? ""} onChange={(e) => setProfileValue("connectionCategory", e.target.value)}>
              <option value="">{translate("feedback.program.preferNot")}</option>
              {["ethernet", "wifi", "cellular", "other", "unknown"].map((value) => <option key={value} value={value}>{value}</option>)}
            </select>
          </label>
          <label>{translate("feedback.program.bandwidth")}
            <select className={inputClass} value={profile.bandwidthTier ?? ""} onChange={(e) => setProfileValue("bandwidthTier", e.target.value)}>
              <option value="">{translate("feedback.program.preferNot")}</option>
              {["under_25_mbps", "25_100_mbps", "100_300_mbps", "over_300_mbps", "unknown"].map((value) => <option key={value} value={value}>{value.replace(/_/g, " ")}</option>)}
            </select>
          </label>
        </div>}
        {status.enabled && <>
          <p>{translate("feedback.program.contextRemoval")}</p>
          <Button variant="secondary" disabled={optionalContext === status.optionalContextEnabled || status.pendingControlAction !== null}
            onClick={() => void consent(true, optionalContext)}>{translate("feedback.program.saveContext")}</Button>
        </>}
      </fieldset>

      {!status.enabled && !deleting && <section className="space-y-3">
        {deletionAcknowledged && <p>{translate("feedback.program.deleted")}</p>}
        {status.enrollmentStatus === "pending" && <p className="text-[#ffd78a]">{translate("feedback.program.recovery")}</p>}
        {!profileValid && <p role="alert" className="text-[#ffd78a]">{translate("feedback.program.invalidCountry")}</p>}
        {rejoining && <p>{translate("feedback.program.rejoining")}</p>}
        {!policyMatches && <p role="alert" className="text-[#ffd78a]">{translate("feedback.program.policyChanged")}</p>}
        <label className="flex items-start gap-2">
          <input type="checkbox" className="mt-1 h-5 w-5 shrink-0" checked={accepted} disabled={busy || !canJoin}
            onChange={(e) => setAccepted(e.target.checked)} />
          <span>{translate("feedback.program.participationConsent")}</span>
        </label>
        <div className="flex flex-wrap gap-2">
          <Button disabled={!canJoin || !accepted || busy} loading={busy} onClick={() => void join()}>{rejoining ? translate("feedback.program.rejoin") : deletionAcknowledged ? translate("feedback.program.joinAgain") : translate("feedback.program.join")}</Button>
          <Button variant="ghost" disabled={busy} onClick={onClose}>{translate("feedback.program.notNow")}</Button>
        </div>
      </section>}

      {hasIdentity && <section className="space-y-3 border-t border-[#495188] pt-3">
        <h3 className="font-display text-xs">{translate("feedback.program.stopOrDelete")}</h3>
        {!deleting && status.enrollmentStatus === "enrolled" && <Button variant="ghost" disabled={busy} onClick={() => void consent(false, false)}>{translate("feedback.program.stop")}</Button>}
        <p>{translate("feedback.program.deleteHelp")}</p>
        {!confirmDelete ? <Button variant="danger" disabled={busy} onClick={() => setConfirmDelete(true)}>{translate("feedback.program.delete")}</Button> : <div className="space-y-2 rounded border border-[#ff8ca2] p-3">
          <p>{translate("feedback.program.deleteConfirmHelp")}</p>
          <div className="flex flex-wrap gap-2">
            <Button variant="danger" disabled={busy} onClick={() => { void deleteContributions().then(() => setConfirmDelete(false)); }}>{translate("feedback.program.deleteConfirm")}</Button>
            <Button variant="ghost" disabled={busy} onClick={() => setConfirmDelete(false)}>{translate("common.cancel")}</Button>
          </div>
        </div>}
      </section>}
    </FeedbackDialog>
  );
}
