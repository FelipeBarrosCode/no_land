import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";

export interface AppUpdateInfo {
  currentVersion: string;
  latestVersion: string;
  releaseName: string;
  releaseNotes: string;
  publishedAt: string | null;
}

export interface AppUpdateProgress {
  phase: "downloading" | "installing" | "restarting";
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number | null;
}

let pendingUpdate: Update | null = null;

export async function checkForAppUpdate(): Promise<AppUpdateInfo | null> {
  if (!("__TAURI_INTERNALS__" in window)) return null;

  if (!pendingUpdate) pendingUpdate = await check({ timeout: 30_000 });
  if (!pendingUpdate) return null;

  return {
    currentVersion: pendingUpdate.currentVersion,
    latestVersion: pendingUpdate.version,
    releaseName: `Noland Connect ${pendingUpdate.version}`,
    releaseNotes: pendingUpdate.body?.trim() || "Performance improvements and bug fixes.",
    publishedAt: pendingUpdate.date ?? null,
  };
}

export async function installPendingAppUpdate(
  onProgress: (progress: AppUpdateProgress) => void,
): Promise<void> {
  const update = pendingUpdate;
  if (!update) throw new Error("The update is no longer available. Check again.");

  let downloadedBytes = 0;
  let totalBytes: number | null = null;
  const report = (event: DownloadEvent) => {
    if (event.event === "Started") {
      totalBytes = event.data.contentLength ?? null;
    } else if (event.event === "Progress") {
      downloadedBytes += event.data.chunkLength;
    }
    onProgress({
      phase: event.event === "Finished" ? "installing" : "downloading",
      downloadedBytes,
      totalBytes,
      percent: totalBytes && totalBytes > 0
        ? Math.min(100, Math.round((downloadedBytes / totalBytes) * 100))
        : null,
    });
  };

  await update.downloadAndInstall(report);
  pendingUpdate = null;
  onProgress({ phase: "restarting", downloadedBytes, totalBytes, percent: 100 });
  await relaunch();
}
