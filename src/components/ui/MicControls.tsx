import { translate } from "../../lib/i18n";
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
  return translate("generated.78fdb0a14598c194");
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
    disabled: translate("generated.02090677aa9e6109"),
    ready: translate("generated.5fa7aac5375c5815"),
    starting: translate("generated.82b93630a921dddf"),
    connecting: translate("generated.5f04ae9ed6a865bb"),
    streaming: translate("generated.92340695899bd2d8"),
    no_audio_detected: translate("generated.067606abf652e6a9"),
    wireguard_disconnected: translate("generated.5cc990792b650912"),
    vm_agent_unreachable: translate("generated.94de2f1e928d8c37"),
    cloud_mic_missing: translate("generated.e98b45fe20bf2eed"),
    packet_loss_high: translate("generated.17a04810ea0dda88"),
    pipewire_unavailable: translate("generated.6c255bc02cc9b1ee"),
    no_microphone: translate("generated.85565c9728cc4f76"),
    capture_failure: translate("generated.c115adae4e24c8f0"),
    pipeline_failure: translate("generated.8fe6533cba77c001"),
    network_failure: translate("generated.7f0fa908c1b69762"),
    reconnecting: translate("generated.66bce4bdb48e51a0"),
    degraded: translate("generated.a8494c12f2243903"),
    error: translate("generated.54a0e8c17ebb21a1"),
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
            isForwardingEnabled ? translate("generated.50f0b881b2dc5ea7") : translate("generated.38cfc5ba526b9e43")
          }
        >
          {loading ? "..." : isForwardingEnabled ? translate("generated.eec08d90f7b34b62") : translate("generated.218f9aee1be74bc0")}
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
          {translate("generated.8967bd0544368ea6")}
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
          ? translate("generated.b93900bded315d04")
          : isForwardingEnabled
            ? translate("generated.ed4ec948a890f213")
            : translate("generated.bfca33b78f323ad8")}
      </button>

      {/* Device selection */}
      <div>
        <div className="mb-1 flex items-center justify-between gap-2">
          <label className="text-xs text-gray-400 block">{translate("generated.967438abb32e13c2")}</label>
          <button
            type="button"
            onClick={handleRefreshDevices}
            disabled={loading}
            className="rounded border border-gray-600 bg-gray-800 px-2 py-1 text-[10px] text-gray-300 transition-colors hover:bg-gray-700 disabled:opacity-50"
          >
            {translate("generated.0e91610117029a62")}
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
              {translate("generated.a0926aa44b31b666")}
            </option>
          ) : devices.length === 0 ? (
            <option value={config?.deviceId ?? "default"}>
              {translate("generated.a83ecb13b294fb6f")}
            </option>
          ) : (
            devices.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name} {d.isDefault ? translate("generated.1fc1ffe61be98b40") : ""}
              </option>
            ))
          )}
        </select>
        <p className="mt-1 text-[11px] text-gray-500">
          {translate("generated.c09f632874c2511b")} {config?.deviceName ?? "System Default"}
        </p>
      </div>

      {/* Quality profile */}
      <div>
        <label className="text-xs text-gray-400 block mb-1">{translate("generated.1b2c08a8733d7ff1")}</label>
        <select
          className="w-full bg-gray-800 border border-gray-600 rounded px-3 py-1.5 text-sm text-gray-200"
          value={config?.qualityProfile ?? "standard"}
          onChange={(e) =>
            handleProfileChange(e.target.value as MicQualityProfile)
          }
        >
          <option value="standard">{translate("generated.919cc4ac2074deb4")}</option>
          <option value="lowLatency">{translate("generated.3078d46679cbfa8e")}</option>
          <option value="highQuality">{translate("generated.860ea320f86090e3")}</option>
        </select>
      </div>

      <label className="flex items-center justify-between gap-3 rounded border border-gray-700 bg-gray-800/60 px-3 py-2 text-xs text-gray-300">
        <span>
          {translate("generated.55d4e8663b94f916")}
          <span className="mt-0.5 block text-[10px] text-gray-500">
            {translate("generated.49094771c472cd49")}
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
          {status?.muted ? translate("generated.ce4ee4efc5e324fc") : translate("generated.8dd6857baf026850")}
        </button>
        <button
          onClick={handleReconnect}
          disabled={loading || !isActive}
          className="px-3 py-1.5 rounded border border-gray-600 bg-gray-800 text-xs text-gray-200 transition-colors hover:bg-gray-700 disabled:opacity-50"
        >
          {translate("generated.5c2af7646c1fe2ed")}
        </button>
        <button
          onClick={handleRecreateRemoteDevice}
          disabled={loading}
          className="px-3 py-1.5 rounded border border-gray-600 bg-gray-800 text-xs text-gray-200 transition-colors hover:bg-gray-700 disabled:opacity-50"
        >
          {translate("generated.30ca1fb4239148e2")}
        </button>
      </div>

      {/* Stats when active */}
      {isActive && status && (
        <div className="space-y-1 text-xs text-gray-400">
          {status.packetLossPercent !== undefined && (
            <div className="flex justify-between">
              <span>{translate("generated.efe580226c57a395")}</span>
              <span>{status.packetLossPercent.toFixed(1)}%</span>
            </div>
          )}
          {status.jitterMs !== undefined && (
            <div className="flex justify-between">
              <span>{translate("generated.91fbe4c855fbe075")}</span>
              <span>{status.jitterMs.toFixed(1)} {translate("generated.f785c3ce1d580c8f")}</span>
            </div>
          )}
          {status.bufferDepthMs !== undefined && (
            <div className="flex justify-between">
              <span>{translate("generated.e44193fd2d21722a")}</span>
              <span>{status.bufferDepthMs.toFixed(0)} {translate("generated.f785c3ce1d580c8f")}</span>
            </div>
          )}
          <div className="flex justify-between">
            <span>{translate("generated.9cf2f743c69f622f")}</span>
            <span>{status.ringFillMs.toFixed(1)} {translate("generated.f785c3ce1d580c8f")}</span>
          </div>
          <div className="flex justify-between">
            <span>{translate("generated.68c8c1ca29e319ec")}</span>
            <span>{status.appsrcQueueMs.toFixed(1)} {translate("generated.f785c3ce1d580c8f")}</span>
          </div>
          {status.bitrateKbps && (
            <div className="flex justify-between">
              <span>{translate("generated.0b2b7f69b8eff531")}</span>
              <span>{status.bitrateKbps} {translate("generated.1f01791cf8751512")}</span>
            </div>
          )}
          {status.reconnectCount > 0 && (
            <div className="flex justify-between">
              <span>{translate("generated.cabc4d6f1b94b4a2")}</span>
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
