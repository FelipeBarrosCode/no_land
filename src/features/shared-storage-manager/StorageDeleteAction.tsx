import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import { confirmStorageDeletion, prepareStorageDeletion, subscribeSharedStorageProgress, type StorageDeletionPreview } from "../../lib/backend";
import { translate } from "../../lib/i18n";
import type { SharedStorageProgressEvent } from "../../lib/types";
import { deletionErrorMessage, deletionProgressFor, secondsSinceDeletionProgress } from "../../lib/storageDeletionProgress";

interface Props {
  instanceId: number | null;
  target: StorageDeletionPreview["target"];
  selectedPaths: string[];
  disabled: boolean;
  onBusyChange: (busy: boolean) => void;
  onDeleted: () => Promise<void>;
}

export function StorageDeleteAction({ instanceId, target, selectedPaths, disabled, onBusyChange, onDeleted }: Props) {
  const [preview, setPreview] = useState<(StorageDeletionPreview & { instanceId: number }) | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [completed, setCompleted] = useState(false);
  const [acknowledgedDependents, setAcknowledgedDependents] = useState(false);
  const [progressEvent, setProgressEvent] = useState<SharedStorageProgressEvent | null>(null);
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!preview) return;
    let active = true;
    let unsubscribe: (() => void) | undefined;
    void subscribeSharedStorageProgress(event => {
      if (active && deletionProgressFor(preview, event)) setProgressEvent(event);
    }).then(unlisten => {
      if (active) unsubscribe = unlisten;
      else unlisten();
    }).catch(() => { /* The authoritative confirmation request still reports its result. */ });
    return () => { active = false; unsubscribe?.(); };
  }, [preview]);
  useEffect(() => {
    if (!working) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [working]);
  const progress = progressEvent?.deletion;
  const secondsSinceProgress = secondsSinceDeletionProgress(progress?.last_success_at ?? null, now);
  const label = translate(target === "cloud" ? "storage.delete.cloud" : "storage.delete.pc");
  const help = translate(target === "cloud" ? "storage.delete.cloudHelp" : "storage.delete.pcHelp");
  const run = async (action: () => Promise<void>) => {
    setWorking(true);
    onBusyChange(true);
    setError(null);
    try { await action(); }
    catch (reason) { setError(deletionErrorMessage(reason)); }
    finally { setWorking(false); onBusyChange(false); }
  };
  const explicit = selectedPaths.length > 0 && selectedPaths.every(path => target === "cloud"
    ? /^\/(catalog|retained-backups)\/[^/]+\/[^/]+$/.test(path) || (selectedPaths.length === 1 && /^\/pending-deletions\/[^/]+$/.test(path))
    : /^\/apps\/[^/*]+$/.test(path));

  return <>
    <Button variant="danger" disabled={disabled || working || instanceId === null || !explicit}
      onClick={() => void run(async () => {
        setCompleted(false);
        setProgressEvent(null);
        setAcknowledgedDependents(false);
        const selectedInstance = instanceId!;
        setPreview({ ...await prepareStorageDeletion(selectedInstance, target, selectedPaths), instanceId: selectedInstance });
      })}>{label}</Button>
    {(preview || error) && createPortal(
      <ModalFrame panelClassName="glass-panel pixel-frame max-w-2xl" zIndexClassName="z-[60]" labelledBy="storage-delete-title">
        <h2 id="storage-delete-title" className="border-b border-[#3e4270] p-5 font-display text-white">{label} · {preview?.instanceId ?? instanceId}</h2>
        <ModalBody className="space-y-3 p-5 text-[#d7e8ff]">
          {error && <p role="alert" className="text-red-300">{error}</p>}
          {preview && <>
            <p>{help}</p>
            <ul className="max-h-64 list-inside list-disc overflow-auto break-words">{preview.names.map(name => <li key={name}>{name}</li>)}</ul>
            <p>{translate("storage.delete.items", { count: preview.items, bytes: preview.bytes.toLocaleString() })}</p>
            {target === "cloud" && <>
              <p>{translate("storage.delete.physical", { count: preview.packCount ?? 0, bytes: (preview.physicalBytes ?? 0).toLocaleString() })}</p>
              {(preview.dependentBundles ?? 0) > 0 && <>
                <p role="alert" className="text-red-300">{translate("storage.delete.dependents", { count: preview.dependentBundles ?? 0 })}</p>
                <label className="flex items-start gap-2"><input type="checkbox" checked={acknowledgedDependents}
                  onChange={event => setAcknowledgedDependents(event.target.checked)} disabled={working} />
                  {translate("storage.delete.ackDependents")}</label>
              </>}
            </>}
            {preview.skipped > 0 && <p>{translate("storage.delete.skipped", { count: preview.skipped })}</p>}
            {preview.samplePaths.length > 0 && <ul className="max-h-40 overflow-auto break-all font-mono text-sm">{preview.samplePaths.map(path => <li key={path}>{path}</li>)}</ul>}
          </>}
          {progress && !completed && <>
            <div role="status" aria-live="polite" className="space-y-2">
              <p>{translate(`storage.delete.phase.${progress.phase}`)}</p>
              <progress className="w-full" max={Math.max(1, progress.total_objects)} value={progress.completed_objects}
                aria-label={translate("storage.delete.working")} />
              <p>{translate("storage.delete.progress", {
                objects: progress.completed_objects, total: progress.total_objects,
                packs: progress.completed_packs, totalPacks: progress.total_packs,
                bytes: progress.verified_absent_bytes.toLocaleString(),
              })}</p>
              <p className="text-sm">{translate("storage.delete.quotaNote")}</p>
            </div>
            {secondsSinceProgress !== null && working && <p aria-live="off" className="text-sm">
              {translate("storage.delete.lastProgress", { seconds: secondsSinceProgress })}
            </p>}
            {progressEvent?.state === "RECONNECTING" && <p role="alert">{progressEvent.message}</p>}
            {progressEvent?.state === "MONITORING_PAUSED" && <p role="alert">{translate("storage.monitoringPaused")}</p>}
          </>}
          {completed && <p role="status">{translate("storage.delete.completed")}</p>}
        </ModalBody>
        <div className="flex justify-end gap-3 border-t border-[#3e4270] p-5">
          <Button variant="ghost" disabled={working} onClick={() => { setPreview(null); setError(null); }}>{translate("generated.7d9eb7acb13e2462")}</Button>
          {preview && !completed && <Button variant="danger" loading={working} loadingText={translate("storage.delete.working")}
            disabled={target === "cloud" && (preview.fullDeletion !== true || ((preview.dependentBundles ?? 0) > 0 && !acknowledgedDependents))}
            onClick={() => void run(async () => {
              await confirmStorageDeletion(preview.instanceId, preview);
              setCompleted(true);
              await onDeleted();
            })}>{translate("storage.delete.confirm")}</Button>}
        </div>
      </ModalFrame>, document.body)}
  </>;
}
