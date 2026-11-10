import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { Button } from "../../components/ui/Button";
import { useLocalization, translate } from "../../lib/i18n";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import {
  closeRemoteTerminal,
  openRemoteTerminal,
  resizeRemoteTerminal,
  writeRemoteTerminal,
} from "../../lib/backend";
import type { RentedInstanceSummary } from "../../lib/types";

interface TerminalOutputEvent {
  sessionId: string;
  data: string;
}

interface TerminalClosedEvent {
  sessionId: string;
  exitCode?: number | null;
}

interface Props {
  instance: RentedInstanceSummary | null;
  onClose: () => void;
}

export function InstanceTerminalModal({ instance, onClose }: Props) {
  const { t } = useLocalization();
  const hostRef = useRef<HTMLDivElement>(null);
  const instanceId = instance?.instanceId;
  const sshHost = instance?.sshHost;
  const sshPort = instance?.sshPort;
  const [status, setStatus] = useState<"connecting" | "connected" | "closed" | "error">(
    instance ? "connecting" : "error",
  );

  useEffect(() => {
    if (instanceId === undefined || !hostRef.current) return;

    let disposed = false;
    let sessionId: string | null = null;
    let opening = true;
    setStatus("connecting");
    const earlyOutput: TerminalOutputEvent[] = [];
    const earlyClosed: TerminalClosedEvent[] = [];
    const terminal = new Terminal({
      cursorBlink: true,
      cursorStyle: "block",
      convertEol: true,
      fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
      fontSize: 14,
      lineHeight: 1.18,
      scrollback: 5000,
      theme: {
        background: "#080b18",
        foreground: "#d7e7ff",
        cursor: "#7bff48",
        cursorAccent: "#080b18",
        selectionBackground: "#274b68",
        black: "#080b18",
        brightBlack: "#7183ac",
        red: "#ff8ca2",
        brightRed: "#ffb1c0",
        green: "#7bff48",
        brightGreen: "#b4ff88",
        yellow: "#ffd166",
        brightYellow: "#ffe59a",
        blue: "#61a8ff",
        brightBlue: "#9acbff",
        magenta: "#d58cff",
        brightMagenta: "#e9baff",
        cyan: "#61f7ff",
        brightCyan: "#a7fbff",
        white: "#d7e7ff",
        brightWhite: "#ffffff",
      },
    });
    const fitAddon = new FitAddon();
    terminal.loadAddon(fitAddon);
    terminal.open(hostRef.current);
    fitAddon.fit();
    terminal.writeln(`\x1b[36m${translate("terminal.banner")}\x1b[0m`);
    terminal.writeln(`${translate("terminal.connecting", { host: sshHost ?? "", port: sshPort ?? 22 })}\r\n`);

    const resizeObserver = new ResizeObserver(() => {
      fitAddon.fit();
      if (sessionId) void resizeRemoteTerminal(sessionId, terminal.rows, terminal.cols);
    });
    resizeObserver.observe(hostRef.current);
    const inputDisposable = terminal.onData((data) => {
      if (sessionId) void writeRemoteTerminal(sessionId, data);
    });

    let removeOutputListener: (() => void) | undefined;
    let removeClosedListener: (() => void) | undefined;
    const handleClosed = (payload: TerminalClosedEvent) => {
      if (typeof payload.exitCode === "number" && payload.exitCode !== 0) {
        setStatus("error");
        terminal.writeln(`\r\n\x1b[31m${translate("terminal.ssh.exit", { code: payload.exitCode })}\x1b[0m`);
      } else {
        setStatus("closed");
        terminal.writeln(`\r\n\x1b[33m${translate("terminal.ssh.closed")}\x1b[0m`);
      }
      sessionId = null;
    };

    // Subscribe before spawning SSH, including failures that exit before invoke resolves.
    void (async () => {
      removeOutputListener = await listen<TerminalOutputEvent>("remote-terminal-output", ({ payload }) => {
        if (disposed) return;
        if (!sessionId && opening) {
          earlyOutput.push(payload);
        } else if (payload.sessionId === sessionId) {
          setStatus((current) => current === "connecting" ? "connected" : current);
          terminal.write(payload.data);
        }
      });
      if (disposed) {
        removeOutputListener();
        return;
      }
      removeClosedListener = await listen<TerminalClosedEvent>("remote-terminal-closed", ({ payload }) => {
        if (disposed) return;
        if (!sessionId && opening) earlyClosed.push(payload);
        else if (payload.sessionId === sessionId) handleClosed(payload);
      });
      if (disposed) {
        removeClosedListener();
        return;
      }
      const session = await openRemoteTerminal(instanceId);
      opening = false;
      if (disposed) {
        void closeRemoteTerminal(session.sessionId);
        return;
      }
      sessionId = session.sessionId;
      fitAddon.fit();
      void resizeRemoteTerminal(session.sessionId, terminal.rows, terminal.cols);
      for (const output of earlyOutput) {
        if (output.sessionId === session.sessionId) terminal.write(output.data);
      }
      if (earlyOutput.some((output) => output.sessionId === session.sessionId)) {
        setStatus("connected");
      }
      const closed = earlyClosed.find((event) => event.sessionId === session.sessionId);
      earlyOutput.length = 0;
      earlyClosed.length = 0;
      if (closed) handleClosed(closed);
      terminal.focus();
    })().catch((error) => {
      opening = false;
      if (disposed) return;
      removeOutputListener?.();
      removeClosedListener?.();
      setStatus("error");
      terminal.writeln(`\r\n\x1b[31m${error instanceof Error ? error.message : String(error)}\x1b[0m`);
    });

    return () => {
      disposed = true;
      if (sessionId) void closeRemoteTerminal(sessionId);
      sessionId = null;
      removeOutputListener?.();
      removeClosedListener?.();
      inputDisposable.dispose();
      resizeObserver.disconnect();
      terminal.dispose();
    };
  }, [instanceId, sshHost, sshPort]);

  return (
    <ModalFrame panelClassName="glass-panel pixel-frame max-w-5xl">
      <ModalBody className="p-0">
        <div className="flex items-center justify-between gap-3 border-b border-[#3e4270] px-4 py-3">
          <div>
            <h2 className="font-display text-sm uppercase tracking-[0.16em] text-white">{t("terminal.title")}</h2>
            <p className="mt-1 text-xs text-[#bfd3ee]">
              {instance ? `${instance.label} · ${instance.sshHost}:${instance.sshPort}` : translate("generated.34705857a088051e")}
            </p>
          </div>
          <div className="flex items-center gap-3">
            <span className={`text-[10px] uppercase tracking-wider ${status === "connected" ? "text-neon-lime" : status === "error" ? "text-[#ff8ca2]" : "text-[#ffd166]"}`}>
              {translate(`terminal.status.${status}`)}
            </span>
            <Button variant="ghost" onClick={onClose}>{t("common.close")}</Button>
          </div>
        </div>
        <div className="bg-[#080b18] p-2">
          <div ref={hostRef} className="h-[min(65dvh,38rem)] w-full overflow-hidden" aria-label={t("terminal.accessibility")} />
        </div>
      </ModalBody>
    </ModalFrame>
  );
}
