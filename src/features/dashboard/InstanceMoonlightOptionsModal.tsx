import { translate } from "../../lib/i18n";
import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import {
  moonlightGetHostLatencyPreferences,
  moonlightUpdateHostLatencyPreferences,
} from "../../lib/backend";
import type {
  MoonlightFrameBufferMode,
  MoonlightPacingMode,
  NolandLatencyConfig,
  RentedInstanceSummary,
} from "../../lib/types";

interface Props {
  instance: RentedInstanceSummary;
  onClose: () => void;
}

function errorMessage(error: unknown) {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return translate("generated.9eb5233b9fc7dc18");
}

const EMPTY_LATENCY: NolandLatencyConfig = {
  telemetryEnabled: false,
  adaptiveLateFrameDropEnabled: false,
  adaptivePacketSizeEnabled: false,
  decoderBackpressurePolicyEnabled: false,
  pacingMode: "off",
  frameBufferMode: "off",
  autoReconnectOnUnexpectedTermination: true,
  remoteStreamMode: "auto",
  remotePacketSize: 1024,
  lateFrameToleranceUs: 0,
  vsyncEnabled: false,
};

interface ToggleRowProps {
  label: string;
  description: string;
  checked: boolean;
  disabled: boolean;
  onChange: (checked: boolean) => void;
}

function ToggleRow({
  label,
  description,
  checked,
  disabled,
  onChange,
}: ToggleRowProps) {
  return (
    <label className="flex items-start gap-3 rounded border border-[#283252] bg-[#0d132b] p-3">
      <input
        type="checkbox"
        className="mt-1 h-4 w-4 accent-cyan-400"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span className="flex-1">
        <span className="block text-sm font-medium text-[#d7e6f7]">{label}</span>
        <span className="mt-1 block text-xs leading-5 text-[#8fa7c6]">
          {description}
        </span>
      </span>
    </label>
  );
}

export function InstanceMoonlightOptionsModal({ instance, onClose }: Props) {
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [latency, setLatency] = useState<NolandLatencyConfig>(EMPTY_LATENCY);

  const hostId = `instance-${instance.instanceId}`;

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    moonlightGetHostLatencyPreferences(hostId)
      .then((response) => {
        if (cancelled) return;
        setLatency({ ...EMPTY_LATENCY, ...response.effective.latency });
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
  }, [hostId]);

  function updateLatency(patch: Partial<NolandLatencyConfig>) {
    setLatency((current) => ({ ...current, ...patch }));
  }

  async function save() {
    setSaving(true);
    setError(null);
    try {
      await moonlightUpdateHostLatencyPreferences(hostId, latency);
      onClose();
    } catch (nextError) {
      setError(errorMessage(nextError));
      setSaving(false);
    }
  }

  return (
    <ModalFrame
      labelledBy="instance-moonlight-options-title"
      panelClassName="pixel-frame max-w-xl bg-[#090d20] text-white"
    >
      <div className="flex items-start justify-between gap-4 border-b border-[#283252] p-5">
        <div>
          <p className="font-display text-[10px] uppercase tracking-[0.16em] text-neon-cyan">
            {translate("generated.8a7d65d36229a83f")}
          </p>
          <h2
            id="instance-moonlight-options-title"
            className="mt-1 text-xl font-semibold"
          >
            {instance.label}
          </h2>
        </div>
        <Button variant="ghost" disabled={saving} onClick={onClose}>
          {translate("generated.7d9eb7acb13e2462")}
        </Button>
      </div>

      <ModalBody className="space-y-4 p-5">
        {loading ? (
          <p className="text-[#a8bed6]">{translate("generated.e1348b3458708ca8")}</p>
        ) : (
          <>
            <ToggleRow
              label={translate("generated.044899d0a0485f06")}
              description={translate("generated.1886c38e5dd5174d")}
              checked={latency.adaptivePacketSizeEnabled}
              disabled={saving}
              onChange={(checked) =>
                updateLatency({ adaptivePacketSizeEnabled: checked })
              }
            />
            <ToggleRow
              label={translate("generated.ad57b1c4e5a3227b")}
              description={translate("generated.01c35e756819639a")}
              checked={latency.adaptiveLateFrameDropEnabled}
              disabled={saving}
              onChange={(checked) =>
                updateLatency({ adaptiveLateFrameDropEnabled: checked })
              }
            />
            <ToggleRow
              label={translate("generated.9c17a4e9a9528324")}
              description={translate("generated.c7c37e882cbeeacf")}
              checked={latency.decoderBackpressurePolicyEnabled}
              disabled={saving}
              onChange={(checked) =>
                updateLatency({ decoderBackpressurePolicyEnabled: checked })
              }
            />
            <ToggleRow
              label={translate("generated.262eecd575e76638")}
              description={translate("generated.59b0122e9530fbed")}
              checked={latency.autoReconnectOnUnexpectedTermination}
              disabled={saving}
              onChange={(checked) =>
                updateLatency({ autoReconnectOnUnexpectedTermination: checked })
              }
            />

            <div className="grid gap-3 sm:grid-cols-2">
              <label className="block">
                <span className="mb-2 block text-sm font-medium text-[#d7e6f7]">
                  {translate("generated.3d82644280c832dc")}
                </span>
                <select
                  className="w-full rounded border border-[#354269] bg-[#080d1f] px-3 py-2 text-white outline-none focus:border-neon-cyan"
                  value={latency.pacingMode}
                  disabled={saving}
                  onChange={(event) =>
                    updateLatency({
                      pacingMode: event.target.value as MoonlightPacingMode,
                    })
                  }
                >
                  <option value="off">{translate("generated.ca7981b46ecf2c17")}</option>
                  <option value="automatic">{translate("generated.d461a493a3753877")}</option>
                  <option value="software">{translate("generated.9b3289a385a5301e")}</option>
                  <option value="hardwareMultiple">{translate("generated.a2562853e81c4347")}</option>
                </select>
              </label>
              <label className="block">
                <span className="mb-2 block text-sm font-medium text-[#d7e6f7]">
                  {translate("generated.174bb641a341407c")}
                </span>
                <select
                  className="w-full rounded border border-[#354269] bg-[#080d1f] px-3 py-2 text-white outline-none focus:border-neon-cyan"
                  value={latency.frameBufferMode}
                  disabled={saving}
                  onChange={(event) =>
                    updateLatency({
                      frameBufferMode: event.target
                        .value as MoonlightFrameBufferMode,
                    })
                  }
                >
                  <option value="off">{translate("generated.7976238fbfa4a2ac")}</option>
                  <option value="oneFrame">{translate("generated.7b6ae5d47c287709")}</option>
                  <option value="twoFrames">{translate("generated.d2e40bcf12edaec0")}</option>
                  <option value="threeFrames">{translate("generated.32f0047e2904db75")}</option>
                </select>
              </label>
            </div>

            <p className="text-xs text-[#8fa7c6]">
              {translate("generated.364c7533383aad88")}
            </p>
          </>
        )}

        {error ? (
          <div className="rounded border border-red-400/50 bg-red-950/50 p-3 text-sm text-red-200">
            {error}
          </div>
        ) : null}
      </ModalBody>

      <div className="flex justify-end gap-2 border-t border-[#283252] p-4">
        <Button variant="ghost" disabled={saving} onClick={onClose}>
          {translate("generated.19766ed6ccb2f4a3")}
        </Button>
        <Button loading={saving} loadingText={translate("generated.23e39291d6135814")} onClick={() => void save()}>
          {translate("generated.1509f561f2416598")}
        </Button>
      </div>
    </ModalFrame>
  );
}
