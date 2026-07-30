import { useEffect, useState } from "react";
import clsx from "clsx";

export type BlockingLoaderMode = "indeterminate" | "determinate";

export interface BlockingActionState {
  key: string;
  label: string;
  detail?: string | null;
  progress?: number | null;
  startedAt: number;
  mode: BlockingLoaderMode;
}

interface Props {
  action: BlockingActionState;
  inline?: boolean;
  className?: string;
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

export function BlockingLoaderOverlay({ action, inline = false, className }: Props) {
  const [now, setNow] = useState(() => Date.now());
  const progress =
    action.mode === "determinate" && typeof action.progress === "number"
      ? Math.max(0, Math.min(100, action.progress))
      : null;

  useEffect(() => {
    const intervalId = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(intervalId);
  }, []);

  const content = (
    <div
      aria-busy="true"
      aria-live="polite"
      className={clsx(
        "glass-panel pixel-frame max-h-[80vh] w-full max-w-xl overflow-y-auto p-6 text-left shadow-[0_0_30px_rgba(68,214,255,0.2)]",
        className
      )}
    >
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
        </div>
      </div>
    </div>
  );

  if (inline) {
    return content;
  }

  return (
    <div className="fixed inset-0 z-[110] flex items-center justify-center bg-[#02040be8] p-4 backdrop-blur-[2px]">
      {content}
    </div>
  );
}
