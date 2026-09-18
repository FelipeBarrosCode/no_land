import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

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
    return "The connection to your gaming PC appears to be lost.";
  }
  if (event.reasons.includes("PACKET_LOSS") && metrics) {
    return `Packet loss has reached ${(metrics.lossPercent ?? 0).toFixed(1)}%. Streaming may stutter.`;
  }
  if (event.reasons.includes("HIGH_JITTER") && metrics) {
    return `Network jitter has reached ${(metrics.jitterMs ?? 0).toFixed(1)} ms. Streaming may feel inconsistent.`;
  }
  if (event.reasons.includes("HIGH_LATENCY") && metrics?.medianRttMs != null) {
    return `Network latency is ${metrics.medianRttMs.toFixed(1)} ms. Input may feel delayed.`;
  }
  return "High latency variation or packet loss may affect streaming.";
}

export async function notifyBadConnection(event: NetworkStatusEvent): Promise<void> {
  const body = networkWarningBody(event);
  try {
    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (granted) {
      await sendNotification({
        title: event.reasons.includes("CONNECTION_LOST")
          ? "No Land — Connection lost"
          : "No Land — Connection unstable",
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
