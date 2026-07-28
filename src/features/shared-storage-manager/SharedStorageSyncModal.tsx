import { useEffect, useState } from "react";
import { BlockingLoaderOverlay, type BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import type { ApplicationBundle } from "../../lib/types";

interface Props {
  open: boolean;
  busy: boolean;
  instanceId: number | null;
  bundles: ApplicationBundle[];
  onClose: () => void;
  onLoadBundles: (instanceId?: number) => Promise<void>;
  onRestoreBundle: (bundleId: string) => Promise<void>;
}

export function SharedStorageSyncModal({
  open,
  busy,
  instanceId,
  bundles,
  onClose,
  onLoadBundles,
  onRestoreBundle
}: Props) {
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedBundleId, setSelectedBundleId] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<BlockingActionState | null>(null);

  useEffect(() => {
    if (!open || instanceId === null) {
      return;
    }

    let active = true;
    setLoading(true);
    setPendingAction({
      key: "sync-modal.load",
      label: "Loading bundles",
      detail: "Fetching the available application bundles from shared storage.",
      mode: "indeterminate",
      progress: null,
      startedAt: Date.now()
    });
    setLoadError(null);
    setSelectedBundleId(null);

    const timeoutId = window.setTimeout(() => {
      if (active) {
        setLoadError("Loading bundles is taking too long. Please retry.");
        setLoading(false);
      }
    }, 25000);

    void onLoadBundles(instanceId)
      .catch((error: unknown) => {
        if (!active) {
          return;
        }
        setLoadError(
          error instanceof Error
            ? error.message
            : "Unable to retrieve bundles from shared storage.",
        );
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
  }, [open, instanceId, onLoadBundles]);

  if (!open) {
    return null;
  }

  const committedBundles = bundles.filter((bundle) => bundle.backupStatus === "committed");
  const selectedBundle = committedBundles.find((bundle) => bundle.id === selectedBundleId) ?? null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-[#02040bdd] p-4">
      <div className="glass-panel pixel-frame max-h-[90vh] w-full max-w-3xl overflow-hidden">
        <div className="flex items-center justify-between border-b-2 border-[#3e4270] px-5 py-4">
          <div>
            <h2 className="font-display text-base text-white">Restore Bundle From Shared Storage</h2>
            <p className="text-[1.2rem] text-[#b4c8de]">Choose a software bundle and restore the full app/game with its related data.</p>
          </div>
          <Button variant="ghost" onClick={onClose} disabled={busy || loading}>
            Close
          </Button>
        </div>

        <div className="max-h-[65vh] overflow-y-auto px-5 py-4">
          {loading ? (
            pendingAction ? <BlockingLoaderOverlay action={pendingAction} inline className="max-w-none p-4" /> : <p className="text-[1.2rem] text-[#b4c8de]">Loading bundles...</p>
          ) : loadError ? (
            <div className="space-y-2">
              <p className="text-[1.2rem] text-red-300">{loadError}</p>
              <p className="text-[1.05rem] text-[#b4c8de]">Close and reopen Sync to retry.</p>
            </div>
          ) : committedBundles.length === 0 ? (
            <p className="text-[1.2rem] text-[#b4c8de]">No bundles found in shared storage.</p>
          ) : (
            <div className="grid gap-4 md:grid-cols-[minmax(0,1.2fr)_minmax(280px,0.8fr)]">
              <div className="space-y-2">
                {committedBundles.map((bundle) => {
                  const isSelected = bundle.id === selectedBundleId;
                  return (
                    <button
                      key={bundle.id}
                      type="button"
                      onClick={() => setSelectedBundleId(bundle.id)}
                      className={`w-full rounded border px-3 py-3 text-left transition ${
                        isSelected
                          ? "border-neon-cyan bg-[#13203f]"
                          : "border-[#30365d] bg-[#11162a] hover:border-[#4e5da3]"
                      }`}
                    >
                      <div className="flex items-start justify-between gap-3">
                        <div>
                          <p className="text-[1.2rem] text-[#d7e8ff]">{bundle.displayName}</p>
                          <p className="mt-1 text-[1rem] text-[#8aa1ba]">
                            {bundle.components.length} related location{bundle.components.length !== 1 ? "s" : ""}
                          </p>
                        </div>
                        <span className="rounded border border-[#3f476c] bg-[#0d1326] px-2 py-1 text-[0.95rem] text-[#d7e8ff]">
                          {bundle.kind}
                        </span>
                      </div>
                    </button>
                  );
                })}
              </div>

              <div className="rounded border border-[#30365d] bg-[#11162a] p-3">
                {selectedBundle ? (
                  <>
                    <p className="font-display text-[10px] uppercase tracking-[0.14em] text-neon-cyan">
                      Bundle details
                    </p>
                    <h3 className="mt-2 text-[1.3rem] text-[#d7e8ff]">{selectedBundle.displayName}</h3>
                    <p className="mt-1 text-[1rem] text-[#8aa1ba]">ID: {selectedBundle.id}</p>
                    <p className="mt-3 text-[1rem] text-[#9ec0e4]">
                      Restoring this bundle will recover the software together with its related detected data paths.
                    </p>
                    <div className="mt-3 space-y-2">
                      {selectedBundle.components.map((component) => (
                        <div
                          key={component.id}
                          className="rounded border border-[#3f476c] bg-[#0d1326] px-2 py-2"
                        >
                          <div className="flex items-center justify-between gap-2">
                            <span className="text-[1rem] text-[#d7e8ff]">{component.displayName}</span>
                            <span className="text-[0.95rem] text-[#8aa1ba]">{component.kind}</span>
                          </div>
                        </div>
                      ))}
                    </div>
                  </>
                ) : (
                  <p className="text-[1.1rem] text-[#9ec0e4]">Select a bundle to inspect and restore it.</p>
                )}
              </div>
            </div>
          )}
        </div>

        <div className="flex items-center justify-between border-t-2 border-[#3e4270] px-5 py-4">
          <p className="text-[1.1rem] text-[#9ec0e4]">
            {selectedBundle ? `Selected bundle: ${selectedBundle.displayName}` : "Select a bundle to restore"}
          </p>
          <div className="flex items-center gap-2">
            <Button variant="ghost" onClick={onClose} disabled={busy || loading}>
              Cancel
            </Button>
            <Button
              disabled={busy || loading || !selectedBundleId}
              loading={busy}
              loadingText="Restoring..."
              onClick={async () => {
                if (!selectedBundleId) {
                  return;
                }
                setPendingAction({
                  key: "sync-modal.run",
                  label: "Restoring bundle",
                  detail: "Restoring the selected software bundle to the instance.",
                  mode: "indeterminate",
                  progress: null,
                  startedAt: Date.now()
                });
                await onRestoreBundle(selectedBundleId);
                setPendingAction(null);
                onClose();
              }}
            >
              Restore Bundle
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
