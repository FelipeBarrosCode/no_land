import { useState, useEffect, useCallback, useRef } from "react";
import {
  getInstanceMicConfig,
  enableInstanceMic,
  disableInstanceMic,
  updateInstanceMicSettings,
  getInstanceMicStatus,
  listMicrophones,
  reconnectInstanceMic,
  muteInstanceMic,
  unmuteInstanceMic,
  recreateInstanceMicDevice,
} from "../../lib/backend";
import type {
  InstanceMicConfig,
  InstanceMicRuntimeStatus,
  MicQualityProfile,
  MicrophoneDevice,
} from "../../lib/types";

interface MicControlsProps {
  instanceId: number;
  compact?: boolean;
}

function micErrorMessage(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (typeof error === "object" && error !== null) {
    const details = Reflect.get(error, "details");
    if (typeof details === "string" && details.trim()) {
      return details;
    }
    const message = Reflect.get(error, "message");
    if (typeof message === "string" && message.trim()) {
      return message;
    }
  }
  return "Microphone operation failed. Check the pipeline status and try again.";
}

export function MicControls({ instanceId, compact = false }: MicControlsProps) {
  const [config, setConfig] = useState<InstanceMicConfig | null>(null);
  const [status, setStatus] = useState<InstanceMicRuntimeStatus | null>(null);
  const [devices, setDevices] = useState<MicrophoneDevice[]>([]);
  const [devicesLoading, setDevicesLoading] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const statusPollInFlightRef = useRef(false);
  const initialLoadInFlightRef = useRef(false);

  const loadConfig = useCallback(async () => {
    const cfg = await getInstanceMicConfig(instanceId);
    setConfig(cfg);
    return cfg;
  }, [instanceId]);

  const loadDevices = useCallback(async (forceRefresh = false) => {
    setDevicesLoading(true);
    try {
      const devs = await listMicrophones({ forceRefresh });
      setDevices(devs);
      return devs;
    } finally {
      setDevicesLoading(false);
    }
  }, []);

  const loadStatus = useCallback(
    async (force = false) => {
      if (statusPollInFlightRef.current) {
        return null;
      }
      if (!force) {
        if (document.visibilityState !== "visible") {
          return null;
        }
        if (!document.hasFocus()) {
          return null;
        }
        if (!(config?.forwardingEnabled ?? false)) {
          return null;
        }
      }

      statusPollInFlightRef.current = true;
      try {
        const nextStatus = await getInstanceMicStatus(instanceId);
        setStatus(nextStatus);
        return nextStatus;
      } catch (error) {
        setError(micErrorMessage(error));
        return null;
      } finally {
        statusPollInFlightRef.current = false;
      }
    },
    [config?.forwardingEnabled, instanceId],
  );

  const loadInitialData = useCallback(async () => {
    if (initialLoadInFlightRef.current) {
      return;
    }
    initialLoadInFlightRef.current = true;
    try {
      const [cfg] = await Promise.all([loadConfig(), loadDevices()]);
      if (cfg.forwardingEnabled) {
        await loadStatus(true);
      } else {
        setStatus(null);
      }
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      initialLoadInFlightRef.current = false;
    }
  }, [loadConfig, loadDevices, loadStatus]);

  useEffect(() => {
    void loadInitialData();
  }, [loadInitialData]);

  useEffect(() => {
    const onVisibilityOrFocus = () => {
      if (document.visibilityState === "visible" && document.hasFocus()) {
        void loadConfig();
        void loadStatus(true);
      }
    };

    document.addEventListener("visibilitychange", onVisibilityOrFocus);
    window.addEventListener("focus", onVisibilityOrFocus);

    return () => {
      document.removeEventListener("visibilitychange", onVisibilityOrFocus);
      window.removeEventListener("focus", onVisibilityOrFocus);
    };
  }, [loadConfig, loadStatus]);

  useEffect(() => {
    if (!(config?.forwardingEnabled ?? false)) {
      setStatus(null);
      return;
    }

    void loadStatus(true);
    const interval = window.setInterval(() => {
      void loadStatus(false);
    }, 10000);

    return () => window.clearInterval(interval);
  }, [config?.forwardingEnabled, loadStatus]);

  const handleToggleMic = async () => {
    setLoading(true);
    setError(null);
    try {
      if (config?.forwardingEnabled) {
        await disableInstanceMic(instanceId);
        const nextConfig = await loadConfig();
        if (!nextConfig.enabled) {
          setStatus(null);
        }
      } else {
        await enableInstanceMic(instanceId, config?.qualityProfile);
        await loadConfig();
        await loadStatus(true);
      }
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const handleProfileChange = async (profile: MicQualityProfile) => {
    setLoading(true);
    setError(null);
    try {
      await updateInstanceMicSettings(instanceId, { qualityProfile: profile });
      await loadConfig();
      await loadStatus(true);
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const handleDeviceChange = async (deviceId: string) => {
    setLoading(true);
    setError(null);
    try {
      await updateInstanceMicSettings(instanceId, { deviceId });
      await loadConfig();
      await loadStatus(true);
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const handleRefreshDevices = async () => {
    setLoading(true);
    setError(null);
    try {
      await loadDevices(true);
      await loadConfig();
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const handleAutoConnectChange = async (autoConnect: boolean) => {
    setLoading(true);
    setError(null);
    try {
      await updateInstanceMicSettings(instanceId, { autoConnect });
      await loadConfig();
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const handleMuteToggle = async () => {
    setLoading(true);
    setError(null);
    try {
      if (status?.muted) {
        await unmuteInstanceMic(instanceId);
      } else {
        await muteInstanceMic(instanceId);
      }
      await loadStatus(true);
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const handleReconnect = async () => {
    setLoading(true);
    setError(null);
    try {
      await reconnectInstanceMic(instanceId);
      await loadConfig();
      await loadStatus(true);
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const handleRecreateRemoteDevice = async () => {
    setLoading(true);
    setError(null);
    try {
      await recreateInstanceMicDevice(instanceId);
      await loadStatus(true);
    } catch (e) {
      setError(micErrorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  const isForwardingEnabled = config?.forwardingEnabled ?? false;
  const micState =
    status?.state === "disabled" && isForwardingEnabled
      ? "ready"
      : (status?.state ?? (isForwardingEnabled ? "ready" : "disabled"));
  const isActive = status?.enabled ?? config?.enabled ?? false;

  const stateLabel: Record<string, string> = {
    disabled: "Mic Off",
    ready: "Ready",
    starting: "Starting...",
    connecting: "Connecting...",
    streaming: "Active",
    no_audio_detected: "No Audio",
    wireguard_disconnected: "WireGuard Down",
    vm_agent_unreachable: "VM Unreachable",
    cloud_mic_missing: "Device Missing",
    packet_loss_high: "High Loss",
    pipewire_unavailable: "PipeWire Down",
    no_microphone: "No Microphone",
    capture_failure: "Capture Failed",
    pipeline_failure: "Media Sidecar Failed",
    network_failure: "Network Failed",
    reconnecting: "Reconnecting...",
    degraded: "Degraded",
    error: "Error",
  };

  const stateColor: Record<string, string> = {
    disabled: "bg-gray-500",
    ready: "bg-blue-500",
    starting: "bg-yellow-500",
    connecting: "bg-yellow-500",
    streaming: "bg-green-500",
    no_audio_detected: "bg-yellow-500",
    wireguard_disconnected: "bg-red-500",
    vm_agent_unreachable: "bg-red-500",
    cloud_mic_missing: "bg-red-500",
    packet_loss_high: "bg-orange-500",
    pipewire_unavailable: "bg-red-500",
    no_microphone: "bg-red-500",
    capture_failure: "bg-red-500",
    pipeline_failure: "bg-red-500",
    network_failure: "bg-red-500",
    reconnecting: "bg-yellow-500",
    degraded: "bg-orange-500",
    error: "bg-red-500",
  };

  if (compact) {
    return (
      <div className="flex items-center gap-2">
        <button
          onClick={handleToggleMic}
          disabled={loading}
          className={`px-3 py-1.5 rounded text-sm font-medium transition-colors ${
            isForwardingEnabled
              ? "bg-red-600 hover:bg-red-700 text-white"
              : "bg-blue-600 hover:bg-blue-700 text-white"
          } disabled:opacity-50`}
          title={
            isForwardingEnabled ? "Disable microphone forwarding" : "Enable microphone forwarding"
          }
        >
          {loading ? "..." : isForwardingEnabled ? "🎙 Disable" : "🎙 Enable"}
        </button>
        <span
          className={`w-2.5 h-2.5 rounded-full ${stateColor[micState] ?? "bg-gray-500"}`}
          title={stateLabel[micState] ?? micState}
        />
        {error && (
          <span className="text-red-400 text-xs" title={error}>
            ⚠
          </span>
        )}
      </div>
    );
  }

  return (
    <div className="p-4 bg-gray-900 rounded-lg border border-gray-700 space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-gray-200">
          Microphone Forwarding
        </h3>
        <div className="flex items-center gap-2">
          <span
            className={`w-2.5 h-2.5 rounded-full ${stateColor[micState] ?? "bg-gray-500"}`}
          />
          <span className="text-xs text-gray-400">
            {stateLabel[micState] ?? micState}
          </span>
        </div>
      </div>

      {/* Toggle */}
      <button
        onClick={handleToggleMic}
        disabled={loading}
        className={`w-full py-2 rounded font-medium transition-colors ${
          isForwardingEnabled
            ? "bg-red-600 hover:bg-red-700 text-white"
            : "bg-blue-600 hover:bg-blue-700 text-white"
        } disabled:opacity-50`}
      >
        {loading
          ? "Working..."
          : isForwardingEnabled
            ? "Disable Microphone Forwarding"
            : "Enable Microphone Forwarding"}
      </button>

      {/* Device selection */}
      <div>
        <div className="mb-1 flex items-center justify-between gap-2">
          <label className="text-xs text-gray-400 block">Input Device</label>
          <button
            type="button"
            onClick={handleRefreshDevices}
            disabled={loading}
            className="rounded border border-gray-600 bg-gray-800 px-2 py-1 text-[10px] text-gray-300 transition-colors hover:bg-gray-700 disabled:opacity-50"
          >
            Refresh
          </button>
        </div>
        <select
          className="w-full bg-gray-800 border border-gray-600 rounded px-3 py-1.5 text-sm text-gray-200"
          value={config?.deviceId ?? "default"}
          onChange={(e) => handleDeviceChange(e.target.value)}
          disabled={loading || devicesLoading || devices.length === 0}
        >
          {devicesLoading ? (
            <option value={config?.deviceId ?? "default"}>
              Loading microphones...
            </option>
          ) : devices.length === 0 ? (
            <option value={config?.deviceId ?? "default"}>
              No microphones detected
            </option>
          ) : (
            devices.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name} {d.isDefault ? "(Default)" : ""}
              </option>
            ))
          )}
        </select>
        <p className="mt-1 text-[11px] text-gray-500">
          Current: {config?.deviceName ?? "System Default"}
        </p>
      </div>

      {/* Quality profile */}
      <div>
        <label className="text-xs text-gray-400 block mb-1">Quality</label>
        <select
          className="w-full bg-gray-800 border border-gray-600 rounded px-3 py-1.5 text-sm text-gray-200"
          value={config?.qualityProfile ?? "standard"}
          onChange={(e) =>
            handleProfileChange(e.target.value as MicQualityProfile)
          }
        >
          <option value="standard">Balanced (10ms, 32 kbps)</option>
          <option value="lowLatency">Low Latency (10ms, 48 kbps)</option>
          <option value="highQuality">High Quality (10ms, 64 kbps)</option>
        </select>
      </div>

      <label className="flex items-center justify-between gap-3 rounded border border-gray-700 bg-gray-800/60 px-3 py-2 text-xs text-gray-300">
        <span>
          Auto-connect with game stream
          <span className="mt-0.5 block text-[10px] text-gray-500">
            Mic failures never block Moonlight or Sunshine.
          </span>
        </span>
        <input
          type="checkbox"
          checked={config?.autoConnect ?? true}
          onChange={(event) => handleAutoConnectChange(event.target.checked)}
          disabled={loading || !isForwardingEnabled}
          className="h-4 w-4 accent-blue-500"
        />
      </label>

      <div className="flex flex-wrap gap-2">
        <button
          onClick={handleMuteToggle}
          disabled={loading || !isActive}
          className="px-3 py-1.5 rounded border border-gray-600 bg-gray-800 text-xs text-gray-200 transition-colors hover:bg-gray-700 disabled:opacity-50"
        >
          {status?.muted ? "Unmute" : "Mute"}
        </button>
        <button
          onClick={handleReconnect}
          disabled={loading || !isActive}
          className="px-3 py-1.5 rounded border border-gray-600 bg-gray-800 text-xs text-gray-200 transition-colors hover:bg-gray-700 disabled:opacity-50"
        >
          Reconnect Mic
        </button>
        <button
          onClick={handleRecreateRemoteDevice}
          disabled={loading}
          className="px-3 py-1.5 rounded border border-gray-600 bg-gray-800 text-xs text-gray-200 transition-colors hover:bg-gray-700 disabled:opacity-50"
        >
          Recreate Remote Device
        </button>
      </div>

      {/* Stats when active */}
      {isActive && status && (
        <div className="space-y-1 text-xs text-gray-400">
          {status.packetLossPercent !== undefined && (
            <div className="flex justify-between">
              <span>Packet Loss</span>
              <span>{status.packetLossPercent.toFixed(1)}%</span>
            </div>
          )}
          {status.jitterMs !== undefined && (
            <div className="flex justify-between">
              <span>Jitter</span>
              <span>{status.jitterMs.toFixed(1)} ms</span>
            </div>
          )}
          {status.bufferDepthMs !== undefined && (
            <div className="flex justify-between">
              <span>Buffer</span>
              <span>{status.bufferDepthMs.toFixed(0)} ms</span>
            </div>
          )}
          <div className="flex justify-between">
            <span>Capture Buffer</span>
            <span>{status.ringFillMs.toFixed(1)} ms</span>
          </div>
          <div className="flex justify-between">
            <span>Sidecar Queue</span>
            <span>{status.appsrcQueueMs.toFixed(1)} ms</span>
          </div>
          {status.bitrateKbps && (
            <div className="flex justify-between">
              <span>Bitrate</span>
              <span>{status.bitrateKbps} kbps</span>
            </div>
          )}
          {status.reconnectCount > 0 && (
            <div className="flex justify-between">
              <span>Sidecar Recoveries</span>
              <span>{status.reconnectCount}</span>
            </div>
          )}
        </div>
      )}

      {(error || status?.error) && (
        <p className="text-red-400 text-xs bg-red-900/30 rounded px-2 py-1">
          {error ?? status?.error}
        </p>
      )}
    </div>
  );
}
