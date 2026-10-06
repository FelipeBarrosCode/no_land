import { useState } from "react";
import { Card } from "../../components/ui/Card";
import {
  getNotificationPreferences,
  setNotificationPreference,
  type NotificationKind,
} from "../../lib/notificationPreferences";

const SETTINGS: Array<{ kind: NotificationKind; title: string; description: string }> = [
  {
    kind: "instances",
    title: "Unattended instances",
    description: "Remind me every three hours when rented instances exist without an active stream.",
  },
  {
    kind: "network",
    title: "Network warnings",
    description: "Alert me when streaming has connection loss, high latency, jitter, or packet loss.",
  },
  {
    kind: "storage",
    title: "Shared storage completion",
    description: "Notify me when a shared-storage backup or restore finishes.",
  },
  {
    kind: "provisioning",
    title: "Provisioning updates",
    description: "Notify me when provisioning needs your attention or finishes.",
  },
];

export function NotificationSettings() {
  const [preferences, setPreferences] = useState(getNotificationPreferences);

  return (
    <Card className="pixel-frame">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        System Notifications
      </h2>
      <p className="mt-2 text-[1.05rem] text-[#a8bed6]">
        Choose which Noland events may appear as operating-system notifications.
      </p>
      <div className="mt-5 space-y-3">
        {SETTINGS.map(({ kind, title, description }) => (
          <label
            key={kind}
            className="flex cursor-pointer items-start justify-between gap-4 border border-[#3d426f] bg-[#10152f] p-4"
          >
            <span>
              <span className="block font-display text-[11px] uppercase tracking-[0.08em] text-white">
                {title}
              </span>
              <span className="mt-1 block text-[1rem] leading-snug text-[#a8bed6]">
                {description}
              </span>
            </span>
            <input
              type="checkbox"
              className="mt-1 h-5 w-5 accent-[#7bff48]"
              checked={preferences[kind]}
              onChange={(event) =>
                setPreferences(setNotificationPreference(kind, event.target.checked))
              }
            />
          </label>
        ))}
      </div>
    </Card>
  );
}
