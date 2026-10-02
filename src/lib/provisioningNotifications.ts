import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { isNotificationEnabled } from "./notificationPreferences";

export async function notifyProvisioningUpdate(
  kind: "attention" | "complete",
  message: string,
  details?: string,
): Promise<void> {
  if (!isNotificationEnabled("provisioning")) {
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
      title: kind === "complete"
        ? "No Land — Provisioning complete"
        : "No Land — Provisioning needs attention",
      body: details ? `${message} ${details}` : message,
      icon: "icons/icon.png",
      silent: false,
    });
  } catch (error) {
    console.warn("[provisioning] native notification failed", error);
  }
}
