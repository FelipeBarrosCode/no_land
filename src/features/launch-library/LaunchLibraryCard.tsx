import { useEffect, useState } from "react";
import clsx from "clsx";
import { SpriteIcon } from "../../components/ui/SpriteIcon";
import type {
  LaunchLibraryItem,
  LaunchSoftwareJob,
  SoftwareArtworkResult,
} from "../../lib/types";

interface LaunchPcCardProps {
  available: boolean;
  disabled: boolean;
  onLaunch: () => void;
}

interface SoftwareLaunchCardProps {
  item: LaunchLibraryItem;
  artwork: SoftwareArtworkResult | undefined;
  artworkLoading: boolean;
  job: LaunchSoftwareJob | null;
  launching: boolean;
  disabled: boolean;
  onLoadArtwork: (name: string) => Promise<SoftwareArtworkResult | null>;
  onLaunch: () => void;
}

const cardClassName =
  "glass-panel group flex min-h-[21rem] w-full flex-col overflow-hidden p-0 text-left transition duration-100 enabled:hover:border-neon-cyan enabled:hover:shadow-[inset_0_0_0_2px_#090a17,inset_0_0_0_4px_#2d315b,0_0_0_2px_#090a17,0_0_22px_rgba(68,214,255,0.35)] disabled:cursor-not-allowed disabled:opacity-55";

const terminalStatuses = new Set([
  "completed",
  "complete",
  "succeeded",
  "success",
  "failed",
  "error",
  "cancelled",
  "canceled",
]);

export function isTerminalLaunchJob(job: LaunchSoftwareJob): boolean {
  return Boolean(job.finishedAt) || terminalStatuses.has(job.status.toLowerCase());
}

function isFailedLaunchJob(job: LaunchSoftwareJob): boolean {
  const status = job.status.toLowerCase();
  return Boolean(job.error) || status === "failed" || status === "error";
}

function jobProgress(job: LaunchSoftwareJob): number {
  if (job.streamStarted || (!isFailedLaunchJob(job) && isTerminalLaunchJob(job))) {
    return 100;
  }

  const status = job.status.toLowerCase();
  if (status.includes("stream") || status.includes("launch") || status.includes("start")) {
    return 78;
  }
  if (job.restorePerformed || status.includes("restore")) {
    return 52;
  }
  if (status.includes("queue") || status.includes("pending")) {
    return 18;
  }
  return 32;
}

function Badge({ children, tone = "blue" }: { children: string; tone?: "blue" | "green" | "amber" }) {
  const toneClass =
    tone === "green"
      ? "border-neon-lime/40 bg-neon-lime/10 text-neon-lime"
      : tone === "amber"
        ? "border-amber-400/40 bg-amber-400/10 text-amber-200"
        : "border-neon-cyan/35 bg-neon-cyan/10 text-[#9aefff]";

  return (
    <span
      className={clsx(
        "border px-2 py-1 font-display text-[9px] uppercase tracking-[0.1em]",
        toneClass,
      )}
    >
      {children}
    </span>
  );
}

export function LaunchPcCard({ available, disabled, onLaunch }: LaunchPcCardProps) {
  return (
    <button
      type="button"
      className={cardClassName}
      disabled={disabled || !available}
      onClick={onLaunch}
    >
      <div className="relative flex h-32 shrink-0 items-center justify-center overflow-hidden border-b border-[#354269] bg-[radial-gradient(circle_at_center,_rgba(68,214,255,0.26),_rgba(10,14,31,0.96)_68%)]">
        <SpriteIcon icon="server" className="scale-[2.5]" />
        <div className="absolute inset-x-0 bottom-0 h-px bg-neon-cyan/50" />
      </div>
      <div className="flex flex-1 flex-col p-4">
        <div className="flex flex-wrap gap-2">
          <Badge tone="green">Full desktop</Badge>
          <Badge>Launch PC</Badge>
        </div>
        <h3 className="mt-4 font-display text-lg text-white">Launch PC</h3>
        <p className="mt-2 text-sm leading-relaxed text-[#9db8d4]">
          Start the full remote desktop and choose anything installed on the PC.
        </p>
        <div className="mt-auto border-t border-[#283252] pt-4">
          <p className={clsx("font-display text-[10px] uppercase tracking-[0.12em]", available ? "text-neon-lime" : "text-amber-200")}>
            {available ? "Ready to stream" : "Unavailable"}
          </p>
        </div>
      </div>
    </button>
  );
}

export function SoftwareLaunchCard({
  item,
  artwork,
  artworkLoading,
  job,
  launching,
  disabled,
  onLoadArtwork,
  onLaunch,
}: SoftwareLaunchCardProps) {
  const [imageFailed, setImageFailed] = useState(false);
  const [requestedArtwork, setRequestedArtwork] = useState("");
  const artworkName = item.artworkKey.trim() || item.displayName;

  useEffect(() => {
    setImageFailed(false);
    if (!artwork && !artworkLoading && requestedArtwork !== artworkName) {
      setRequestedArtwork(artworkName);
      void onLoadArtwork(artworkName);
    }
  }, [artwork, artworkLoading, artworkName, onLoadArtwork, requestedArtwork]);

  const active = launching || job !== null;
  const failed = job ? isFailedLaunchJob(job) : false;
  const progress = job ? jobProgress(job) : 0;
  const statusText = launching
    ? "Starting launch…"
    : job
      ? job.error || job.message || job.status
      : item.launchable
        ? item.restoreRequired
          ? "Restore, then launch"
          : "Ready to launch"
        : "Not launchable";

  return (
    <button
      type="button"
      className={cardClassName}
      disabled={disabled || !item.launchable}
      onClick={onLaunch}
    >
      <div className="relative h-32 shrink-0 overflow-hidden border-b border-[#354269] bg-[linear-gradient(135deg,_#111a38,_#1b2948_48%,_#0a0e1f)]">
        {artwork?.imageUrl && !imageFailed ? (
          <img
            src={artwork.imageUrl}
            alt=""
            className="h-full w-full object-cover transition duration-200 group-hover:scale-[1.03]"
            onError={() => setImageFailed(true)}
          />
        ) : (
          <div className="flex h-full items-center justify-center">
            {artworkLoading ? (
              <span className="h-7 w-7 animate-spin rounded-full border-2 border-neon-cyan border-t-transparent" />
            ) : (
              <SpriteIcon icon="play" className="scale-[2]" />
            )}
          </div>
        )}
        <div className="absolute inset-0 bg-gradient-to-t from-[#070b18]/75 via-transparent to-transparent" />
      </div>

      <div className="flex flex-1 flex-col p-4">
        <div className="flex flex-wrap gap-2">
          {item.installed ? <Badge tone="green">Installed</Badge> : <Badge>Cloud tracked</Badge>}
          {item.inSharedStorage ? <Badge>Shared storage</Badge> : null}
          {item.restoreRequired ? <Badge tone="amber">Restore required</Badge> : null}
        </div>

        <h3 className="mt-4 font-display text-base text-white">{item.displayName}</h3>
        <p className="mt-2 text-xs uppercase tracking-wider text-[#7890ae]">
          {item.launchMethod || "Detected software"}
        </p>

        {item.sourceLabels.length > 0 ? (
          <p className="mt-2 line-clamp-2 text-sm text-[#9db8d4]">
            {item.sourceLabels.join(" · ")}
          </p>
        ) : item.aliases.length > 0 ? (
          <p className="mt-2 line-clamp-2 text-sm text-[#9db8d4]">
            {item.aliases.join(" · ")}
          </p>
        ) : null}

        <div className="mt-auto border-t border-[#283252] pt-4">
          <div className="flex items-start justify-between gap-3">
            <p
              className={clsx(
                "break-words font-display text-[10px] uppercase tracking-[0.1em]",
                failed ? "text-red-300" : active ? "text-neon-cyan" : item.launchable ? "text-neon-lime" : "text-[#7890ae]",
              )}
            >
              {statusText}
            </p>
            {job?.streamStarted ? <span className="text-xs text-neon-lime">Streaming</span> : null}
          </div>

          {active ? (
            <div className="mt-3 h-2 overflow-hidden border border-[#3f476c] bg-[#0b0f23]">
              {launching ? (
                <div className="h-full w-2/5 animate-pulse bg-gradient-to-r from-[#1f3155] via-[#61f7ff] to-[#1f3155]" />
              ) : (
                <div
                  className={clsx(
                    "h-full transition-[width] duration-300",
                    failed
                      ? "bg-red-400"
                      : "bg-gradient-to-r from-[#2d5844] via-[#61f7ff] to-[#7bff48]",
                  )}
                  style={{ width: `${progress}%` }}
                />
              )}
            </div>
          ) : null}
        </div>
      </div>
    </button>
  );
}
