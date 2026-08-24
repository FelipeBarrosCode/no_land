import { useEffect, useMemo, useState } from "react";
import { BlockingLoaderOverlay, type BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import type { SharedStorageObjectEntry } from "../../lib/types";

interface Props {
  open: boolean;
  busy: boolean;
  instanceId: number | null;
  onClose: () => void;
  onLoadObjects: (instanceId: number) => Promise<SharedStorageObjectEntry[] | null>;
  onConfirmSync: (selectedPaths: string[]) => Promise<void>;
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
      if (a.isDir !== b.isDir) {
        return a.isDir ? -1 : 1;
      }
      return a.name.localeCompare(b.name);
    });
  }
  return map;
}

export function SharedStorageSyncModal({
  open,
  busy,
  instanceId,
  onClose,
  onLoadObjects,
  onConfirmSync
}: Props) {
  const [entries, setEntries] = useState<SharedStorageObjectEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedPaths, setSelectedPaths] = useState<string[]>([]);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({ "/": true });
  const [pendingAction, setPendingAction] = useState<BlockingActionState | null>(null);

  useEffect(() => {
    if (!open || instanceId === null) {
      return;
    }

    let active = true;
    setLoading(true);
    setPendingAction({
      key: "sync-modal.load",
      label: "Loading shared storage tree",
      detail: "Reading the application catalog from Shared Storage.",
      mode: "indeterminate",
      progress: null,
      startedAt: Date.now()
    });
    setLoadError(null);
    setSelectedPaths([]);

    const timeoutId = window.setTimeout(() => {
      if (active) {
        setLoadError("Loading is taking too long. Please retry.");
        setLoading(false);
      }
    }, 600000);

    void onLoadObjects(instanceId)
      .then((result) => {
        if (!active) {
          return;
        }
        setEntries(result ?? []);
        if (!result) {
          setLoadError("Unable to retrieve files from shared storage.");
        }
      })
      .finally(() => {
        if (active) {
          window.clearTimeout(timeoutId);
          setLoading(false);
          setPendingAction(null);
        }
      });

    return () => {
      active = false;
      window.clearTimeout(timeoutId);
    };
  }, [open, instanceId, onLoadObjects]);

  const childrenMap = useMemo(() => buildChildrenMap(entries), [entries]);

  if (!open) {
    return null;
  }

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
        <div
          className="flex items-center gap-2 rounded border border-[#30365d] bg-[#11162a] px-2 py-1"
          style={{ marginLeft: `${depth * 12}px` }}
        >
          {entry.isDir ? (
            <button
              type="button"
              className="w-5 text-left text-[#7ab6ff]"
              onClick={() => toggleExpanded(entry.path)}
            >
              {isExpanded ? "▾" : "▸"}
            </button>
          ) : (
            <span className="w-5 text-[#7ab6ff]">•</span>
          )}

          {entry.isDir ? (
            <span className="h-4 w-4" aria-hidden="true" />
          ) : (
            <input
              type="checkbox"
              checked={isChecked}
              onChange={() => toggleSelected(entry.path)}
              className="h-4 w-4"
              aria-label={`Select ${entry.name}`}
            />
          )}

          <span className="truncate text-[1.15rem] text-[#d7e8ff]">
            {entry.name}
            {entry.isDir ? "/" : ""}
          </span>
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
            <h2 className="font-display text-base text-white">Sync From Shared Storage</h2>
            <p className="text-[1.2rem] text-[#b4c8de]">Expand an application and choose a specific backup bundle to restore.</p>
          </div>
          <Button variant="ghost" onClick={onClose} disabled={busy || loading}>
            Close
          </Button>
        </div>

        <ModalBody className="px-5 py-4">
          {loading ? (
            pendingAction ? <BlockingLoaderOverlay action={pendingAction} inline className="max-w-none p-4" /> : <p className="text-[1.2rem] text-[#b4c8de]">Loading remote index...</p>
          ) : loadError ? (
            <div className="space-y-2">
              <p className="text-[1.2rem] text-red-300">{loadError}</p>
              <p className="text-[1.05rem] text-[#b4c8de]">Close and reopen Sync to retry.</p>
            </div>
          ) : roots.length === 0 ? (
            <p className="text-[1.2rem] text-[#b4c8de]">No files found in shared storage.</p>
          ) : (
            <div className="space-y-1">{roots.map((entry) => renderNode(entry, 0))}</div>
          )}
        </ModalBody>

        <div className="shrink-0 flex items-center justify-between border-t-2 border-[#3e4270] px-5 py-4">
          <p className="text-[1.1rem] text-[#9ec0e4]">Selected bundles: {selectedPaths.length}</p>
          <div className="flex items-center gap-2">
            <Button variant="ghost" onClick={onClose} disabled={busy || loading}>
              Cancel
            </Button>
            <Button
              disabled={busy || loading || selectedPaths.length === 0}
              loading={busy}
              loadingText="Syncing..."
              onClick={async () => {
                setPendingAction({
                  key: "sync-modal.run",
                  label: "Syncing selected files",
                  detail: "Copying your selected files from shared storage to the instance.",
                  mode: "indeterminate",
                  progress: null,
                  startedAt: Date.now()
                });
                await onConfirmSync(selectedPaths);
                setPendingAction(null);
              }}
            >
              Sync Selected
            </Button>
          </div>
        </div>
    </ModalFrame>
  );
}
