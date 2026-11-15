export type NotificationKind = "network" | "storage" | "instances" | "provisioning" | "budget";

export type NotificationPreferences = Record<NotificationKind, boolean>;

const STORAGE_KEY = "noland.notificationPreferences";
const DEFAULTS: NotificationPreferences = {
  network: true,
  storage: true,
  instances: true,
  provisioning: true,
  budget: true,
};
let current = { ...DEFAULTS };
let unsaved: Partial<NotificationPreferences> = {};

export function getNotificationPreferences(): NotificationPreferences {
  try {
    const parsed: unknown = JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "null");
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      for (const kind of Object.keys(DEFAULTS) as NotificationKind[]) {
        const value = (parsed as Record<string, unknown>)[kind];
        if (typeof value === "boolean") current[kind] = value;
      }
    }
  } catch {
    // Retain the user's current choice instead of reverting a denial to defaults.
  }
  current = { ...current, ...unsaved };
  return { ...current };
}

export function isNotificationEnabled(kind: NotificationKind): boolean {
  return getNotificationPreferences()[kind];
}

export function setNotificationPreference(kind: NotificationKind, enabled: boolean): NotificationPreferences {
  const next = { ...getNotificationPreferences(), [kind]: enabled };
  current = next;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    unsaved = {};
  } catch {
    // Keep the preference effective for the current process even if storage is unavailable.
    unsaved[kind] = enabled;
  }
  return { ...next };
}
