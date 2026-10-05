import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { isNotificationEnabled } from "./notificationPreferences";
import type { SpendAlert } from "./types";

export function spendAlertTitle(alert: SpendAlert): string {
  switch (alert.kind) {
    case "exceeded":
      return "No Land — Monthly budget reached";
    case "auto_stopped":
      return "No Land — Instance stopped by budget";
    case "auto_stop_failed":
      return "No Land — Budget stop failed";
    default:
      return "No Land — Approaching monthly budget";
  }
}

export async function notifySpendAlert(alert: SpendAlert): Promise<void> {
  // A failed auto-stop means an instance keeps billing past the budget, so
  // it is shown even when budget notifications are turned off.
  if (alert.kind !== "auto_stop_failed" && !isNotificationEnabled("budget")) {
    return;
  }
  try {
    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (!granted) {
      return;
    }
    await sendNotification({
      title: spendAlertTitle(alert),
      body: alert.message,
      icon: "icons/icon.png",
      silent: false,
    });
  } catch (error) {
    console.warn("[spend] native notification failed", error);
  }
}
