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
  return "The Moonlight stream options could not be updated.";
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
            Moonlight stream options
          </p>
          <h2
            id="instance-moonlight-options-title"
            className="mt-1 text-xl font-semibold"
          >
            {instance.label}
          </h2>
        </div>
        <Button variant="ghost" disabled={saving} onClick={onClose}>
          Close
        </Button>
      </div>

      <ModalBody className="space-y-4 p-5">
        {loading ? (
          <p className="text-[#a8bed6]">Reading Moonlight preferences…</p>
        ) : (
          <>
            <ToggleRow
              label="Adaptive packet size"
              description="Learn the largest safe GameStream video packet size for this instance's network path, downshift automatically, and cache it. Starts conservatively and never probes the network actively."
              checked={latency.adaptivePacketSizeEnabled}
              disabled={saving}
              onChange={(checked) =>
                updateLatency({ adaptivePacketSizeEnabled: checked })
              }
            />
            <ToggleRow
              label="Adaptive late frame drop"
              description="Drop a stale decoded frame only when a newer frame is queued, the decoder is back-pressured, and latency priority mode is active."
              checked={latency.adaptiveLateFrameDropEnabled}
              disabled={saving}
              onChange={(checked) =>
                updateLatency({ adaptiveLateFrameDropEnabled: checked })
              }
            />
            <ToggleRow
              label="Decoder back-pressure policy"
              description="Keep decode/render queues bounded and adapt queue limits with hysteresis so temporary GPU pressure does not become permanent end-to-end latency."
              checked={latency.decoderBackpressurePolicyEnabled}
              disabled={saving}
              onChange={(checked) =>
                updateLatency({ decoderBackpressurePolicyEnabled: checked })
              }
            />
            <ToggleRow
              label="Reconnect after unexpected termination"
              description="Perform one bounded reconnect attempt without quitting the remote application after an unexpected connection loss."
              checked={latency.autoReconnectOnUnexpectedTermination}
              disabled={saving}
              onChange={(checked) =>
                updateLatency({ autoReconnectOnUnexpectedTermination: checked })
              }
            />

            <div className="grid gap-3 sm:grid-cols-2">
              <label className="block">
                <span className="mb-2 block text-sm font-medium text-[#d7e6f7]">
                  Frame pacing
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
                  <option value="off">Off</option>
                  <option value="automatic">Automatic</option>
                  <option value="software">Software</option>
                  <option value="hardwareMultiple">Hardware multiple</option>
                </select>
              </label>
              <label className="block">
                <span className="mb-2 block text-sm font-medium text-[#d7e6f7]">
                  Frame buffer
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
                  <option value="off">Off (lowest latency)</option>
                  <option value="oneFrame">1 frame</option>
                  <option value="twoFrames">2 frames</option>
                  <option value="threeFrames">3 frames</option>
                </select>
              </label>
            </div>

            <p className="text-xs text-[#8fa7c6]">
              Changes are stored as an override for this instance and take
              effect the next time its stream starts. The adaptive packet-size
              controller and adaptive late-frame dropping default to off for
              safety.
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
          Cancel
        </Button>
        <Button loading={saving} loadingText="Saving…" onClick={() => void save()}>
          Save
        </Button>
      </div>
    </ModalFrame>
  );
}
