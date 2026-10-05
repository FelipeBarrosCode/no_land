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

export function getNotificationPreferences(): NotificationPreferences {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "null") as Partial<NotificationPreferences> | null;
    return {
      network: parsed?.network ?? DEFAULTS.network,
      storage: parsed?.storage ?? DEFAULTS.storage,
      instances: parsed?.instances ?? DEFAULTS.instances,
      provisioning: parsed?.provisioning ?? DEFAULTS.provisioning,
      budget: parsed?.budget ?? DEFAULTS.budget,
    };
  } catch {
    return { ...DEFAULTS };
  }
}

export function isNotificationEnabled(kind: NotificationKind): boolean {
  return getNotificationPreferences()[kind];
}

export function setNotificationPreference(kind: NotificationKind, enabled: boolean): NotificationPreferences {
  const next = { ...getNotificationPreferences(), [kind]: enabled };
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Keep the preference effective for the current process even if storage is unavailable.
  }
  return next;
}
