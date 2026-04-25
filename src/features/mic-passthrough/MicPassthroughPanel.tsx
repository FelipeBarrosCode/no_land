import { useState, useEffect } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import type {
  InstanceMicConfig,
  InstanceMicRuntimeStatus,
  MicQualityProfile,
  MicSessionResponse
} from "../../lib/types";

interface Props {
  instanceId: number;
  config: InstanceMicConfig | null;
  status: InstanceMicRuntimeStatus | null;
  session: MicSessionResponse | null;
  busy: boolean;
  instanceActionRunning: boolean;
  onLoadConfig: (instanceId: number) => Promise<void>;
  onLoadStatus: (instanceId: number) => Promise<void>;
  onEnable: (instanceId: number, profile?: MicQualityProfile) => Promise<MicSessionResponse | null>;
  onDisable: (instanceId: number) => Promise<void>;
  onReconnect: (instanceId: number) => Promise<MicSessionResponse | null>;
  onRecreateDevice: (instanceId: number) => Promise<void>;
  onUpdateSettings: (instanceId: number, payload: { qualityProfile?: MicQualityProfile }) => Promise<void>;
  onClose: () => void;
}

const qualityOptions: { value: MicQualityProfile; label: string }[] = [
  { value: "standard", label: "Standard" },
  { value: "lowLatency", label: "Low Latency" },
  { value: "highQuality", label: "High Quality" }
];

const stateColors: Record<string, string> = {
  disabled: "text-gray-400",
  starting: "text-yellow-400",
  connecting: "text-yellow-400",
  streaming: "text-green-400",
  no_audio_detected: "text-orange-400",
  wireguard_disconnected: "text-red-400",
  vm_agent_unreachable: "text-red-400",
  cloud_mic_missing: "text-red-400",
  packet_loss_high: "text-orange-400",
  pipewire_unavailable: "text-red-400",
  error: "text-red-400"
};

const stateLabels: Record<string, string> = {
  disabled: "Off",
  starting: "Starting",
  connecting: "Connecting",
  streaming: "Connected",
  no_audio_detected: "No Audio Detected",
  wireguard_disconnected: "WireGuard Disconnected",
  vm_agent_unreachable: "VM Agent Unreachable",
  cloud_mic_missing: "Cloud Mic Missing",
  packet_loss_high: "Packet Loss High",
  pipewire_unavailable: "PipeWire Unavailable",
  error: "Error"
};

export function MicPassthroughPanel({
  instanceId,
  config,
  status,
  busy,
  instanceActionRunning,
  onLoadConfig,
  onLoadStatus,
  onEnable,
  onDisable,
  onReconnect,
  onRecreateDevice,
  onUpdateSettings,
  onClose
}: Props) {
  const [selectedProfile, setSelectedProfile] = useState<MicQualityProfile>("standard");
  const [isEnabled, setIsEnabled] = useState(false);

  const actionDisabled = busy || instanceActionRunning;

  useEffect(() => {
    void onLoadConfig(instanceId);
    void onLoadStatus(instanceId);
  }, [instanceId]);

  useEffect(() => {
    if (config) {
      setSelectedProfile(config.qualityProfile);
      setIsEnabled(config.enabled);
    }
  }, [config]);

  // Poll status while enabled
  useEffect(() => {
    if (!isEnabled) return;
    const interval = setInterval(() => {
      void onLoadStatus(instanceId);
    }, 3000);
    return () => clearInterval(interval);
  }, [isEnabled, instanceId]);

  const handleToggle = async () => {
    if (isEnabled) {
      await onDisable(instanceId);
      setIsEnabled(false);
    } else {
      const result = await onEnable(instanceId, selectedProfile);
      if (result) {
        setIsEnabled(true);
      }
    }
    void onLoadStatus(instanceId);
  };

  const handleProfileChange = async (profile: MicQualityProfile) => {
    setSelectedProfile(profile);
    await onUpdateSettings(instanceId, { qualityProfile: profile });
  };

  const currentState = status?.state ?? "disabled";
  const stateColor = stateColors[currentState] ?? "text-gray-400";
  const stateLabel = stateLabels[currentState] ?? currentState;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm p-4">
      <Card className="w-full max-w-lg p-6">
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-lg font-display text-neon-cyan">Microphone Passthrough</h3>
          <Button variant="ghost" onClick={onClose} disabled={actionDisabled}>
            Close
          </Button>
        </div>

        <div className="space-y-4">
          {/* Status */}
          <div className="flex items-center justify-between p-3 bg-[#0b0f23] rounded border border-[#3f476c]">
            <span className="text-sm text-gray-400">Status</span>
            <span className={`text-sm font-medium ${stateColor}`}>{stateLabel}</span>
          </div>

          {/* Quality Profile */}
          <div>
            <label className="text-xs text-gray-500 uppercase tracking-wider block mb-2">
              Quality Profile
            </label>
            <div className="grid grid-cols-3 gap-2">
              {qualityOptions.map((opt) => (
                <button
                  key={opt.value}
                  className={`px-3 py-2 text-xs rounded border transition ${
                    selectedProfile === opt.value
                      ? "border-neon-cyan bg-neon-cyan/10 text-neon-cyan"
                      : "border-[#3f476c] text-gray-400 hover:border-gray-500"
                  }`}
                  onClick={() => handleProfileChange(opt.value)}
                  disabled={actionDisabled || isEnabled}
                >
                  {opt.label}
                </button>
              ))}
            </div>
            <p className="text-[10px] text-gray-500 mt-1">
              {selectedProfile === "lowLatency"
                ? "48 kbps, 10 ms frames — best for voice chat"
                : selectedProfile === "highQuality"
                ? "64 kbps, 20 ms frames — best for recording"
                : "32 kbps, 20 ms frames — balanced"}
            </p>
          </div>

          {/* Enable Toggle */}
          <div className="flex items-center justify-between">
            <div>
              <span className="text-sm text-gray-200">Enable Microphone</span>
              <p className="text-xs text-gray-500">
                Stream local mic to VM via WireGuard
              </p>
            </div>
            <button
              className={`relative w-12 h-6 rounded-full transition ${
                isEnabled ? "bg-neon-cyan" : "bg-gray-600"
              }`}
              onClick={handleToggle}
              disabled={actionDisabled}
            >
              <span
                className={`absolute top-1 w-4 h-4 rounded-full bg-white transition ${
                  isEnabled ? "left-7" : "left-1"
                }`}
              />
            </button>
          </div>

          {/* Metrics */}
          {status && isEnabled && (
            <div className="grid grid-cols-2 gap-2 text-xs">
              <div className="p-2 bg-[#0b0f23] rounded border border-[#3f476c]">
                <span className="text-gray-500">Packet Loss</span>
                <p className="text-gray-200">{status.packetLossPercent.toFixed(1)}%</p>
              </div>
              <div className="p-2 bg-[#0b0f23] rounded border border-[#3f476c]">
                <span className="text-gray-500">Jitter</span>
                <p className="text-gray-200">{status.jitterMs.toFixed(1)} ms</p>
              </div>
              <div className="p-2 bg-[#0b0f23] rounded border border-[#3f476c]">
                <span className="text-gray-500">Buffer Depth</span>
                <p className="text-gray-200">{status.bufferDepthMs.toFixed(1)} ms</p>
              </div>
              <div className="p-2 bg-[#0b0f23] rounded border border-[#3f476c]">
                <span className="text-gray-500">Bitrate</span>
                <p className="text-gray-200">{status.bitrateKbps} kbps</p>
              </div>
            </div>
          )}

          {/* Error */}
          {status?.error && (
            <div className="p-3 bg-red-900/20 rounded border border-red-500/30">
              <p className="text-xs text-red-300">{status.error}</p>
            </div>
          )}

          {/* Actions */}
          <div className="flex gap-2">
            <Button
              variant="secondary"
              className="text-xs"
              disabled={actionDisabled || !isEnabled}
              onClick={() => onReconnect(instanceId)}
            >
              Reconnect
            </Button>
            <Button
              variant="ghost"
              className="text-xs"
              disabled={actionDisabled}
              onClick={() => onRecreateDevice(instanceId)}
            >
              Recreate Cloud Mic
            </Button>
          </div>
        </div>
      </Card>
    </div>
  );
}
