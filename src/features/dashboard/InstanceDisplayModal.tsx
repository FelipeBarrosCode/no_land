import { useEffect, useMemo, useState } from "react";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import {
  applyInstanceDisplayMode,
  getInstanceDisplayStatus,
} from "../../lib/backend";
import type {
  DisplayModeSpec,
  InstanceDisplayStatus,
  RentedInstanceSummary,
} from "../../lib/types";

interface Props {
  instance: RentedInstanceSummary;
  onClose: () => void;
}

function modeKey(mode: DisplayModeSpec) {
  return `${mode.width}x${mode.height}@${mode.refreshMillihz}`;
}

function modeLabel(mode: DisplayModeSpec) {
  const refresh = mode.refreshMillihz / 1000;
  return `${mode.width} × ${mode.height} @ ${Number.isInteger(refresh) ? refresh.toFixed(0) : refresh.toFixed(2)} Hz`;
}

function errorMessage(error: unknown) {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "The remote display operation failed.";
}

export function InstanceDisplayModal({ instance, onClose }: Props) {
  const [status, setStatus] = useState<InstanceDisplayStatus | null>(null);
  const [selectedKey, setSelectedKey] = useState("");
  const [loading, setLoading] = useState(true);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [resultMessage, setResultMessage] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getInstanceDisplayStatus(instance.instanceId)
      .then((nextStatus) => {
        if (cancelled) return;
        setStatus(nextStatus);
        const initial =
          [nextStatus.selectedMode, nextStatus.activeMode].find(
            (candidate): candidate is DisplayModeSpec =>
              candidate !== null &&
              nextStatus.desiredProfile.advertisedModes.some(
                (mode) => modeKey(mode) === modeKey(candidate),
              ),
          ) ?? nextStatus.desiredProfile.preferredMode;
        setSelectedKey(modeKey(initial));
        setError(null);
      })
      .catch((nextError) => {
        if (!cancelled) setError(errorMessage(nextError));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [instance.instanceId]);

  const selectedMode = useMemo(
    () =>
      status?.desiredProfile.advertisedModes.find(
        (mode) => modeKey(mode) === selectedKey,
      ) ?? null,
    [selectedKey, status],
  );

  async function applyMode() {
    if (!selectedMode) return;
    setApplying(true);
    setError(null);
    setResultMessage(null);
    try {
      const result = await applyInstanceDisplayMode(
        instance.instanceId,
        selectedMode,
      );
      setStatus(result.status);
      setResultMessage(
        result.xorgRestarted
          ? "The EDID profile changed, so Xorg and Sunshine were restarted and verified."
          : "The resolution was switched and Sunshine was verified.",
      );
    } catch (nextError) {
      setError(errorMessage(nextError));
    } finally {
      setApplying(false);
    }
  }

  return (
    <ModalFrame
      labelledBy="instance-display-title"
      panelClassName="pixel-frame max-w-2xl bg-[#090d20] text-white"
    >
      <div className="flex items-start justify-between gap-4 border-b border-[#283252] p-5">
        <div>
          <p className="font-display text-[10px] uppercase tracking-[0.16em] text-neon-cyan">
            Remote display
          </p>
          <h2 id="instance-display-title" className="mt-1 text-xl font-semibold">
            {instance.label}
          </h2>
        </div>
        <Button variant="ghost" disabled={applying} onClick={onClose}>
          Close
        </Button>
      </div>

      <ModalBody className="space-y-5 p-5">
        {loading ? <p className="text-[#a8bed6]">Reading remote display state…</p> : null}

        {status ? (
          <>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="rounded border border-[#283252] bg-[#0d132b] p-3">
                <p className="text-xs uppercase tracking-wider text-[#7890ae]">Client profile</p>
                <p className="mt-1 font-medium">
                  {modeLabel(status.desiredProfile.preferredMode)}
                </p>
                <p className="mt-1 text-sm text-[#a8bed6]">
                  {status.desiredProfile.sourceLabel}
                </p>
              </div>
              <div className="rounded border border-[#283252] bg-[#0d132b] p-3">
                <p className="text-xs uppercase tracking-wider text-[#7890ae]">Remote state</p>
                <p className="mt-1 font-medium">
                  {status.activeMode ? modeLabel(status.activeMode) : "No active mode detected"}
                </p>
                <p className="mt-1 text-sm text-[#a8bed6]">
                  Output {status.outputName ?? "unknown"} · Xorg {status.xorgActive ? "ready" : "offline"} · Sunshine {status.sunshineActive ? "ready" : "offline"}
                </p>
              </div>
            </div>

            {status.profileUpdateRequired ? (
              <div className="rounded border border-amber-400/40 bg-amber-400/10 p-3 text-sm text-amber-100">
                The VM has a different EDID profile. Applying a resolution will install the current client-native profile and briefly restart Xorg and Sunshine.
              </div>
            ) : (
              <div className="rounded border border-emerald-400/30 bg-emerald-400/10 p-3 text-sm text-emerald-100">
                The VM already has the current multi-resolution EDID. Applying another listed mode uses the fast switch path.
              </div>
            )}

            <label className="block">
              <span className="mb-2 block text-sm font-medium text-[#d7e6f7]">
                Resolution advertised by this EDID
              </span>
              <select
                className="w-full rounded border border-[#354269] bg-[#080d1f] px-3 py-3 text-white outline-none focus:border-neon-cyan"
                value={selectedKey}
                disabled={applying}
                onChange={(event) => setSelectedKey(event.target.value)}
              >
                {status.desiredProfile.advertisedModes.map((mode) => (
                  <option key={modeKey(mode)} value={modeKey(mode)}>
                    {modeLabel(mode)}
                    {modeKey(mode) === modeKey(status.desiredProfile.preferredMode)
                      ? " — client native"
                      : ""}
                  </option>
                ))}
              </select>
            </label>

            <p className="text-sm text-[#91a9c4]">
              Applying a mode interrupts an active stream. The selection is saved on the VM and restored after reboot before Sunshine starts.
            </p>

            <Button
              className="w-full"
              disabled={!selectedMode || applying}
              loading={applying}
              loadingText="Applying and verifying…"
              onClick={applyMode}
            >
              Apply Resolution
            </Button>
          </>
        ) : null}

        {resultMessage ? (
          <div className="rounded border border-neon-lime/30 bg-neon-lime/10 p-3 text-sm text-neon-lime">
            {resultMessage}
          </div>
        ) : null}
        {error ? (
          <div className="rounded border border-red-400/40 bg-red-500/10 p-3 text-sm text-red-200">
            {error}
          </div>
        ) : null}
      </ModalBody>
    </ModalFrame>
  );
}
