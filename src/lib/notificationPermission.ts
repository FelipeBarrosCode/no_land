import { invoke } from "@tauri-apps/api/core";
import { requestPermission } from "@tauri-apps/plugin-notification";

const REQUESTED_KEY = "noland.notificationPermissionRequested.v1";
let requestedThisSession = false;
let pending: Promise<boolean> | null = null;

function wasRequested(): boolean {
  if (requestedThisSession) return true;
  try {
    return window.localStorage.getItem(REQUESTED_KEY) === "true";
  } catch {
    return false;
  }
}

function rememberRequest() {
  requestedThisSession = true;
  try {
    window.localStorage.setItem(REQUESTED_KEY, "true");
  } catch {
    // Still suppress repeats in this process when local persistence is unavailable.
  }
}

async function checkPermission(explicit: boolean): Promise<boolean> {
  // A stored bit is never a grant. Respect OS revocation, and notice grants made
  // later in system settings without requesting authorization again.
  // The plugin's JS isPermissionGranted() caches Notification.permission for
  // the lifetime of a webview. Query native state so a system-settings change
  // is seen without restarting (null means the OS has not decided yet).
  const permission = await invoke<boolean | null>("plugin:notification|is_permission_granted");
  if (permission === true) return true;
  if (permission === false) {
    rememberRequest();
    return false;
  }
  if (!explicit && wasRequested()) return false;
  // Record before opening the dialog, including dismissals/errors. Background
  // events must not repeatedly ask; an explicit settings action can retry.
  rememberRequest();
  return (await requestPermission()) === "granted";
}

export function ensureNotificationPermission(explicit = false): Promise<boolean> {
  if (!pending) {
    pending = checkPermission(explicit).finally(() => { pending = null; });
  }
  return pending;
}
