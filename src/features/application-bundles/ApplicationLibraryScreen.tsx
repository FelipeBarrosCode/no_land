import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import type { ApplicationBundle, BundleSnapshot } from "../../lib/types";

interface Props {
  bundles: ApplicationBundle[];
  selectedIds: string[];
  busy: boolean;
  onToggleSelect: (bundleId: string) => void;
  onRestore: (bundleId: string, mode: string, components?: string[]) => Promise<void>;
  onRemoveLocal: (bundleId: string) => Promise<void>;
  onRequestCloudDeletion: (bundleId: string) => Promise<void>;
  onLoadSnapshots: (bundleId: string) => Promise<void>;
  snapshots: BundleSnapshot[];
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[i]}`;
}

function availabilityLabel(ab: string): { text: string; color: string } {
  switch (ab) {
    case "installed": return { text: "Installed", color: "bg-green-900/50 text-green-300" };
    case "cloud-only": return { text: "Cloud Only", color: "bg-blue-900/50 text-blue-300" };
    case "restoring": return { text: "Restoring", color: "bg-yellow-900/50 text-yellow-300" };
    case "backup-pending": return { text: "Backup Pending", color: "bg-orange-900/50 text-orange-300" };
    default: return { text: ab, color: "bg-gray-700 text-gray-300" };
  }
}

function backupStatusLabel(bs: string): string {
  switch (bs) {
    case "committed": return "Cloud protected";
    case "uploading": return "Uploading...";
    case "no-backup": return "Not backed up";
    case "verification-failed": return "Verification failed";
    default: return bs;
  }
}

export function ApplicationLibraryScreen({
  bundles,
  selectedIds,
  busy,
  onToggleSelect,
  onRestore,
  onRemoveLocal,
  onRequestCloudDeletion,
  onLoadSnapshots,
}: Props) {
  const navigate = useNavigate();
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [showRestoreModal, setShowRestoreModal] = useState<string | null>(null);
  const [showRemoveConfirmation, setShowRemoveConfirmation] = useState<string | null>(null);
  const [showDeleteCloudConfirmation, setShowDeleteCloudConfirmation] = useState<string | null>(null);
  const [restoreMode, setRestoreMode] = useState<string>("complete");
  const [selectedComponents, setSelectedComponents] = useState<string[]>([]);

  const installed = bundles.filter((b) => b.availability === "installed");
  const cloudOnly = bundles.filter((b) => b.availability === "cloud-only");
  const other = bundles.filter((b) => b.availability !== "installed" && b.availability !== "cloud-only");

  function toggleComponent(componentId: string) {
    setSelectedComponents((prev) =>
      prev.includes(componentId)
        ? prev.filter((id) => id !== componentId)
        : [...prev, componentId],
    );
  }

  async function handleRestore(bundleId: string) {
    if (restoreMode === "custom") {
      await onRestore(bundleId, restoreMode, selectedComponents);
    } else {
      await onRestore(bundleId, restoreMode);
    }
    setShowRestoreModal(null);
    setSelectedComponents([]);
    setRestoreMode("complete");
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h2 className="font-display text-sm uppercase tracking-wider text-neon-cyan">
            Application Library
          </h2>
          <p className="text-xs text-gray-400 mt-1">
            {bundles.length} application{bundles.length !== 1 ? "s" : ""} ·{" "}
            {bundles.filter((b) => b.backupStatus === "committed").length} protected
          </p>
        </div>
        <Button variant="ghost" onClick={() => navigate("/")}>
          Back to Dashboard
        </Button>
      </div>

      {/* Installed section */}
      {installed.length > 0 && (
        <div>
          <h3 className="text-xs font-display uppercase tracking-wider text-gray-500 mb-2">
            Installed
          </h3>
          <div className="space-y-2">
            {installed.map((bundle) => (
              <BundleRow
                key={bundle.id}
                bundle={bundle}
                selected={selectedIds.includes(bundle.id)}
                expanded={expandedId === bundle.id}
                busy={busy}
                onToggleSelect={() => onToggleSelect(bundle.id)}
                onToggleExpand={() => {
                  setExpandedId(expandedId === bundle.id ? null : bundle.id);
                  if (expandedId !== bundle.id) void onLoadSnapshots(bundle.id);
                }}
                onRestore={() => setShowRestoreModal(bundle.id)}
                onRemoveLocal={() => setShowRemoveConfirmation(bundle.id)}
                onRequestCloudDeletion={() => setShowDeleteCloudConfirmation(bundle.id)}
              />
            ))}
          </div>
        </div>
      )}

      {/* Cloud-only section */}
      {cloudOnly.length > 0 && (
        <div>
          <h3 className="text-xs font-display uppercase tracking-wider text-gray-500 mb-2">
            Available in Cloud
          </h3>
          <div className="space-y-2">
            {cloudOnly.map((bundle) => (
              <BundleRow
                key={bundle.id}
                bundle={bundle}
                selected={selectedIds.includes(bundle.id)}
                expanded={expandedId === bundle.id}
                busy={busy}
                onToggleSelect={() => onToggleSelect(bundle.id)}
                onToggleExpand={() => setExpandedId(expandedId === bundle.id ? null : bundle.id)}
                onRestore={() => setShowRestoreModal(bundle.id)}
                onRemoveLocal={() => {}}
                onRequestCloudDeletion={() => setShowDeleteCloudConfirmation(bundle.id)}
              />
            ))}
          </div>
        </div>
      )}

      {/* Other */}
      {other.length > 0 && (
        <div>
          <h3 className="text-xs font-display uppercase tracking-wider text-gray-500 mb-2">
            Other
          </h3>
          <div className="space-y-2">
            {other.map((bundle) => (
              <BundleRow
                key={bundle.id}
                bundle={bundle}
                selected={selectedIds.includes(bundle.id)}
                expanded={expandedId === bundle.id}
                busy={busy}
                onToggleSelect={() => onToggleSelect(bundle.id)}
                onToggleExpand={() => setExpandedId(expandedId === bundle.id ? null : bundle.id)}
                onRestore={() => setShowRestoreModal(bundle.id)}
                onRemoveLocal={() => setShowRemoveConfirmation(bundle.id)}
                onRequestCloudDeletion={() => setShowDeleteCloudConfirmation(bundle.id)}
              />
            ))}
          </div>
        </div>
      )}

      {bundles.length === 0 && (
        <Card className="p-6 text-center">
          <p className="text-gray-400">No applications found.</p>
          <p className="text-xs text-gray-500 mt-2">
            Launch an application on your instance to create its bundle.
          </p>
        </Card>
      )}

      {/* Restore Modal */}
      {showRestoreModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm p-4">
          <Card className="w-full max-w-lg p-6">
            <h3 className="text-lg font-display text-neon-cyan mb-4">Restore Application</h3>
            <p className="text-sm text-gray-400 mb-4">
              Choose how to restore this application.
            </p>

            <div className="space-y-3 mb-6">
              <label className="flex items-center gap-3 p-3 border border-[#3f476c] rounded cursor-pointer hover:border-neon-cyan">
                <input
                  type="radio"
                  name="restoreMode"
                  value="complete"
                  checked={restoreMode === "complete"}
                  onChange={() => setRestoreMode("complete")}
                  className="accent-neon-cyan"
                />
                <div>
                  <p className="text-sm text-gray-200">Complete Restore</p>
                  <p className="text-xs text-gray-500">Everything: game, saves, settings, mods, runtime</p>
                </div>
              </label>
              <label className="flex items-center gap-3 p-3 border border-[#3f476c] rounded cursor-pointer hover:border-neon-cyan">
                <input
                  type="radio"
                  name="restoreMode"
                  value="personal_state"
                  checked={restoreMode === "personal_state"}
                  onChange={() => setRestoreMode("personal_state")}
                  className="accent-neon-cyan"
                />
                <div>
                  <p className="text-sm text-gray-200">Personal Data Only</p>
                  <p className="text-xs text-gray-500">Saves, settings, mods, profiles. Reinstall app from store.</p>
                </div>
              </label>
              <label className="flex items-center gap-3 p-3 border border-[#3f476c] rounded cursor-pointer hover:border-neon-cyan">
                <input
                  type="radio"
                  name="restoreMode"
                  value="custom"
                  checked={restoreMode === "custom"}
                  onChange={() => setRestoreMode("custom")}
                  className="accent-neon-cyan"
                />
                <div>
                  <p className="text-sm text-gray-200">Custom Selection</p>
                  <p className="text-xs text-gray-500">Choose specific components</p>
                </div>
              </label>
            </div>

            {restoreMode === "custom" && (
              <div className="mb-4 space-y-2">
                {bundles
                  .find((b) => b.id === showRestoreModal)
                  ?.components.map((comp) => (
                    <label key={comp.id} className="flex items-center gap-2 text-sm text-gray-300">
                      <input
                        type="checkbox"
                        checked={selectedComponents.includes(comp.id)}
                        onChange={() => toggleComponent(comp.id)}
                        className="accent-neon-cyan"
                      />
                      <span>{comp.displayName}</span>
                      <span className="text-gray-500 ml-auto">{formatBytes(comp.logicalSize)}</span>
                    </label>
                  ))}
              </div>
            )}

            <div className="flex gap-2 justify-end">
              <Button variant="ghost" onClick={() => { setShowRestoreModal(null); setSelectedComponents([]); setRestoreMode("complete"); }}>
                Cancel
              </Button>
              <Button
                variant="primary"
                onClick={() => handleRestore(showRestoreModal)}
                disabled={busy || (restoreMode === "custom" && selectedComponents.length === 0)}
                loading={busy}
                loadingText="Restoring..."
              >
                Restore
              </Button>
            </div>
          </Card>
        </div>
      )}

      {/* Remove local confirmation */}
      {showRemoveConfirmation && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm p-4">
          <Card className="w-full max-w-md p-6">
            <h3 className="text-lg font-display text-yellow-400 mb-4">Remove Local Copy</h3>
            <p className="text-sm text-gray-300 mb-4">
              The cloud backup will remain available in Shared Storage.
            </p>
            <div className="flex gap-2 justify-end">
              <Button variant="ghost" onClick={() => setShowRemoveConfirmation(null)}>
                Cancel
              </Button>
              <Button
                variant="secondary"
                className="text-red-400"
                onClick={() => {
                  void onRemoveLocal(showRemoveConfirmation);
                  setShowRemoveConfirmation(null);
                }}
              >
                Remove Local Copy
              </Button>
            </div>
          </Card>
        </div>
      )}

      {/* Delete cloud confirmation */}
      {showDeleteCloudConfirmation && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm p-4">
          <Card className="w-full max-w-md p-6 border-red-500/30">
            <h3 className="text-lg font-display text-red-400 mb-4">Delete Cloud Backup?</h3>
            <p className="text-sm text-gray-300 mb-4">
              This will permanently delete the cloud backup. This action cannot be undone.
            </p>
            <div className="flex gap-2 justify-end">
              <Button variant="ghost" onClick={() => setShowDeleteCloudConfirmation(null)}>
                Cancel
              </Button>
              <Button
                variant="secondary"
                className="text-red-400 border-red-500/50"
                onClick={() => {
                  void onRequestCloudDeletion(showDeleteCloudConfirmation);
                  setShowDeleteCloudConfirmation(null);
                }}
              >
                Delete Cloud Backup
              </Button>
            </div>
          </Card>
        </div>
      )}
    </div>
  );
}

// ─── Bundle Row Component ─────────────────────────────────

interface BundleRowProps {
  bundle: ApplicationBundle;
  selected: boolean;
  expanded: boolean;
  busy: boolean;
  onToggleSelect: () => void;
  onToggleExpand: () => void;
  onRestore: () => void;
  onRemoveLocal: () => void;
  onRequestCloudDeletion: () => void;
}

function BundleRow({
  bundle,
  selected,
  expanded,
  busy,
  onToggleSelect,
  onToggleExpand,
  onRestore,
  onRemoveLocal,
  onRequestCloudDeletion,
}: BundleRowProps) {
  const avail = availabilityLabel(bundle.availability);
  const backup = backupStatusLabel(bundle.backupStatus);
  const totalSize = bundle.components.reduce((sum, c) => sum + c.logicalSize, 0);

  return (
    <div className="border border-[#3f476c] rounded bg-[#0b0f23]/60">
      <div className="flex items-center p-3 gap-3">
        <input
          type="checkbox"
          checked={selected}
          onChange={onToggleSelect}
          className="w-4 h-4 accent-neon-cyan"
        />
        <button className="flex-1 text-left" onClick={onToggleExpand}>
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-sm text-white font-medium">{bundle.displayName}</span>
            <span className={`text-[10px] px-2 py-0.5 rounded ${avail.color}`}>
              {avail.text}
            </span>
            <span className="text-[10px] text-gray-500">{backup}</span>
          </div>
          <p className="text-xs text-gray-500 mt-1">
            {formatBytes(totalSize)} · {bundle.components.length} component{bundle.components.length !== 1 ? "s" : ""}
          </p>
        </button>
        <div className="flex gap-1">
          {bundle.availability === "cloud-only" && (
            <Button variant="primary" className="text-xs py-1" onClick={onRestore} disabled={busy}>
              Restore
            </Button>
          )}
          {bundle.availability === "installed" && (
            <Button variant="ghost" className="text-xs py-1 text-red-400" onClick={onRemoveLocal} disabled={busy}>
              Remove
            </Button>
          )}
          <span className="text-xs text-gray-400">{expanded ? "▲" : "▼"}</span>
        </div>
      </div>

      {expanded && (
        <div className="px-4 pb-4 border-t border-[#3f476c]/50 pt-3">
          <div className="grid gap-1 mb-3">
            <p className="text-xs text-gray-400">
              ID: <span className="font-mono text-gray-300">{bundle.id}</span>
            </p>
            <p className="text-xs text-gray-400">
              Kind: <span className="text-gray-300">{bundle.kind}</span>
            </p>
            {bundle.storeMetadata && (
              <p className="text-xs text-gray-400">
                Version: <span className="text-gray-300">{bundle.storeMetadata.installedVersion || "unknown"}</span>
              </p>
            )}
            <p className="text-xs text-gray-400">
              Last Backup: <span className="text-gray-300">
                {new Date(bundle.updatedAt * 1000).toLocaleString()}
              </span>
            </p>
          </div>

          <p className="text-xs font-display uppercase tracking-wider text-gray-500 mb-2">
            Components
          </p>
          <div className="space-y-1">
            {bundle.components.map((comp) => (
              <div key={comp.id} className="flex items-center justify-between text-xs">
                <div className="flex items-center gap-2">
                  <span className={comp.required ? "text-neon-cyan" : "text-gray-400"}>
                    {comp.required ? "●" : "○"}
                  </span>
                  <span className="text-gray-300">{comp.displayName}</span>
                </div>
                <span className="text-gray-500">{formatBytes(comp.logicalSize)}</span>
              </div>
            ))}
          </div>

          {bundle.relationships.length > 0 && (
            <>
              <p className="text-xs font-display uppercase tracking-wider text-gray-500 mt-3 mb-2">
                Related
              </p>
              <div className="space-y-1">
                {bundle.relationships.map((rel, idx) => (
                  <p key={idx} className="text-xs text-gray-400">
                    {rel.displayName} · {rel.relationshipKind}
                  </p>
                ))}
              </div>
            </>
          )}

          <div className="flex gap-2 mt-3">
            <Button variant="secondary" className="text-xs" onClick={onRestore} disabled={busy}>
              Restore
            </Button>
            <Button variant="ghost" className="text-xs text-red-400" onClick={onRequestCloudDeletion} disabled={busy}>
              Delete Backup
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
