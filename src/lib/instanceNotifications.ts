import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { isNotificationEnabled } from "./notificationPreferences";
import { translate } from "./i18n";

export async function notifyInstancesNeedAttention(instanceCount: number): Promise<void> {
  if (!isNotificationEnabled("instances")) {
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
      title: translate("notification.instances.title"),
      body: translate("notification.instances.body", { count: instanceCount }),
      icon: "icons/icon.png",
      silent: false,
    });
  } catch (error) {
    console.warn("[instance-monitor] native notification failed", error);
  }
}
