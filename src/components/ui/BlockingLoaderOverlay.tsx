import { useEffect, useRef, useState } from "react";
import clsx from "clsx";
import { ModalFrame } from "./ModalFrame";

export type BlockingLoaderMode = "indeterminate" | "determinate";

export interface BlockingActionState {
  key: string;
  label: string;
  detail?: string | null;
  progress?: number | null;
  startedAt: number;
  mode: BlockingLoaderMode;
  cancellable?: boolean;
  cancelRequested?: boolean;
  operationId?: string | null;
  instanceId?: number | null;
  stage?: string | null;
}

interface Props {
  action: BlockingActionState;
  inline?: boolean;
  className?: string;
  onCancel?: () => void;
  onStopProvisioning?: () => void;
  stopRequested?: boolean;
}

function formatElapsed(startedAt: number, now: number): string {
  const elapsedMs = Math.max(0, now - startedAt);
  const seconds = Math.floor(elapsedMs / 1000);
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;

  if (minutes === 0) {
    return `${remainingSeconds}s elapsed`;
  }

  return `${minutes}m ${remainingSeconds}s elapsed`;
}

export function BlockingLoaderOverlay({
  action,
  inline = false,
  className,
  onCancel,
  onStopProvisioning,
  stopRequested = false,
}: Props) {
  const [now, setNow] = useState(() => Date.now());
  const stageRef = useRef<string | null | undefined>(action.stage);
  const [stageStartedAt, setStageStartedAt] = useState(() => action.startedAt);
  const progress =
    action.mode === "determinate" && typeof action.progress === "number"
      ? Math.max(0, Math.min(100, action.progress))
      : null;

  useEffect(() => {
    const intervalId = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(intervalId);
  }, []);

  useEffect(() => {
    if (stageRef.current !== action.stage) {
      stageRef.current = action.stage;
      setStageStartedAt(Date.now());
    }
  }, [action.stage]);

  const showStopControl = action.key === "provisioning.flow";
  const showInstanceInactiveWarning =
    showStopControl &&
    action.stage === "WaitingForInstance" &&
    now - stageStartedAt >= 10 * 60 * 1000;

  const content = (
    <div
      aria-busy="true"
      aria-live="polite"
      className={clsx(
        "relative w-full p-6 text-left",
        inline
          ? "glass-panel pixel-frame max-w-none shadow-[0_0_30px_rgba(68,214,255,0.2)]"
          : "min-h-0 flex-1 overflow-y-auto",
        className
      )}
    >
      {showStopControl && onStopProvisioning && (
        <button
          type="button"
          onClick={onStopProvisioning}
          disabled={stopRequested}
          aria-label="Stop provisioning"
          title="Stop provisioning after the current step finishes"
          className="absolute right-3 top-3 flex h-8 w-8 items-center justify-center border border-[#3f476c] bg-[#10152f] text-[18px] leading-none text-[#cfe7ff] transition hover:border-[#ff8ca2] hover:text-[#ffc1cf] disabled:cursor-not-allowed disabled:opacity-50"
        >
          ×
        </button>
      )}

      <div className="flex items-start gap-4">
        <div
          aria-hidden="true"
          className="mt-1 h-10 w-10 animate-spin rounded-full border-[3px] border-[#61f7ff] border-t-transparent shadow-[0_0_14px_rgba(97,247,255,0.45)]"
        />

        <div className="min-w-0 flex-1">
          <p className="font-display text-[10px] uppercase tracking-[0.18em] text-neon-lime">
            Action In Progress
          </p>
          <h2 className="mt-1 font-display text-base text-white md:text-lg">{action.label}</h2>
          {action.detail && <p className="mt-2 text-[1.25rem] leading-[1.08] text-[#c6dbf4]">{action.detail}</p>}

          {showInstanceInactiveWarning && (
            <div className="mt-4 border border-[#ffd76b] bg-[#4a3c12] p-3 text-[1.05rem] leading-snug text-[#ffe9a8]">
              This step is taking longer than expected. The instance might be
              inactive. You can stop provisioning and start again with a
              different server.
            </div>
          )}

          {showStopControl && stopRequested && (
            <div className="mt-4 border border-neon-cyan bg-[#0e2840] p-3 text-[1.05rem] leading-snug text-[#cfe7ff]">
              Stop requested. Noland will finish the current step, then stop
              before starting the next one.
            </div>
          )}

          <div className="mt-4">
            <div className="h-3 overflow-hidden border border-[#3f476c] bg-[#0b0f23] shadow-[inset_0_0_0_2px_#121731]">
              {progress === null ? (
                <div className="h-full w-2/5 animate-pulse bg-gradient-to-r from-[#1f3155] via-[#61f7ff] to-[#1f3155]" />
              ) : (
                <div
                  className="h-full bg-gradient-to-r from-[#2d5844] via-[#61f7ff] to-[#7bff48] transition-[width] duration-300"
                  style={{ width: `${progress}%` }}
                />
              )}
            </div>

            <div className="mt-2 flex items-center justify-between gap-2 text-[1.05rem] text-[#91b7d8]">
              <span>{progress === null ? "Working..." : `${Math.round(progress)}% complete`}</span>
              <span>{formatElapsed(action.startedAt, now)}</span>
            </div>
          </div>

          {onCancel && action.cancellable && (
            <div className="mt-4">
              <button
                type="button"
                className="border border-[#ff8ca2] bg-[#4b1f2f] px-3 py-2 font-display text-[12px] uppercase tracking-[0.12em] text-[#ffc1cf] hover:bg-[#673149] disabled:opacity-50"
                disabled={action.cancelRequested}
                onClick={onCancel}
              >
                {action.cancelRequested ? "Cancelling..." : "Cancel"}
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );

  if (inline) {
    return content;
  }

  return (
    <ModalFrame
      panelClassName="glass-panel pixel-frame max-w-xl shadow-[0_0_30px_rgba(68,214,255,0.2)]"
      overlayClassName="bg-[#02040be8] backdrop-blur-[2px]"
      zIndexClassName="z-[110]"
    >
      {content}
    </ModalFrame>
  );
}
