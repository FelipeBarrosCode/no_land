interface StorageAction {
  key: string;
  instanceId?: number | null;
  operationId?: string | null;
  stage?: string | null;
  pendingStorageRequest?: boolean;
  storageOperationIds?: string[];
}

export function canClearStorageCompletion(action: StorageAction | null, operationId: string) {
  return action?.operationId === operationId && !action.pendingStorageRequest &&
    (action.key === "instance.storage.sync" || action.key === "instance.storage.export");
}

export function settleBackgroundRequest<T extends {
  requestId?: string;
  stage?: string | null;
  pendingStorageRequest?: boolean;
}>(action: T | null, requestId: string): T | null {
  if (action?.requestId !== requestId) return action;
  return action.stage === "MONITORING_PAUSED" ? { ...action, pendingStorageRequest: false } : null;
}

interface StorageEvent {
  instanceId: number;
  operationId: string;
  kind: string;
}

/** One selected-app request may emit several sequential agent operations. */
export function correlateStorageProgress(action: StorageAction, event: StorageEvent) {
  const kind = action.key === "instance.storage.sync" ? "restore"
    : action.key === "instance.storage.export" ? "backup" : null;
  if (!kind || event.kind !== kind || !event.operationId ||
      (action.instanceId != null && action.instanceId !== event.instanceId)) {
    return null;
  }
  const previous = action.storageOperationIds ?? (action.operationId ? [action.operationId] : []);
  if (action.operationId && action.operationId !== event.operationId &&
      (!action.pendingStorageRequest || action.stage !== "COMPLETED" ||
        previous.includes(event.operationId))) {
    return null;
  }
  return {
    operationId: event.operationId,
    instanceId: event.instanceId,
    storageOperationIds: previous.includes(event.operationId) ? previous : [...previous, event.operationId],
  };
}
