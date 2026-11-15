import type { SharedStorageBrowseSnapshot } from "./types";

export type StorageBrowseError =
  | "refresh_failed"
  | "scope_changed"
  | "helper_missing"
  | "agent_unavailable"
  | "agent_incompatible"
  | "agent_updating"
  | "provider_unavailable"
  | "refresh_timeout"
  | "helper_install_failed";

/** Only an own, plain code value may select locally-owned UI copy. */
export function storageBrowseError(reason: unknown): StorageBrowseError {
  let code: unknown;
  try {
    if (reason !== null && typeof reason === "object") {
      code = Object.getOwnPropertyDescriptor(reason, "code")?.value;
    }
  } catch {
    return "refresh_failed";
  }
  switch (code) {
    case "storage_helper_missing": return "helper_missing";
    case "storage_agent_unavailable": return "agent_unavailable";
    case "storage_agent_incompatible": return "agent_incompatible";
    case "storage_agent_updating": return "agent_updating";
    case "storage_provider_unavailable": return "provider_unavailable";
    case "storage_profile_changed": return "scope_changed";
    case "storage_refresh_timeout": return "refresh_timeout";
    case "storage_helper_install_failed": return "helper_install_failed";
    default: return "refresh_failed";
  }
}

export function storageBrowseErrorKey(error: StorageBrowseError): string {
  switch (error) {
    case "scope_changed": return "storage.browse.scopeChanged";
    case "helper_missing": return "storage.browse.helperMissing";
    case "agent_unavailable": return "storage.browse.agentUnavailable";
    case "agent_incompatible": return "storage.browse.agentIncompatible";
    case "agent_updating": return "storage.browse.agentUpdating";
    case "provider_unavailable": return "storage.browse.providerUnavailable";
    case "refresh_timeout": return "storage.browse.refreshTimeout";
    case "helper_install_failed": return "storage.browse.helperInstallFailed";
    default: return "storage.browse.failed";
  }
}

export interface StorageBrowseView {
  snapshot: SharedStorageBrowseSnapshot | null;
  refreshing: boolean;
  error: StorageBrowseError | null;
}

export interface StorageHelperPreparationView {
  preparing: boolean;
  error: "helper_install_failed" | null;
}

/** Creating a controller never installs anything; only a human's run() does. */
export function createStorageHelperPreparation(
  prepare: () => Promise<void>,
  update: (view: StorageHelperPreparationView) => void,
  ready: () => void,
  isCurrent: () => boolean,
) {
  let active = true;
  let preparing = false;
  return {
    async run() {
      if (!active || !isCurrent() || preparing) return;
      preparing = true;
      update({ preparing: true, error: null });
      let error: StorageHelperPreparationView["error"] = null;
      try {
        await prepare();
      } catch {
        // Never propagate native details, provider stderr or token material.
        error = "helper_install_failed";
      }
      preparing = false;
      if (!active || !isCurrent()) return;
      update({ preparing: false, error });
      if (!error && active && isCurrent()) ready();
    },
    cancel() { active = false; },
  };
}

/**
 * Cached rows are display hints only. The native transfer/deletion paths still
 * validate the selected repository objects before performing any operation.
 * A closed/replaced view cannot receive late responses from a previous request.
 */
export function startStorageBrowse(
  load: (refresh: boolean) => Promise<SharedStorageBrowseSnapshot>,
  update: (view: StorageBrowseView) => void,
  forceRefresh = false,
) {
  let active = true;
  let snapshot: SharedStorageBrowseSnapshot | null = null;
  const publish = (refreshing: boolean, error: StorageBrowseView["error"] = null) => {
    if (active) update({ snapshot, refreshing, error });
  };
  const fail = (error: StorageBrowseError) => {
    if (error === "scope_changed") snapshot = null;
    else if (snapshot?.freshness === "fresh") snapshot = { ...snapshot, freshness: "cached" };
    publish(false, error);
  };
  publish(true);
  const finished = (async () => {
    let scope: string | null = null;
    try {
      const cached = await load(false);
      if (!active) return;
      scope = cached.scope;
      if (cached.observedAt !== null) snapshot = cached;
      if (snapshot?.freshness === "fresh" && !forceRefresh) {
        publish(false);
        return;
      }
      publish(true);
    } catch {
      // A disposable cache failure must not prevent an authoritative refresh.
    }
    if (!active) return;
    try {
      const fresh = await load(true);
      if (!active) return;
      if (scope !== null && fresh.scope !== scope) {
        fail("scope_changed");
        return;
      }
      if (fresh.observedAt === null || fresh.freshness !== "fresh") {
        fail("refresh_failed");
        return;
      }
      snapshot = fresh;
      publish(false, snapshot ? null : "refresh_failed");
    } catch (reason) {
      fail(storageBrowseError(reason));
    }
  })();
  return {
    finished,
    cancel() { active = false; },
  };
}
