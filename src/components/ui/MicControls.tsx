import { useState, useEffect, useCallback, useRef } from "react";
import {
  getInstanceMicConfig,
  enableInstanceMic,
  disableInstanceMic,
  updateInstanceMicSettings,
  getInstanceMicStatus,
  listMicrophones,
  reconnectInstanceMic,
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
        if (!(config?.enabled ?? false)) {
          return null;
        }
      }

      statusPollInFlightRef.current = true;
      try {
        const nextStatus = await getInstanceMicStatus(instanceId);
        setStatus(nextStatus);
        return nextStatus;
      } catch {
        return null;
      } finally {
        statusPollInFlightRef.current = false;
      }
    },
    [config?.enabled, instanceId],
  );

  const loadInitialData = useCallback(async () => {
    if (initialLoadInFlightRef.current) {
      return;
    }
    initialLoadInFlightRef.current = true;
    try {
      const [cfg] = await Promise.all([loadConfig(), loadDevices()]);
      if (cfg.enabled) {
        await loadStatus(true);
      } else {
        setStatus(null);
      }
    } catch (e) {
      setError(String(e));
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
    if (!(config?.enabled ?? false)) {
      setStatus(null);
      return;
    }

    void loadStatus(true);
    const interval = window.setInterval(() => {
      void loadStatus(false);
    }, 10000);

    return () => window.clearInterval(interval);
  }, [config?.enabled, loadStatus]);

  const handleToggleMic = async () => {
    setLoading(true);
    setError(null);
    try {
      if (config?.enabled) {
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
      setError(String(e));
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
      setError(String(e));
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
      setError(String(e));
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
      setError(String(e));
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
      setError(String(e));
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
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const micState = status?.state ?? "disabled";
  const isActive = config?.enabled ?? false;

  const stateLabel: Record<string, string> = {
    disabled: "Mic Off",
    starting: "Starting...",
    connecting: "Connecting...",
    streaming: "Active",
    no_audio_detected: "No Audio",
    wireguard_disconnected: "WireGuard Down",
    vm_agent_unreachable: "VM Unreachable",
    cloud_mic_missing: "Device Missing",
    packet_loss_high: "High Loss",
    pipewire_unavailable: "PipeWire Down",
    error: "Error",
  };

  const stateColor: Record<string, string> = {
    disabled: "bg-gray-500",
    starting: "bg-yellow-500",
    connecting: "bg-yellow-500",
    streaming: "bg-green-500",
    no_audio_detected: "bg-yellow-500",
    wireguard_disconnected: "bg-red-500",
    vm_agent_unreachable: "bg-red-500",
    cloud_mic_missing: "bg-red-500",
    packet_loss_high: "bg-orange-500",
    pipewire_unavailable: "bg-red-500",
    error: "bg-red-500",
  };

  if (compact) {
    return (
      <div className="flex items-center gap-2">
        <button
          onClick={handleToggleMic}
          disabled={loading}
          className={`px-3 py-1.5 rounded text-sm font-medium transition-colors ${
            isActive
              ? "bg-red-600 hover:bg-red-700 text-white"
              : "bg-blue-600 hover:bg-blue-700 text-white"
          } disabled:opacity-50`}
          title={isActive ? "Disable microphone" : "Enable microphone"}
        >
          {loading ? "..." : isActive ? "🎙 Stop" : "🎙 Start"}
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
          isActive
            ? "bg-red-600 hover:bg-red-700 text-white"
            : "bg-blue-600 hover:bg-blue-700 text-white"
        } disabled:opacity-50`}
      >
        {loading
          ? "Working..."
          : isActive
            ? "Disable Microphone"
            : "Enable Microphone"}
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
          <option value="standard">Standard (20ms, 32 kbps)</option>
          <option value="lowLatency">Low Latency (10ms, 48 kbps)</option>
          <option value="highQuality">High Quality (20ms, 64 kbps)</option>
        </select>
      </div>

      <div className="flex flex-wrap gap-2">
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
          {status.bitrateKbps && (
            <div className="flex justify-between">
              <span>Bitrate</span>
              <span>{status.bitrateKbps} kbps</span>
            </div>
          )}
        </div>
      )}

      {error && (
        <p className="text-red-400 text-xs bg-red-900/30 rounded px-2 py-1">
          {error}
        </p>
      )}
    </div>
  );
}
