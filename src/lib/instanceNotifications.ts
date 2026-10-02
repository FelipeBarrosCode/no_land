import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

export async function notifyInstancesNeedAttention(instanceCount: number): Promise<void> {
  try {
    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (!granted) {
      return;
    }

    await sendNotification({
      title: "No Land — Instances need attention",
      body: `You have ${instanceCount} rented instance${instanceCount === 1 ? "" : "s"} with no active stream. They may continue charging. Open No Land to continue, set up shared storage, or delete them.`,
      icon: "icons/icon.png",
      silent: false,
    });
  } catch (error) {
    console.warn("[instance-monitor] native notification failed", error);
  }
}
