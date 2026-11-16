import type { BackupFinalizationStage, SharedStorageProgressEvent } from "./types";

// Remote phases are data, never translation keys or authoritative UI messages.
export const BACKUP_FINALIZATION_LABELS: Record<BackupFinalizationStage, string> = {
  publishing_manifest: "storage.backup.finalization.publishing_manifest",
  publishing_catalog: "storage.backup.finalization.publishing_catalog",
  verifying_backup_history: "storage.backup.finalization.verifying_backup_history",
  caching_backup_metadata: "storage.backup.finalization.caching_backup_metadata",
  saving_backup_index: "storage.backup.finalization.saving_backup_index",
  writing_local_checkpoint: "storage.backup.finalization.writing_local_checkpoint",
  releasing_repository: "storage.backup.finalization.releasing_repository",
};

type Translate = (key: string, values?: Record<string, string | number>) => string;

function knownStage(value: unknown): BackupFinalizationStage | null {
  return typeof value === "string" && Object.prototype.hasOwnProperty.call(BACKUP_FINALIZATION_LABELS, value)
    ? value as BackupFinalizationStage : null;
}

/** Pure presentation only: neither EOF, zero packs nor the last stage is success. */
export function backupProgressPresentation(
  event: SharedStorageProgressEvent,
  translate: Translate,
  formatBytes: (bytes: number) => string,
) {
  if (event.kind !== "backup") return null;
  const pipeline = event.backupPipeline;
  const terminal = ["COMPLETED", "SEALED", "ROLLED_BACK", "FAILED", "CANCELLED", "INTERRUPTED"].includes(event.state);
  const retrying = ["QUEUED", "REQUESTED"].includes(event.state);
  const telemetry = event.backupFinalization?.version === 1 && !retrying
    ? event.backupFinalization : null;
  const stage = knownStage(event.phase) ?? knownStage(telemetry?.stage);
  const finalizing = !terminal && !retrying &&
    (stage !== null || telemetry !== null || ["COMMITTING", "CHECKPOINTING"].includes(event.state));
  if (!pipeline && !finalizing) return null;
  const detail: string[] = [];
  if (pipeline) {
    detail.push(
      `${pipeline.selected_files} files · ${formatBytes(pipeline.source_total_bytes)} selected`,
      `${pipeline.completed_files + pipeline.reused_files}/${pipeline.selected_files} files captured or reused`,
      `${formatBytes(pipeline.durable_bytes)}/${formatBytes(pipeline.capture_total_bytes)} captured`,
      `${formatBytes(pipeline.prepared_bytes)} prepared`,
    );
  }
  if (typeof event.transferredBytes === "number" && Number.isFinite(event.transferredBytes) && event.transferredBytes >= 0) {
    detail.push(translate("storage.backup.gameDataUploaded", { bytes: formatBytes(event.transferredBytes) }));
  }
  if (event.completedObjects != null) detail.push(`${event.completedObjects} packs confirmed`);
  const idleWorkers = pipeline?.active_readers === 0 && pipeline.active_codecs === 0 && pipeline.inflight_uploads === 0;
  if (pipeline && !(finalizing && pipeline.capture_complete === true && idleWorkers)) {
    detail.push(`${pipeline.active_readers} readers · ${pipeline.active_codecs} packers · ${pipeline.inflight_uploads}/${pipeline.upload_limit} uploads`);
    if (!finalizing) detail.push(`Waiting: ${pipeline.waiting_on.replace(/_/g, " ")}`);
  }
  if (finalizing) {
    if (pipeline?.capture_complete === true && event.totalObjects === 0 && pipeline.prepared_bytes === 0) {
      detail.push(translate("storage.backup.noNewGameDataPacks"));
    }
    detail.push(translate("storage.backup.finalization.metadataWork"));
  } else if (terminal && event.message) {
    // Keep authoritative terminal cleanup-required warnings visible.
    detail.push(event.message);
  }
  return {
    label: finalizing
      ? translate(stage ? BACKUP_FINALIZATION_LABELS[stage] : "storage.backup.inProgress")
      : null,
    detail: detail.join(" · "),
    indeterminate: finalizing || !!pipeline,
  };
}
