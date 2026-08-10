import { useState, useEffect } from "react";
import { BlockingLoaderOverlay, type BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import type {
  AppBundle,
  BundleIndex,
  RestoreDryRunResult,
  RestoreJob
} from "../../lib/types";

interface Props {
  bundleIndex: BundleIndex | null;
  restoreJob: RestoreJob | null;
  instanceId: number;
  busy: boolean;
  instanceActionRunning: boolean;
  onLoadBundles: (instanceId: number) => Promise<void>;
  onGenerateIndex: () => Promise<void>;
  onDryRun: (instanceId: number, bundleId: string, folderIds: string[], mode: string) => Promise<RestoreDryRunResult | null>;
  onRestore: (instanceId: number, bundleId: string, folderIds: string[], mode: string) => Promise<RestoreJob | null>;
  onPollJob: (jobId: string) => Promise<void>;
  onClose: () => void;
}

export function RestoreBundlesPanel({
  bundleIndex,
  restoreJob,
  instanceId,
  busy,
  instanceActionRunning,
  onLoadBundles,
  onGenerateIndex,
  onDryRun,
  onRestore,
  onPollJob,
  onClose
}: Props) {
  const [search, setSearch] = useState("");
  const [expandedBundleId, setExpandedBundleId] = useState<string | null>(null);
  const [selectedFolders, setSelectedFolders] = useState<Record<string, Set<string>>>({});
  const [dryRunResult, setDryRunResult] = useState<RestoreDryRunResult | null>(null);
  const [activeJobId, setActiveJobId] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<BlockingActionState | null>(null);

  useEffect(() => {
    void (async () => {
      setPendingAction({
        key: "restore.index.load",
        label: "Loading restore bundles",
        detail: "Fetching indexed backup bundles for this instance.",
        mode: "indeterminate",
        progress: null,
        startedAt: Date.now()
      });
      await onLoadBundles(instanceId);
      setPendingAction(null);
    })();
  }, [instanceId]);

  // Poll active job
  useEffect(() => {
    if (!activeJobId) return;
    const interval = setInterval(() => {
      void onPollJob(activeJobId);
    }, 2000);
    return () => clearInterval(interval);
  }, [activeJobId]);

  // Clear dry run when bundle selection changes
  useEffect(() => {
    setDryRunResult(null);
  }, [selectedFolders]);

  const filteredBundles = bundleIndex?.bundles.filter((b) => {
    const term = search.toLowerCase();
    return (
      b.name.toLowerCase().includes(term) ||
      b.signals.some((s) => s.toLowerCase().includes(term))
    );
  }) ?? [];

  const highConfidence = filteredBundles.filter((b) => b.confidence >= 0.5);
  const lowConfidence = filteredBundles.filter((b) => b.confidence < 0.5);

  function toggleFolder(bundleId: string, folderId: string) {
    setSelectedFolders((prev) => {
      const next = { ...prev };
      const set = new Set(next[bundleId] ?? []);
      if (set.has(folderId)) {
        set.delete(folderId);
      } else {
        set.add(folderId);
      }
      next[bundleId] = set;
      return next;
    });
  }

  function selectDefaultFolders(bundle: AppBundle) {
    setSelectedFolders((prev) => {
      const next = { ...prev };
      next[bundle.id] = new Set(
        bundle.folderBundles.filter((f) => f.defaultSelected).map((f) => f.id)
      );
      return next;
    });
  }

  function getSelectedFolderIds(bundleId: string): string[] {
    return Array.from(selectedFolders[bundleId] ?? []);
  }

  async function handleDryRun(bundle: AppBundle) {
    const folderIds = getSelectedFolderIds(bundle.id);
    if (folderIds.length === 0) return;
    setPendingAction({
      key: "restore.dry_run",
      label: "Running restore dry run",
      detail: "Previewing the changes before any files are restored.",
      mode: "indeterminate",
      progress: null,
      startedAt: Date.now()
    });
    const result = await onDryRun(instanceId, bundle.id, folderIds, "merge");
    if (result) {
      setDryRunResult(result);
    }
    setPendingAction(null);
  }

  async function handleRestore(bundle: AppBundle, mode: string) {
    const folderIds = getSelectedFolderIds(bundle.id);
    if (folderIds.length === 0) return;
    setPendingAction({
      key: "restore.run",
      label: "Restoring backup bundle",
      detail: "Copying the selected backup data back onto the instance.",
      mode: "indeterminate",
      progress: null,
      startedAt: Date.now()
    });
    const job = await onRestore(instanceId, bundle.id, folderIds, mode);
    if (job) {
      setActiveJobId(job.jobId);
      setDryRunResult(null);
    }
    setPendingAction(null);
  }

  const actionDisabled = busy || instanceActionRunning;

  return (
    <ModalFrame
      panelClassName="glass-panel pixel-frame max-w-3xl"
      overlayClassName="bg-black/70 backdrop-blur-sm"
    >
      <ModalBody className="p-6">
        {pendingAction && <BlockingLoaderOverlay action={pendingAction} inline className="mb-4 max-w-none p-4" />}
        <div className="flex items-center justify-between mb-4">
          <div>
            <h3 className="text-lg font-display text-neon-cyan">Backup & Restore</h3>
            <p className="text-xs text-gray-400 mt-1">
              {bundleIndex
                ? `Index generated at ${new Date(bundleIndex.generatedAt).toLocaleString()}`
                : "No restore index available yet"}
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="secondary"
              onClick={async () => {
                setPendingAction({
                  key: "restore.index.generate",
                  label: "Generating restore index",
                  detail: "Scanning backup metadata so bundles can be restored.",
                  mode: "indeterminate",
                  progress: null,
                  startedAt: Date.now()
                });
                await onGenerateIndex();
                setPendingAction(null);
              }}
              disabled={actionDisabled}
              className="text-xs"
              loading={busy}
              loadingText="Generating..."
            >
              Regenerate Index
            </Button>
            <Button variant="ghost" onClick={onClose} disabled={actionDisabled}>
              Close
            </Button>
          </div>
        </div>

        {bundleIndex === null && !instanceActionRunning && (
          <div className="text-center py-8">
            <p className="text-gray-400 mb-4">No restore index available yet.</p>
            <Button onClick={onGenerateIndex} disabled={actionDisabled}>
              Generate Bundle Index
            </Button>
          </div>
        )}

        {bundleIndex !== null && (
          <>
            <input
              type="text"
              placeholder="Search bundles..."
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              className="w-full mb-4 bg-[#0b0f23] border border-[#3f476c] px-3 py-2 text-sm text-[#dff8ff] rounded placeholder:text-[#5e7396] focus:border-neon-cyan outline-none"
            />

            {highConfidence.length === 0 && lowConfidence.length === 0 && (
              <p className="text-gray-400 text-center py-4">No bundles match your search.</p>
            )}

            <div className="space-y-3">
              {highConfidence.map((bundle) => (
                <BundleCard
                  key={bundle.id}
                  bundle={bundle}
                  expanded={expandedBundleId === bundle.id}
                  selectedFolders={selectedFolders[bundle.id] ?? new Set()}
                  actionDisabled={actionDisabled}
                  dryRunResult={expandedBundleId === bundle.id ? dryRunResult : null}
                  activeJob={restoreJob?.jobId === activeJobId ? restoreJob : null}
                  onToggleExpand={() => {
                    const next = expandedBundleId === bundle.id ? null : bundle.id;
                    setExpandedBundleId(next);
                    setDryRunResult(null);
                    if (next) {
                      selectDefaultFolders(bundle);
                    }
                  }}
                  onToggleFolder={(folderId) =>
                    toggleFolder(bundle.id, folderId)
                  }
                  onDryRun={() => handleDryRun(bundle)}
                  onRestore={(mode) => handleRestore(bundle, mode)}
                />
              ))}

              {lowConfidence.length > 0 && (
                <div className="mt-4">
                  <p className="text-xs text-gray-500 uppercase tracking-wider mb-2">Other detected folders</p>
                  {lowConfidence.map((bundle) => (
                    <BundleCard
                      key={bundle.id}
                      bundle={bundle}
                      expanded={expandedBundleId === bundle.id}
                      selectedFolders={selectedFolders[bundle.id] ?? new Set()}
                      actionDisabled={actionDisabled}
                      dryRunResult={expandedBundleId === bundle.id ? dryRunResult : null}
                      activeJob={restoreJob?.jobId === activeJobId ? restoreJob : null}
                      onToggleExpand={() => {
                        const next = expandedBundleId === bundle.id ? null : bundle.id;
                        setExpandedBundleId(next);
                        setDryRunResult(null);
                        if (next) {
                          selectDefaultFolders(bundle);
                        }
                      }}
                      onToggleFolder={(folderId) =>
                        toggleFolder(bundle.id, folderId)
                      }
                      onDryRun={() => handleDryRun(bundle)}
                      onRestore={(mode) => handleRestore(bundle, mode)}
                    />
                  ))}
                </div>
              )}
            </div>
          </>
        )}
      </ModalBody>
    </ModalFrame>
  );
}

interface BundleCardProps {
  bundle: AppBundle;
  expanded: boolean;
  selectedFolders: Set<string>;
  actionDisabled: boolean;
  dryRunResult: RestoreDryRunResult | null;
  activeJob: RestoreJob | null;
  onToggleExpand: () => void;
  onToggleFolder: (folderId: string) => void;
  onDryRun: () => void;
  onRestore: (mode: string) => void;
}

function BundleCard({
  bundle,
  expanded,
  selectedFolders,
  actionDisabled,
  dryRunResult,
  activeJob,
  onToggleExpand,
  onToggleFolder,
  onDryRun,
  onRestore
}: BundleCardProps) {
  const selectedCount = selectedFolders.size;
  const typeColors: Record<string, string> = {
    app: "text-neon-cyan",
    project: "text-neon-lime",
    download: "text-yellow-400",
  };

  return (
    <div className="border border-[#3f476c] rounded bg-[#0b0f23]/60">
      <button
        className="w-full flex items-center justify-between p-3 text-left hover:bg-[#121731]/40 transition"
        onClick={onToggleExpand}
      >
        <div className="flex items-center gap-3">
          <span className={`text-xs font-display uppercase ${typeColors[bundle.type] ?? "text-gray-400"}`}>
            {bundle.type}
          </span>
          <span className="text-sm text-white font-medium">{bundle.name}</span>
          <span className="text-xs text-gray-500">
            {Math.round(bundle.confidence * 100)}% confidence
          </span>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-gray-500">
            {bundle.folderBundles.length} folders
          </span>
          <span className="text-xs text-gray-400">{expanded ? "▼" : "▶"}</span>
        </div>
      </button>

      {expanded && (
        <div className="p-3 pt-0 border-t border-[#3f476c]/50">
          <div className="mt-2 flex flex-wrap gap-1 mb-3">
            {bundle.signals.map((s) => (
              <span
                key={s}
                className="text-[10px] px-2 py-0.5 rounded bg-[#1a1f3a] text-gray-400 border border-[#2a3050]"
              >
                {s}
              </span>
            ))}
          </div>

          <div className="space-y-2">
            {bundle.folderBundles.map((folder) => (
              <label
                key={folder.id}
                className="flex items-center gap-3 p-2 rounded hover:bg-[#121731]/40 cursor-pointer"
              >
                <input
                  type="checkbox"
                  checked={selectedFolders.has(folder.id)}
                  onChange={() => onToggleFolder(folder.id)}
                  className="w-4 h-4 accent-neon-cyan"
                />
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-gray-200">{folder.label}</span>
                    <span className="text-[10px] text-gray-500 uppercase">{folder.kind}</span>
                  </div>
                  <p className="text-xs text-gray-500 font-mono truncate">{folder.target}</p>
                </div>
              </label>
            ))}
          </div>

          {selectedCount > 0 && (
            <div className="mt-3 flex flex-wrap gap-2">
              <Button
                variant="secondary"
                className="text-xs"
                disabled={actionDisabled}
                onClick={onDryRun}
              >
                Dry Run
              </Button>
              <Button
                variant="primary"
                className="text-xs"
                disabled={actionDisabled}
                onClick={() => onRestore("restore_to_staging")}
              >
                Restore to Staging
              </Button>
              <Button
                variant="ghost"
                className="text-xs text-red-400 hover:text-red-300"
                disabled={actionDisabled}
                onClick={() => {
                  if (confirm("This will overwrite files at their original locations. Continue?")) {
                    onRestore("merge");
                  }
                }}
              >
                Restore to Original
              </Button>
            </div>
          )}

          {dryRunResult && (
            <div className="mt-3 p-3 bg-[#1a1f3a] rounded border border-[#2a3050]">
              <p className="text-xs text-neon-cyan mb-2">
                Dry run: {dryRunResult.wouldRestore.length} items, ~{dryRunResult.totalFilesEstimate} files
              </p>
              <div className="space-y-1 max-h-32 overflow-y-auto">
                {dryRunResult.wouldRestore.map((item) => (
                  <div key={item.folderBundleId} className="text-xs text-gray-400 flex gap-2">
                    <span className="text-gray-300">{item.label}</span>
                    <span className="text-gray-600">→</span>
                    <span className="font-mono truncate">{item.target}</span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {activeJob && activeJob.bundleId === bundle.id && (
            <div className="mt-3 p-3 bg-[#1a1f3a] rounded border border-[#2a3050]">
              <p className="text-xs text-gray-300 mb-2">
                Job {activeJob.status}: {activeJob.items.filter((i) => i.status === "completed").length}/{activeJob.items.length} completed
              </p>
              <div className="space-y-1">
                {activeJob.items.map((item) => (
                  <div key={item.folderBundleId} className="flex items-center gap-2 text-xs">
                    <span
                      className={
                        item.status === "completed"
                          ? "text-green-400"
                          : item.status === "failed"
                          ? "text-red-400"
                          : item.status === "running"
                          ? "text-yellow-400"
                          : "text-gray-500"
                      }
                    >
                      {item.status === "completed" ? "✓" : item.status === "failed" ? "✗" : item.status === "running" ? "⟳" : "○"}
                    </span>
                    <span className="text-gray-400">{item.label}</span>
                    {item.error && (
                      <span className="text-red-400 text-[10px]">{item.error}</span>
                    )}
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
