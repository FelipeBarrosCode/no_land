import { useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  moonlightDisconnectStream,
  moonlightGetActiveInputMode,
  moonlightGetInputDebugState,
  moonlightGetSessionState,
} from "../../lib/backend";

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
      return "relative";
    case 2:
      return "absolute";
    default:
      return "none";
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
  const [disconnecting, setDisconnecting] = useState(false);
  const [disconnectError, setDisconnectError] = useState<string | null>(null);
  const [showHud, setShowHud] = useState(true);
  const teardownRequestedRef = useRef(false);
  const hasSeenActiveSessionRef = useRef(false);

  useEffect(() => {
    document.documentElement.classList.add("stream-window");
    document.body.classList.add("stream-window");

    void moonlightGetActiveInputMode()
      .then((mouseMode) => setPreferredMouseMode(mouseMode))
      .catch(() => setPreferredMouseMode(null));

    let cancelled = false;
    const closeStreamWindow = async () => {
      teardownRequestedRef.current = true;
      try {
        await getCurrentWindow().close();
      } catch {
        window.close();
      }
    };

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
      if (!teardownRequestedRef.current) {
        teardownRequestedRef.current = true;
        void moonlightDisconnectStream().catch(() => undefined);
      }
      document.documentElement.classList.remove("stream-window");
      document.body.classList.remove("stream-window");
    };
  }, []);

  const captureHint = useMemo(() => {
    if (preferredMouseMode === "absolute") {
      return "Native stream window active — click stream to capture desktop mouse · Ctrl+Alt+Shift+Z to release";
    }
    if (preferredMouseMode === "relative") {
      return "Native stream window active — click stream to capture relative mouse · Ctrl+Alt+Shift+Z to release";
    }
    return "Native stream window active — click stream to capture · Ctrl+Alt+Shift+Z to release";
  }, [preferredMouseMode]);

  const detail = useMemo(() => {
    if (preferredMouseMode === "absolute") {
      return "Native macOS stream view owns input. WebView forwarding is disabled for the normal path.";
    }
    if (preferredMouseMode === "relative") {
      return "Native macOS stream view owns relative mouse and keyboard capture. WebView forwarding is disabled for the normal path.";
    }
    return "Native macOS stream view owns stream input. WebView forwarding is disabled for the normal path.";
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

  return (
    <main className="relative h-screen w-screen overflow-hidden bg-transparent text-white">
      <div className="pointer-events-none absolute inset-0 select-none">
        {showHud ? (
          <div className="absolute inset-x-0 top-0 flex justify-center p-4">
            <div className="rounded border border-cyan-300/70 bg-slate-950/70 px-4 py-2 font-mono text-sm shadow-[0_0_18px_rgba(34,211,238,0.25)] backdrop-blur-sm">
              {captureHint}
            </div>
          </div>
        ) : null}

        <div className="pointer-events-auto absolute right-4 top-4 flex flex-col items-end gap-2">
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => setShowHud((value) => !value)}
              className="rounded border border-cyan-300/70 bg-slate-950/80 px-4 py-2 font-mono text-sm text-cyan-100 shadow-[0_0_18px_rgba(34,211,238,0.18)] backdrop-blur-sm transition hover:bg-slate-900/90"
            >
              {showHud ? "Hide HUD" : "Show HUD"}
            </button>
            <button
              type="button"
              onClick={() => {
                void handleDisconnectStream();
              }}
              disabled={disconnecting}
              className="rounded border border-amber-300/70 bg-slate-950/80 px-4 py-2 font-mono text-sm text-amber-100 shadow-[0_0_18px_rgba(251,191,36,0.18)] backdrop-blur-sm transition hover:bg-slate-900/90 disabled:cursor-wait disabled:opacity-70"
            >
              {disconnecting ? "Ending stream…" : "End stream"}
            </button>
          </div>
          {disconnectError ? (
            <div className="max-w-md rounded border border-red-400/70 bg-red-950/80 px-3 py-2 font-mono text-xs text-red-100 shadow-[0_0_18px_rgba(248,113,113,0.18)] backdrop-blur-sm">
              {disconnectError}
            </div>
          ) : null}
        </div>

        {showHud ? (
          <div className="absolute bottom-4 right-4 max-w-lg rounded border border-slate-700/80 bg-slate-950/65 px-3 py-2 font-mono text-xs text-slate-100 shadow-[0_0_18px_rgba(15,23,42,0.35)] backdrop-blur-sm">
            <div>{detail}</div>
            <div className="mt-1 text-slate-300">
              Click the stream window itself to enter capture
            </div>
            <div className="mt-1 text-slate-400">
              Ctrl+Alt+Shift+Z releases capture · Ctrl+Alt+Shift+Q remains a compatibility alias
            </div>
            <div className="mt-1 text-slate-400">
              Use End stream if audio/video gets into a bad state, then start the session again from the main app.
            </div>

            <div className="mt-3 border-t border-slate-700/80 pt-2 text-[11px] leading-5 text-cyan-100">
              <div>
                capture: {debugState.captureActive ? "active" : "inactive"} ({captureModeLabel(debugState.captureMode)}) · requests: {debugState.captureRequests}
              </div>
              <div>
                native events: move={debugState.nativeMouseMoves} down={debugState.nativeMouseDowns} up={debugState.nativeMouseUps} key={debugState.nativeKeys}
              </div>
              <div>
                rust callbacks: rel={debugState.rustRelativeCallbacks} abs={debugState.rustAbsoluteCallbacks} btn={debugState.rustButtonCallbacks} key={debugState.rustKeyCallbacks}
              </div>
              <div>
                send attempts: rel={debugState.relativeSendAttempts} abs={debugState.absoluteSendAttempts} btn={debugState.buttonSendAttempts} key={debugState.keySendAttempts} scroll={debugState.scrollSendAttempts}
              </div>
              <div>
                send errors: {debugState.sendErrors}
              </div>
            </div>
          </div>
        ) : null}
      </div>
    </main>
  );
}
