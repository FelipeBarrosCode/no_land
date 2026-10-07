import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { isNotificationEnabled } from "./notificationPreferences";
import { translate, translateSource } from "./i18n";

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
        ? translate("notification.provisioning.complete")
        : translate("notification.provisioning.attention"),
      body: details
        ? `${translateSource(message)} ${translateSource(details)}`
        : translateSource(message),
      icon: "icons/icon.png",
      silent: false,
    });
  } catch (error) {
    console.warn("[provisioning] native notification failed", error);
  }
}
