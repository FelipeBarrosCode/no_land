import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { Button } from "../../components/ui/Button";
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
  const hostRef = useRef<HTMLDivElement>(null);
  const sessionIdRef = useRef<string | null>(null);
  const [status, setStatus] = useState<"connecting" | "connected" | "closed" | "error">(
    instance ? "connecting" : "error",
  );

  useEffect(() => {
    if (!instance || !hostRef.current) return;

    let disposed = false;
    const earlyOutput: TerminalOutputEvent[] = [];
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
    terminal.writeln("\x1b[36mNOLAND REMOTE TERMINAL\x1b[0m");
    terminal.writeln(`Connecting to ${instance.sshHost}:${instance.sshPort}...\r\n`);

    const resizeObserver = new ResizeObserver(() => {
      fitAddon.fit();
      const sessionId = sessionIdRef.current;
      if (sessionId) void resizeRemoteTerminal(sessionId, terminal.rows, terminal.cols);
    });
    resizeObserver.observe(hostRef.current);
    const inputDisposable = terminal.onData((data) => {
      const sessionId = sessionIdRef.current;
      if (sessionId) void writeRemoteTerminal(sessionId, data);
    });

    let removeOutputListener: (() => void) | undefined;
    let removeClosedListener: (() => void) | undefined;
    void Promise.all([
      listen<TerminalOutputEvent>("remote-terminal-output", ({ payload }) => {
        const sessionId = sessionIdRef.current;
        if (!sessionId) {
          earlyOutput.push(payload);
        } else if (payload.sessionId === sessionId) {
          setStatus((current) => current === "connecting" ? "connected" : current);
          terminal.write(payload.data);
        }
      }),
      listen<TerminalClosedEvent>("remote-terminal-closed", ({ payload }) => {
        if (payload.sessionId !== sessionIdRef.current) return;

        if (typeof payload.exitCode === "number" && payload.exitCode !== 0) {
          setStatus("error");
          terminal.writeln(`\r\n\x1b[31mSSH exited with code ${payload.exitCode}. Check the host, port, and SSH key, then refresh the instance connection details.\x1b[0m`);
        } else {
          setStatus("closed");
          terminal.writeln("\r\n\x1b[33mSSH connection closed.\x1b[0m");
        }
      }),
    ]).then(([removeOutput, removeClosed]) => {
      if (disposed) {
        removeOutput();
        removeClosed();
      } else {
        removeOutputListener = removeOutput;
        removeClosedListener = removeClosed;
      }
    });

    void openRemoteTerminal(instance.instanceId)
      .then((session) => {
        if (disposed) {
          void closeRemoteTerminal(session.sessionId);
          return;
        }
        sessionIdRef.current = session.sessionId;
        fitAddon.fit();
        void resizeRemoteTerminal(session.sessionId, terminal.rows, terminal.cols);
        for (const output of earlyOutput) {
          if (output.sessionId === session.sessionId) terminal.write(output.data);
        }
        if (earlyOutput.some((output) => output.sessionId === session.sessionId)) {
          setStatus("connected");
        }
        terminal.focus();
      })
      .catch((error) => {
        if (disposed) return;
        setStatus("error");
        terminal.writeln(`\r\n\x1b[31m${error instanceof Error ? error.message : String(error)}\x1b[0m`);
      });

    return () => {
      disposed = true;
      const sessionId = sessionIdRef.current;
      sessionIdRef.current = null;
      if (sessionId) void closeRemoteTerminal(sessionId);
      removeOutputListener?.();
      removeClosedListener?.();
      inputDisposable.dispose();
      resizeObserver.disconnect();
      terminal.dispose();
    };
  }, [instance]);

  return (
    <ModalFrame panelClassName="glass-panel pixel-frame max-w-5xl">
      <ModalBody className="p-0">
        <div className="flex items-center justify-between gap-3 border-b border-[#3e4270] px-4 py-3">
          <div>
            <h2 className="font-display text-sm uppercase tracking-[0.16em] text-white">Remote Terminal</h2>
            <p className="mt-1 text-xs text-[#bfd3ee]">
              {instance ? `${instance.label} · ${instance.sshHost}:${instance.sshPort}` : "No running instance selected"}
            </p>
          </div>
          <div className="flex items-center gap-3">
            <span className={`text-[10px] uppercase tracking-wider ${status === "connected" ? "text-neon-lime" : status === "error" ? "text-[#ff8ca2]" : "text-[#ffd166]"}`}>
              {status}
            </span>
            <Button variant="ghost" onClick={onClose}>Close</Button>
          </div>
        </div>
        <div className="bg-[#080b18] p-2">
          <div ref={hostRef} className="h-[min(65dvh,38rem)] w-full overflow-hidden" aria-label="Interactive remote terminal" />
        </div>
      </ModalBody>
    </ModalFrame>
  );
}
