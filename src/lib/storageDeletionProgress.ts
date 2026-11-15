import type { SharedStorageProgressEvent } from "./types";

interface ConfirmedDeletion {
  instanceId: number;
  planId: string;
  target: "cloud" | "remote_pc";
}

/** A late event can never attach to a different plan, PC, or restore operation. */
export function deletionProgressFor(preview: ConfirmedDeletion, event: SharedStorageProgressEvent) {
  if (preview.target !== "cloud" || event.kind !== "delete" ||
      event.instanceId !== preview.instanceId || event.operationId !== preview.planId ||
      !event.deletion) return null;
  return event;
}

export function secondsSinceDeletionProgress(lastSuccess: string | null, now: number) {
  if (!lastSuccess) return null;
  const timestamp = Date.parse(lastSuccess);
  return Number.isFinite(timestamp) ? Math.max(0, Math.floor((now - timestamp) / 1000)) : null;
}

/** Tauri rejects with a structured FrontendError, not necessarily an Error. */
export function deletionErrorMessage(reason: unknown) {
  if (reason instanceof Error) return reason.message;
  if (reason && typeof reason === "object") {
    const error = reason as { details?: unknown; message?: unknown };
    if (typeof error.details === "string" && error.details.trim()) return error.details;
    if (typeof error.message === "string" && error.message.trim()) return error.message;
  }
  return String(reason);
}
