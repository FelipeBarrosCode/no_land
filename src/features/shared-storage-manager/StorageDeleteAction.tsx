import { useState } from "react";
import { createPortal } from "react-dom";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import { prepareStorageDeletion, type StorageDeletionPreview } from "../../lib/backend";
import { useAppStore } from "../../store/appStore";
import { translate } from "../../lib/i18n";
import { deletionErrorMessage } from "../../lib/storageDeletionProgress";

interface Props {
  instanceId: number | null;
  target: StorageDeletionPreview["target"];
  selectedPaths: string[];
  disabled: boolean;
  onBusyChange: (busy: boolean) => void;
  onStarted: () => void;
}

export function StorageDeleteAction({ instanceId, target, selectedPaths, disabled, onBusyChange, onStarted }: Props) {
  const startDeletion = useAppStore(state => state.startStorageDeletion);
  const storageAction = useAppStore(state => state.backgroundStorageAction);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [preview, setPreview] = useState<(StorageDeletionPreview & { instanceId: number }) | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [acknowledgedDependents, setAcknowledgedDependents] = useState(false);
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
    <Button variant="danger" disabled={disabled || working || !!storageAction || instanceId === null || !explicit}
      onClick={() => void run(async () => {
        setDialogOpen(true);
        setPreview(null);
        setAcknowledgedDependents(false);
        const selectedInstance = instanceId!;
        setPreview({ ...await prepareStorageDeletion(selectedInstance, target, selectedPaths), instanceId: selectedInstance });
      })}>{label}</Button>
    {(dialogOpen || preview || error) && createPortal(
      <ModalFrame nonBlocking panelClassName="glass-panel pixel-frame max-w-2xl" zIndexClassName="z-[125]" labelledBy="storage-delete-title">
        <h2 id="storage-delete-title" className="border-b border-[#3e4270] p-5 font-display text-white">{label} · {preview?.instanceId ?? instanceId}</h2>
        <ModalBody className="space-y-3 p-5 text-[#d7e8ff]">
          {error && <p role="alert" className="text-red-300">{error}</p>}
          {working && !preview && <p role="status">{translate("storage.delete.preparing")}</p>}
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
        </ModalBody>
        <div className="flex justify-end gap-3 border-t border-[#3e4270] p-5">
          <Button variant="ghost" disabled={working} onClick={() => { setDialogOpen(false); setPreview(null); setError(null); }}>{translate("generated.7d9eb7acb13e2462")}</Button>
          {preview && <Button variant="danger" loading={working} loadingText={translate("storage.delete.working")}
            disabled={target === "cloud" && (preview.fullDeletion !== true || ((preview.dependentBundles ?? 0) > 0 && !acknowledgedDependents))}
            onClick={() => void run(async () => {
              await startDeletion(preview.instanceId, preview);
              setDialogOpen(false);
              setPreview(null);
              onStarted();
            })}>{translate("storage.delete.confirm")}</Button>}
        </div>
      </ModalFrame>, document.body)}
  </>;
}
