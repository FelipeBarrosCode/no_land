import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { isNotificationEnabled } from "./notificationPreferences";
import { translate } from "./i18n";
import type { SpendAlert } from "./types";

export function spendAlertTitle(alert: SpendAlert): string {
  switch (alert.kind) {
    case "exceeded":
      return translate("notification.spend.exceeded.title");
    case "auto_stopped":
      return translate("notification.spend.auto_stopped.title");
    case "auto_stop_failed":
      return translate("notification.spend.auto_stop_failed.title");
    default:
      return translate("notification.spend.warning.title");
  }
}

export async function notifySpendAlert(alert: SpendAlert): Promise<boolean> {
  // A failed auto-stop means an instance keeps billing past the budget, so
  // it is shown even when budget notifications are turned off.
  if (alert.kind !== "auto_stop_failed" && !isNotificationEnabled("budget")) {
    return true;
  }
  try {
    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (!granted) {
      return true;
    }
    await sendNotification({
      title: spendAlertTitle(alert),
      body: alert.message,
      icon: "icons/icon.png",
      silent: false,
    });
    return true;
  } catch (error) {
    console.warn("[spend] native notification failed", error);
    return false;
  }
}
