import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { isNotificationEnabled } from "./notificationPreferences";
import { translate } from "./i18n";

export type NetworkStatusEvent = {
  current: "WARMING_UP" | "GREAT" | "GOOD" | "POOR" | "BAD";
  reasons: string[];
  alertEligible: boolean;
  keyMetrics?: {
    medianRttMs?: number | null;
    jitterMs?: number;
    lossPercent?: number;
    longestLossBurst?: number;
  };
};

export function networkWarningBody(event: NetworkStatusEvent): string {
  const metrics = event.keyMetrics;
  if (event.reasons.includes("CONNECTION_LOST")) {
    return translate("notification.network.lost.body");
  }
  if (event.reasons.includes("PACKET_LOSS") && metrics) {
    return translate("notification.network.packet_loss", { percent: (metrics.lossPercent ?? 0).toFixed(1) });
  }
  if (event.reasons.includes("HIGH_JITTER") && metrics) {
    return translate("notification.network.jitter", { jitter: (metrics.jitterMs ?? 0).toFixed(1) });
  }
  if (event.reasons.includes("HIGH_LATENCY") && metrics?.medianRttMs != null) {
    return translate("notification.network.latency", { latency: metrics.medianRttMs.toFixed(1) });
  }
  return translate("notification.network.generic");
}

export async function notifyBadConnection(event: NetworkStatusEvent): Promise<void> {
  if (!isNotificationEnabled("network")) {
    return;
  }
  const connectionLost = event.reasons.includes("CONNECTION_LOST");
  const body = connectionLost
    ? networkWarningBody(event)
    : translate("notification.network.unstable.body");
  try {
    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (granted) {
      await sendNotification({
        title: connectionLost
          ? translate("notification.network.lost.title")
          : translate("notification.network.unstable.title"),
        body,
        icon: "icons/icon.png",
        silent: false,
      });
    }
  } catch (error) {
    console.warn("[network-monitor] native notification failed", error);
  }

  try {
    const sound = new Audio("/connection-warning.wav");
    sound.volume = 0.65;
    await sound.play();
  } catch (error) {
    console.warn("[network-monitor] warning sound failed", error);
  }
}
