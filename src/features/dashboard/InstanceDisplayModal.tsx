import { useEffect, useMemo, useState } from "react";
import { Button } from "../../components/ui/Button";
import { useLocalization, translate } from "../../lib/i18n";
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
  return translate("generated.fc1e004dd2927102");
}

export function InstanceDisplayModal({ instance, onClose }: Props) {
  const { t } = useLocalization();
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
            {translate("generated.ed04911b08d667fc")}
          </p>
          <h2 id="instance-display-title" className="mt-1 text-xl font-semibold">
            {instance.label}
          </h2>
        </div>
        <Button variant="ghost" disabled={applying} onClick={onClose}>
          {translate("generated.7d9eb7acb13e2462")}
        </Button>
      </div>

      <ModalBody className="space-y-5 p-5">
        {loading ? <p className="text-[#a8bed6]">{t("display.reading")}</p> : null}

        {status ? (
          <>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="rounded border border-[#283252] bg-[#0d132b] p-3">
                <p className="text-xs uppercase tracking-wider text-[#7890ae]">{t("display.client.profile")}</p>
                <p className="mt-1 font-medium">
                  {modeLabel(status.desiredProfile.preferredMode)}
                </p>
                <p className="mt-1 text-sm text-[#a8bed6]">
                  {status.desiredProfile.sourceLabel}
                </p>
              </div>
              <div className="rounded border border-[#283252] bg-[#0d132b] p-3">
                <p className="text-xs uppercase tracking-wider text-[#7890ae]">{t("display.remote.state")}</p>
                <p className="mt-1 font-medium">
                  {status.activeMode ? modeLabel(status.activeMode) : translate("generated.05c0c0fb892d1336")}
                </p>
                <p className="mt-1 text-sm text-[#a8bed6]">
                  {translate("generated.b2439bcb8dee14b6")} {status.outputName ?? "unknown"} {translate("generated.a9edaf13dc67419d")} {status.xorgActive ? translate("generated.b24d6d33736ecd56") : translate("generated.8e2c7ac508139a02")} {translate("generated.270eddf370ee19a3")} {status.sunshineActive ? translate("generated.b24d6d33736ecd56") : translate("generated.8e2c7ac508139a02")}
                </p>
              </div>
            </div>

            {status.profileUpdateRequired ? (
              <div className="rounded border border-amber-400/40 bg-amber-400/10 p-3 text-sm text-amber-100">
                {translate("generated.d213c409bc224bf7")}
              </div>
            ) : (
              <div className="rounded border border-emerald-400/30 bg-emerald-400/10 p-3 text-sm text-emerald-100">
                {translate("generated.d74d1f866fe42955")}
              </div>
            )}

            <label className="block">
              <span className="mb-2 block text-sm font-medium text-[#d7e6f7]">
                {translate("generated.052f8adb4237c3a4")}
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
                      ? translate("generated.36248cd2a36664d6")
                      : ""}
                  </option>
                ))}
              </select>
            </label>

            <p className="text-sm text-[#91a9c4]">
              {translate("generated.eaca5d6c30e4ed7c")}
            </p>

            <Button
              className="w-full"
              disabled={!selectedMode || applying}
              loading={applying}
              loadingText={translate("generated.0a4d00786291e374")}
              onClick={applyMode}
            >
              {translate("generated.6306b6322724b81e")}
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
