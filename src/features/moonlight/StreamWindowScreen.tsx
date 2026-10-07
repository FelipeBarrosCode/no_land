import { translate } from "../../lib/i18n";
import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import {
  moonlightDisconnectStream,
  moonlightGetClipboardFromRemote,
  moonlightGetActiveInputMode,
  moonlightGetInputDebugState,
  moonlightGetSessionState,
  moonlightSendClipboardToRemote,
} from "../../lib/backend";
import {
  networkWarningBody,
  notifyBadConnection,
  type NetworkStatusEvent,
} from "../../lib/networkNotifications";

type LatencyStatistics = {
  state: string;
  streamFps: number;
  clientRefreshRateX100: number;
  configuredPacingMode: string;
  effectivePacingMode: string;
  videoPacketsInterval: number;
  fecPacketsInterval: number;
  fecRecoveriesInterval: number;
  fecFailuresInterval: number;
  outOfSequencePacketsInterval: number;
  invalidPacketsInterval: number;
  invalidFecPacketsInterval: number;
  pendingCoreVideoFrames: number;
  decoderQueueDepth: number;
  renderQueueDepth: number;
  averageDecodePipelineUs: number;
  averageRenderQueueDwellUs: number;
  lateFrameCount: number;
  adaptiveStaleDropCount: number;
  pacerBacklogDropCount: number;
  maximumLatenessUs: number;
  decoderBackpressureTimeUs: number;
  decoderBackpressured: boolean;
  renderedFpsX100: number;
  smoothingQueueDepth: number;
  smoothingQueueCapacity: number;
  smoothingOverflowDrops: number;
  smoothingUnderflowRepeats: number;
  smoothingReserveBudgetUs: number;
  frameTimingRingCount: number;
  reconnectAttemptCount: number;
  reconnectSuccessCount: number;
  resolvedRemoteStreamMode: string;
  requestedPacketSize: number;
  adaptivePacketSizeEnabled: boolean;
  packetSizeControllerState: string;
  packetPathLabel: string;
  packetPathMtuHint: number | null;
  packetSizeLastGood: number | null;
  packetSizeBadWindowCount: number;
  packetSizeConfidence: number;
  packetPathFingerprint: string;
  adaptivePacketReconnectCount: number;
};


type DebugState = {
  captureActive: boolean;
  captureMode: number;
  captureRequests: number;
  nativeMouseMoves: number;
  nativeMouseDowns: number;
  nativeMouseUps: number;
  nativeKeys: number;
  rustRelativeCallbacks: number;
  rustAbsoluteCallbacks: number;
  rustButtonCallbacks: number;
  rustKeyCallbacks: number;
  relativeSendAttempts: number;
  absoluteSendAttempts: number;
  buttonSendAttempts: number;
  keySendAttempts: number;
  scrollSendAttempts: number;
  sendErrors: number;
};

const EMPTY_DEBUG: DebugState = {
  captureActive: false,
  captureMode: 0,
  captureRequests: 0,
  nativeMouseMoves: 0,
  nativeMouseDowns: 0,
  nativeMouseUps: 0,
  nativeKeys: 0,
  rustRelativeCallbacks: 0,
  rustAbsoluteCallbacks: 0,
  rustButtonCallbacks: 0,
  rustKeyCallbacks: 0,
  relativeSendAttempts: 0,
  absoluteSendAttempts: 0,
  buttonSendAttempts: 0,
  keySendAttempts: 0,
  scrollSendAttempts: 0,
  sendErrors: 0,
};

function captureModeLabel(mode: number): string {
  switch (mode) {
    case 1:
      return translate("generated.d2d9e1f13413d3e0");
    case 2:
      return translate("generated.747355bdc2a22403");
    default:
      return translate("generated.140bedbf9c3f6d56");
  }
}


function isActiveSessionState(state: string | null): boolean {
  return (
    state === "preparing" ||
    state === "launching" ||
    state === "creating_surface" ||
    state === "connecting" ||
    state === "streaming" ||
    state === "reconnecting" ||
    state === "stopping"
  );
}

export function StreamWindowScreen() {
  const [preferredMouseMode, setPreferredMouseMode] = useState<
    "relative" | "absolute" | null
  >(null);
  const [debugState, setDebugState] = useState<DebugState>(EMPTY_DEBUG);
  const [latencyStats, setLatencyStats] = useState<LatencyStatistics | null>(null);
  const [disconnecting, setDisconnecting] = useState(false);
  const [disconnectError, setDisconnectError] = useState<string | null>(null);
  const [clipboardBusy, setClipboardBusy] = useState<"send" | "get" | null>(null);
  const [clipboardStatus, setClipboardStatus] = useState<string | null>(null);
  const [showHud, setShowHud] = useState(true);
  const [networkWarning, setNetworkWarning] = useState<NetworkStatusEvent | null>(null);
  const networkWarningTimeoutRef = useRef<number | null>(null);
  const networkBadEpisodeRef = useRef(false);
  const teardownRequestedRef = useRef(false);
  const allowWindowCloseRef = useRef(false);
  const hasSeenActiveSessionRef = useRef(false);

  useEffect(() => {
    document.documentElement.classList.add("stream-window");
    document.body.classList.add("stream-window");

    void moonlightGetActiveInputMode()
      .then((mouseMode) => setPreferredMouseMode(mouseMode))
      .catch(() => setPreferredMouseMode(null));

    let cancelled = false;
    const appWindow = getCurrentWindow();
    const closeStreamWindow = async () => {
      teardownRequestedRef.current = true;
      allowWindowCloseRef.current = true;
      try {
        await appWindow.close();
      } catch {
        window.close();
      }
    };

    const unlistenStatsPromise = listen<LatencyStatistics>(
      "moonlight://statistics",
      ({ payload }) => {
        if (!cancelled) {
          setLatencyStats(payload);
        }
      },
    );

    const unlistenCloseRequestedPromise = appWindow.onCloseRequested(
      async (event) => {
        if (allowWindowCloseRef.current) {
          return;
        }
        event.preventDefault();
        if (teardownRequestedRef.current) {
          return;
        }

        teardownRequestedRef.current = true;
        setDisconnecting(true);
        setDisconnectError(null);

        try {
          await moonlightDisconnectStream();
          allowWindowCloseRef.current = true;
          try {
            await appWindow.close();
          } catch {
            window.close();
          }
        } catch (error) {
          teardownRequestedRef.current = false;
          setDisconnecting(false);
          const message = error instanceof Error ? error.message : String(error);
          setDisconnectError(message || "Failed to end stream session");
        }
      },
    );

    const poll = async () => {
      try {
        const [nextDebug, session] = await Promise.all([
          moonlightGetInputDebugState(),
          moonlightGetSessionState(),
        ]);
        if (cancelled) {
          return;
        }
        setDebugState(nextDebug);
        if (isActiveSessionState(session.state)) {
          hasSeenActiveSessionRef.current = true;
          return;
        }
        if (hasSeenActiveSessionRef.current && session.state === "idle") {
          void closeStreamWindow();
        }
      } catch {
        // ignore polling errors while debugging
      }
    };

    void poll();
    const interval = window.setInterval(() => {
      void poll();
    }, 250);

    return () => {
      cancelled = true;
      window.clearInterval(interval);
      void unlistenCloseRequestedPromise.then((unlisten) => unlisten());
      void unlistenStatsPromise.then((unlisten) => unlisten());
      if (!teardownRequestedRef.current) {
        teardownRequestedRef.current = true;
        void moonlightDisconnectStream().catch(() => undefined);
      }
      document.documentElement.classList.remove("stream-window");
      document.body.classList.remove("stream-window");
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const unlistenStatusPromise = listen<NetworkStatusEvent>(
      "network-monitor://status",
      ({ payload }) => {
        if (cancelled) {
          return;
        }
        if (payload.current !== "BAD") {
          networkBadEpisodeRef.current = false;
          if (networkWarningTimeoutRef.current != null) {
            window.clearTimeout(networkWarningTimeoutRef.current);
            networkWarningTimeoutRef.current = null;
          }
          setNetworkWarning(null);
          return;
        }
        setNetworkWarning(payload);
        if (!payload.alertEligible || networkBadEpisodeRef.current) {
          return;
        }
        networkBadEpisodeRef.current = true;
        void notifyBadConnection(payload);
        if (networkWarningTimeoutRef.current != null) {
          window.clearTimeout(networkWarningTimeoutRef.current);
        }
        networkWarningTimeoutRef.current = window.setTimeout(() => {
          networkWarningTimeoutRef.current = null;
          setNetworkWarning(null);
        }, 8_000);

      },
    );

    return () => {
      cancelled = true;
      if (networkWarningTimeoutRef.current != null) {
        window.clearTimeout(networkWarningTimeoutRef.current);
        networkWarningTimeoutRef.current = null;
      }
      void unlistenStatusPromise.then((unlisten) => unlisten());
    };
  }, []);

  const captureHint = useMemo(() => {
    if (preferredMouseMode === "absolute") {
      return translate("generated.3f80a4810bcc5dfa");
    }
    if (preferredMouseMode === "relative") {
      return translate("generated.2aba47a8a332de64");
    }
    return translate("generated.121453cf700c2ef9");
  }, [preferredMouseMode]);

  const detail = useMemo(() => {
    if (preferredMouseMode === "absolute") {
      return translate("generated.6aa8acf82d2aec6f");
    }
    if (preferredMouseMode === "relative") {
      return translate("generated.a06e6bc5e72c99cb");
    }
    return translate("generated.34339a32584c67b4");
  }, [preferredMouseMode]);

  const handleDisconnectStream = async () => {
    if (disconnecting) {
      return;
    }

    teardownRequestedRef.current = true;
    setDisconnecting(true);
    setDisconnectError(null);
    try {
      await moonlightDisconnectStream();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setDisconnectError(message || "Failed to end stream session");
      setDisconnecting(false);
    }
  };

  const handleClipboard = async (direction: "send" | "get") => {
    if (clipboardBusy) {
      return;
    }
    setClipboardBusy(direction);
    setClipboardStatus(null);
    try {
      const result = direction === "send"
        ? await moonlightSendClipboardToRemote()
        : await moonlightGetClipboardFromRemote();
      setClipboardStatus(
        translate(direction === "send" ? "stream.clipboard.sent" : "stream.clipboard.received", {
          count: result.byteCount,
        }),
      );
    } catch (error) {
      setClipboardStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setClipboardBusy(null);
    }
  };

  return (
    <main className="relative h-dvh w-full overflow-hidden bg-transparent text-white">
      <div className="pointer-events-none absolute inset-0 select-none">
        {showHud ? (
          <div className="absolute inset-x-0 top-0 flex justify-center p-4">
            <div className="rounded border border-cyan-300/70 bg-slate-950/70 px-4 py-2 font-mono text-sm shadow-[0_0_18px_rgba(34,211,238,0.25)] backdrop-blur-sm">
              {captureHint}
            </div>
          </div>
        ) : null}

        {networkWarning ? (
          <div className="absolute inset-x-0 top-20 flex justify-center px-4">
            <div className="max-w-lg rounded border border-amber-300/80 bg-amber-950/90 px-5 py-4 font-mono text-amber-50 shadow-[0_0_24px_rgba(251,191,36,0.3)] backdrop-blur-sm">
              <div className="text-sm font-semibold uppercase tracking-[0.12em]">
                {translate("generated.d6f0569777748206")}
              </div>
              <div className="mt-2 text-xs leading-5 text-amber-100">
                {networkWarningBody(networkWarning)}
              </div>
              {networkWarning.keyMetrics ? (
                <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-amber-200">
                  {networkWarning.keyMetrics.medianRttMs != null ? (
                    <span>{translate("generated.758d61f26a444483")} {networkWarning.keyMetrics.medianRttMs.toFixed(1)} {translate("generated.f785c3ce1d580c8f")}</span>
                  ) : null}
                  <span>{translate("generated.16c7dc721bc2a15a")} {(networkWarning.keyMetrics.jitterMs ?? 0).toFixed(1)} {translate("generated.f785c3ce1d580c8f")}</span>
                  <span>{translate("generated.2ea71c18131a7f03")} {(networkWarning.keyMetrics.lossPercent ?? 0).toFixed(1)}%</span>
                </div>
              ) : null}
            </div>
          </div>
        ) : null}

        <div className="pointer-events-auto absolute right-4 top-4 flex flex-col items-end gap-2">
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => void handleClipboard("send")}
              disabled={clipboardBusy !== null || disconnecting}
              className="rounded border border-violet-300/70 bg-slate-950/80 px-4 py-2 font-mono text-sm text-violet-100 shadow-[0_0_18px_rgba(196,181,253,0.18)] backdrop-blur-sm transition hover:bg-slate-900/90 disabled:cursor-wait disabled:opacity-70"
            >
              {clipboardBusy === "send" ? translate("generated.b8ed5279e897be5d") : translate("generated.639d984d9ffc0361")}
            </button>
            <button
              type="button"
              onClick={() => void handleClipboard("get")}
              disabled={clipboardBusy !== null || disconnecting}
              className="rounded border border-violet-300/70 bg-slate-950/80 px-4 py-2 font-mono text-sm text-violet-100 shadow-[0_0_18px_rgba(196,181,253,0.18)] backdrop-blur-sm transition hover:bg-slate-900/90 disabled:cursor-wait disabled:opacity-70"
            >
              {clipboardBusy === "get" ? translate("generated.eaa6c37a1811815b") : translate("generated.9d060e4dd135b141")}
            </button>
            <button
              type="button"
              onClick={() => setShowHud((value) => !value)}
              className="rounded border border-cyan-300/70 bg-slate-950/80 px-4 py-2 font-mono text-sm text-cyan-100 shadow-[0_0_18px_rgba(34,211,238,0.18)] backdrop-blur-sm transition hover:bg-slate-900/90"
            >
              {showHud ? translate("generated.9af4b195a28a6dd4") : translate("generated.a3980e524819578a")}
            </button>
            <button
              type="button"
              onClick={() => {
                void handleDisconnectStream();
              }}
              disabled={disconnecting}
              className="rounded border border-amber-300/70 bg-slate-950/80 px-4 py-2 font-mono text-sm text-amber-100 shadow-[0_0_18px_rgba(251,191,36,0.18)] backdrop-blur-sm transition hover:bg-slate-900/90 disabled:cursor-wait disabled:opacity-70"
            >
              {disconnecting ? translate("generated.6feadc6d3b71fbcc") : translate("generated.23c13af33709b5e3")}
            </button>
          </div>
          {disconnectError ? (
            <div className="max-w-md rounded border border-red-400/70 bg-red-950/80 px-3 py-2 font-mono text-xs text-red-100 shadow-[0_0_18px_rgba(248,113,113,0.18)] backdrop-blur-sm">
              {disconnectError}
            </div>
          ) : null}
          {clipboardStatus ? (
            <div className="max-w-md rounded border border-violet-300/70 bg-slate-950/80 px-3 py-2 font-mono text-xs text-violet-100 shadow-[0_0_18px_rgba(196,181,253,0.18)] backdrop-blur-sm">
              {clipboardStatus}
            </div>
          ) : null}
        </div>

        {showHud && latencyStats && (latencyStats.frameTimingRingCount > 0 || latencyStats.adaptivePacketSizeEnabled) ? (
          <div className="absolute bottom-4 left-4 max-w-md rounded border border-emerald-400/60 bg-slate-950/70 px-3 py-2 font-mono text-[11px] leading-5 text-emerald-50 shadow-[0_0_18px_rgba(52,211,153,0.18)] backdrop-blur-sm">
            <div>
              {translate("generated.887270d0cbc560af")} {(latencyStats.renderedFpsX100 / 100).toFixed(1)} {translate("generated.fe131b2fea5626c8")} {latencyStats.streamFps} {translate("generated.ae45f5198135a65b")} {(latencyStats.clientRefreshRateX100 / 100).toFixed(2)} {translate("generated.987d2c1d9216b34f")}
            </div>
            <div>
              {translate("generated.7bfef0ad1b273d14")} {latencyStats.effectivePacingMode} {translate("generated.3ac452a0b121b531")} {latencyStats.configuredPacingMode})
            </div>
            <div>
              {translate("generated.017964127e8f37cb")}{latencyStats.pendingCoreVideoFrames} {translate("generated.48482d640fea00e5")}{latencyStats.decoderQueueDepth} {translate("generated.972409a7bfd12f9e")}{latencyStats.renderQueueDepth}
            </div>
            <div>
              {translate("generated.713a1417c769dc75")} {(latencyStats.averageDecodePipelineUs / 1000).toFixed(2)} {translate("generated.43afaea4575e0f3e")} {(latencyStats.averageRenderQueueDwellUs / 1000).toFixed(2)} {translate("generated.f785c3ce1d580c8f")}
            </div>
            <div>
              {translate("generated.f8c4379da2505d21")} {latencyStats.videoPacketsInterval} {translate("generated.26fc7d3ffe801152")}{latencyStats.fecPacketsInterval} {translate("generated.7a201874276741b8")}{latencyStats.fecRecoveriesInterval} {translate("generated.c1b4613b8a026a3c")}{latencyStats.fecFailuresInterval} {translate("generated.ba753f2339339812")}{latencyStats.outOfSequencePacketsInterval} {translate("generated.4a08ee798120df9c")}{latencyStats.invalidPacketsInterval}/{latencyStats.invalidFecPacketsInterval}
            </div>
            <div>
              {translate("generated.f006b6812e1f7b90")}{latencyStats.lateFrameCount} {translate("generated.c0e324810b9b3d4f")}{latencyStats.adaptiveStaleDropCount} {translate("generated.a36d2f345c55e08e")}{latencyStats.pacerBacklogDropCount} {translate("generated.c786bfd89235078f")}{(latencyStats.maximumLatenessUs / 1000).toFixed(2)} {translate("generated.f785c3ce1d580c8f")}
            </div>
            <div>
              {translate("generated.6a06ac7024efc018")} {latencyStats.decoderBackpressured ? translate("generated.96879611650f80a8") : translate("generated.4fb62348858c2f6f")} {translate("generated.6831906b60c54d01")} {(latencyStats.decoderBackpressureTimeUs / 1000).toFixed(1)} {translate("generated.f785c3ce1d580c8f")}
            </div>
            <div>
              {translate("generated.3637a614cb738f81")} {latencyStats.smoothingQueueDepth}/{latencyStats.smoothingQueueCapacity} {translate("generated.8a113c449d88dd3b")} {(latencyStats.smoothingReserveBudgetUs / 1000).toFixed(1)} {translate("generated.55aa8179210d5963")}{latencyStats.smoothingOverflowDrops} {translate("generated.6d4ba7ac63a8c0a3")}{latencyStats.smoothingUnderflowRepeats}
            </div>
            <div>
              {translate("generated.91669c43679f4342")} {latencyStats.reconnectSuccessCount}/{latencyStats.reconnectAttemptCount} {translate("generated.397f813b7b2e8f16")}{latencyStats.adaptivePacketReconnectCount} {translate("generated.e69c7ec749a66341")}{latencyStats.resolvedRemoteStreamMode} {translate("generated.1bfee5cf707e1cbd")}{latencyStats.requestedPacketSize}
            </div>
            {latencyStats.adaptivePacketSizeEnabled ? (
              <div>
                {translate("generated.8010851385091b52")}{latencyStats.packetSizeControllerState} {translate("generated.f23ab60eeb1d9247")}{latencyStats.packetPathLabel} {translate("generated.73555efb472501a6")}{latencyStats.packetPathMtuHint ?? "unknown"} {translate("generated.2395ff0b33c72fd0")}{latencyStats.packetSizeLastGood ?? "none"} {translate("generated.d495f5384e8b9784")}{latencyStats.packetSizeBadWindowCount}{translate("generated.4b63af8c396475c2")}{(latencyStats.packetSizeConfidence * 100).toFixed(0)}{translate("generated.fc48f5a69120449a")}{latencyStats.packetPathFingerprint.slice(0, 8)}
              </div>
            ) : null}
          </div>
        ) : null}

        {showHud ? (
          <div className="absolute bottom-4 right-4 max-w-lg rounded border border-slate-700/80 bg-slate-950/65 px-3 py-2 font-mono text-xs text-slate-100 shadow-[0_0_18px_rgba(15,23,42,0.35)] backdrop-blur-sm">
            <div>{detail}</div>
            <div className="mt-1 text-slate-300">
              {translate("generated.9d00ac2a5bb863a5")}
            </div>
            <div className="mt-1 text-slate-400">
              {translate("generated.13722278f53c8e51")}
            </div>
            <div className="mt-1 text-slate-400">
              {translate("generated.682c736f3810d7be")}
            </div>

            <div className="mt-3 border-t border-slate-700/80 pt-2 text-[11px] leading-5 text-cyan-100">
              <div>
                {translate("generated.6516af620c324a03")} {debugState.captureActive ? translate("generated.96879611650f80a8") : translate("generated.d1022618b99a974b")} ({captureModeLabel(debugState.captureMode)}{translate("generated.a82c11cd2e358ef0")} {debugState.captureRequests}
              </div>
              <div>
                {translate("generated.bd1825d7d0c9ca5e")}{debugState.nativeMouseMoves} {translate("generated.2410128cd7f0bb44")}{debugState.nativeMouseDowns} {translate("generated.2a600496b53f8060")}{debugState.nativeMouseUps} {translate("generated.a61a3f400a99f825")}{debugState.nativeKeys}
              </div>
              <div>
                {translate("generated.c7b3b4255dc33778")}{debugState.rustRelativeCallbacks} {translate("generated.1e44e918f93acc37")}{debugState.rustAbsoluteCallbacks} {translate("generated.68f5c2e560ba7c7c")}{debugState.rustButtonCallbacks} {translate("generated.a61a3f400a99f825")}{debugState.rustKeyCallbacks}
              </div>
              <div>
                {translate("generated.a172c95553d0ee21")}{debugState.relativeSendAttempts} {translate("generated.1e44e918f93acc37")}{debugState.absoluteSendAttempts} {translate("generated.68f5c2e560ba7c7c")}{debugState.buttonSendAttempts} {translate("generated.a61a3f400a99f825")}{debugState.keySendAttempts} {translate("generated.89a7d32040600cd2")}{debugState.scrollSendAttempts}
              </div>
              <div>
                {translate("generated.d2e4dda7b6298cef")} {debugState.sendErrors}
              </div>
            </div>
          </div>
        ) : null}
      </div>
    </main>
  );
}
