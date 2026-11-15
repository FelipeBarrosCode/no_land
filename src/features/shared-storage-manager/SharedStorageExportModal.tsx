import { translate } from "../../lib/i18n";
import { useEffect, useMemo, useState } from "react";
import { BlockingLoaderOverlay, type BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import { StorageDeleteAction } from "./StorageDeleteAction";
import type {
  SharedStorageObjectEntry,
} from "../../lib/types";
import { StorageBrowseStatus } from "./StorageBrowseStatus";
import { useStorageBrowse } from "./useStorageBrowse";

interface Props {
  open: boolean;
  busy: boolean;
  instanceId: number | null;
  onClose: () => void;
  onLoadObjects: (instanceId: number) => Promise<SharedStorageObjectEntry[] | null>;
  onConfirmExport: (
    selectedPaths: string[],
  ) => Promise<void>;
  onRefreshIndexing?: (instanceId: number) => Promise<void>;
}

function buildChildrenMap(entries: SharedStorageObjectEntry[]) {
  const map = new Map<string, SharedStorageObjectEntry[]>();
  for (const entry of entries) {
    const key = entry.parentPath || "/";
    const current = map.get(key) ?? [];
    current.push(entry);
    map.set(key, current);
  }
  for (const value of map.values()) {
    value.sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
  }
  return map;
}

export function SharedStorageExportModal({
  open,
  busy,
  instanceId,
  onClose,
  onConfirmExport,
  onRefreshIndexing
}: Props) {
  const browse = useStorageBrowse(open, instanceId, "remote_pc");
  const entries = browse.entries;
  const [loading, setLoading] = useState(false);
  const [selectedPaths, setSelectedPaths] = useState<string[]>([]);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({ "/": true });
  const [pendingAction, setPendingAction] = useState<BlockingActionState | null>(null);

  useEffect(() => {
    setSelectedPaths([]);
  }, [open, instanceId, browse.snapshot?.scope]);

  useEffect(() => {
    if (browse.snapshot?.freshness === "fresh") {
      const available = new Set(entries.map(entry => entry.path));
      setSelectedPaths(paths => paths.filter(path => available.has(path)));
    }
  }, [browse.snapshot, entries]);

  const childrenMap = useMemo(() => buildChildrenMap(entries), [entries]);
  if (!open) return null;

  const toggleSelected = (path: string) => {
    setSelectedPaths((current) =>
      current.includes(path) ? current.filter((item) => item !== path) : [...current, path]
    );
  };

  const toggleExpanded = (path: string) => {
    setExpanded((current) => ({ ...current, [path]: !current[path] }));
  };

  const renderNode = (entry: SharedStorageObjectEntry, depth: number) => {
    const isChecked = selectedPaths.includes(entry.path);
    const children = childrenMap.get(entry.path) ?? [];
    const isExpanded = expanded[entry.path] ?? depth < 1;
    return (
      <div key={`${entry.path}-${entry.isDir ? "dir" : "file"}`}>
        <div className="flex items-center gap-2 rounded border border-[#30365d] bg-[#11162a] px-2 py-1" style={{ marginLeft: `${depth * 12}px` }}>
          {entry.isDir ? (
            <button type="button" className="w-5 text-left text-[#7ab6ff]" onClick={() => toggleExpanded(entry.path)}>
              {isExpanded ? "▾" : "▸"}
            </button>
          ) : (
            <span className="w-5 text-[#7ab6ff]">•</span>
          )}
          <input type="checkbox" checked={isChecked} onChange={() => toggleSelected(entry.path)} className="h-4 w-4" />
          <span className="truncate text-[1.15rem] text-[#d7e8ff]">{entry.name}{entry.isDir ? "/" : ""}</span>
        </div>
        {entry.isDir && isExpanded && children.map((child) => renderNode(child, depth + 1))}
      </div>
    );
  };

  const roots = childrenMap.get("/") ?? [];

  return (
    <ModalFrame panelClassName="glass-panel pixel-frame max-w-3xl">
        <div className="shrink-0 flex items-center justify-between border-b-2 border-[#3e4270] px-5 py-4">
          <div>
            <h2 className="font-display text-base text-white">{translate("storage.games.upload")}</h2>
            <p className="text-[1.2rem] text-[#b4c8de]">{translate("storage.backup.select")}</p>
          </div>
          <Button variant="ghost" onClick={onClose} disabled={busy || loading}>{translate("generated.7d9eb7acb13e2462")}</Button>
        </div>
        <ModalBody className="px-5 py-4">
          <StorageBrowseStatus {...browse} onRefresh={browse.reload} onPrepareHelper={browse.prepareHelper} />
          {loading || (!browse.snapshot && browse.refreshing) ? (
            pendingAction ? <BlockingLoaderOverlay action={pendingAction} inline className="max-w-none p-4" /> : <p className="text-[1.2rem] text-[#b4c8de]">{translate("generated.db69feb354d65693")}</p>
          ) : roots.length === 0 && browse.snapshot ? (
            <p className="text-[1.2rem] text-[#b4c8de]">{translate("generated.648afc13de8af296")}</p>
          ) : (
            <div className="space-y-1">{roots.map((entry) => renderNode(entry, 0))}</div>
          )}
          <p className="mt-4 text-[1.1rem] text-[#9ec0e4]">{translate("storage.backup.completeHelp")}</p>
        </ModalBody>
        <div className="shrink-0 flex flex-wrap items-end justify-between gap-3 border-t-2 border-[#3e4270] px-5 py-4">
          <div className="flex items-end gap-4">
            <p className="pb-2 text-[1.1rem] text-[#9ec0e4]">
              {translate("generated.bef9d85df8ea13fe")} {selectedPaths.length}
            </p>
          </div>
          <div className="flex items-center gap-2">
            <StorageDeleteAction instanceId={instanceId} target="remote_pc" selectedPaths={selectedPaths}
              disabled={busy || loading || browse.preparingHelper || browse.refreshing || browse.snapshot?.freshness !== "fresh" || !!browse.error} onBusyChange={setLoading} onStarted={onClose} />
            {onRefreshIndexing && (
              <Button
                variant="ghost"
                disabled={busy || loading || browse.preparingHelper || browse.refreshing}
                onClick={async () => {
                  if (instanceId !== null) {
                    setLoading(true);
                    setPendingAction({
                      key: "export-modal.refresh",
                      label: "Refreshing indexed applications",
                      detail: "Running the discovery agent on the remote instance.",
                      mode: "indeterminate",
                      progress: null,
                      startedAt: Date.now()
                    });
                    try {
                      await onRefreshIndexing(instanceId);
                      browse.reload();
                      setSelectedPaths([]);
                    } finally {
                      setLoading(false);
                      setPendingAction(null);
                    }
                  }
                }}
              >
                {translate("generated.f09ae0be0e10c568")}
              </Button>
            )}
            <Button variant="ghost" onClick={onClose} disabled={busy || loading}>{translate("generated.19766ed6ccb2f4a3")}</Button>
            <Button
              disabled={busy || loading || browse.preparingHelper || browse.refreshing || browse.snapshot?.freshness !== "fresh" || !!browse.error || selectedPaths.length === 0}
              loading={busy}
              loadingText={`${translate("storage.games.upload")}…`}
              onClick={async () => {
                setPendingAction({
                  key: "export-modal.run",
                  label: translate("storage.games.upload"),
                  detail: translate("storage.backup.completeHelp"),
                  mode: "indeterminate",
                  progress: null,
                  startedAt: Date.now()
                });
                await onConfirmExport(selectedPaths);
                setPendingAction(null);
              }}
            >
              {translate("storage.games.upload")}
            </Button>
          </div>
        </div>
    </ModalFrame>
  );
}
