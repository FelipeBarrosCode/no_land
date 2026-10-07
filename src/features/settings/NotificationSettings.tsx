import { useState } from "react";
import { Card } from "../../components/ui/Card";
import {
  getNotificationPreferences,
  setNotificationPreference,
  type NotificationKind,
} from "../../lib/notificationPreferences";
import { useLocalization } from "../../lib/i18n";

const SETTINGS: Array<{ kind: NotificationKind; title: string; description: string }> = [
  {
    kind: "instances",
    title: "notifications.instances.title",
    description: "notifications.instances.description",
  },
  {
    kind: "network",
    title: "notifications.network.title",
    description: "notifications.network.description",
  },
  {
    kind: "storage",
    title: "notifications.storage.title",
    description: "notifications.storage.description",
  },
  {
    kind: "provisioning",
    title: "notifications.provisioning.title",
    description: "notifications.provisioning.description",
  },
];

export function NotificationSettings() {
  const { t } = useLocalization();
  const [preferences, setPreferences] = useState(getNotificationPreferences);

  return (
    <Card className="pixel-frame">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        {t("notifications.system")}
      </h2>
      <p className="mt-2 text-[1.05rem] text-[#a8bed6]">
        {t("notifications.description")}
      </p>
      <div className="mt-5 space-y-3">
        {SETTINGS.map(({ kind, title, description }) => (
          <label
            key={kind}
            className="flex cursor-pointer items-start justify-between gap-4 border border-[#3d426f] bg-[#10152f] p-4"
          >
            <span>
              <span className="block font-display text-[11px] uppercase tracking-[0.08em] text-white">
                 {t(title)}
              </span>
              <span className="mt-1 block text-[1rem] leading-snug text-[#a8bed6]">
                 {t(description)}
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
