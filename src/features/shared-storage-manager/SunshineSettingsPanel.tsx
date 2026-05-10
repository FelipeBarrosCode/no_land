import { useState, useEffect } from "react";
import { BlockingLoaderOverlay, type BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import type { SunshineSetting, SunshineSettingsResponse } from "../../lib/types";

interface Props {
  settings: SunshineSettingsResponse | null;
  busy: boolean;
  defaultUsername: string;
  defaultPassword: string;
  onLoad: (sunshineUsername: string, sunshinePassword: string) => Promise<void>;
  onSave: (settings: Record<string, unknown>, sunshineUsername: string, sunshinePassword: string) => Promise<void>;
  onReset: (sunshineUsername: string, sunshinePassword: string) => Promise<void>;
  onClose: () => void;
}

export function SunshineSettingsPanel({
  settings,
  busy,
  defaultUsername,
  defaultPassword,
  onLoad,
  onSave,
  onReset,
  onClose
}: Props) {
  const [formValues, setFormValues] = useState<Record<string, string>>({});
  const [sunshineUsername, setSunshineUsername] = useState(defaultUsername);
  const [sunshinePassword, setSunshinePassword] = useState(defaultPassword);
  const [hasChanges, setHasChanges] = useState(false);
  const [pendingAction, setPendingAction] = useState<BlockingActionState | null>(null);

  useEffect(() => {
    setSunshineUsername(defaultUsername);
    setSunshinePassword(defaultPassword);
  }, [defaultUsername, defaultPassword]);

  useEffect(() => {
    if (settings?.raw) {
      const initial: Record<string, string> = {};
      Object.entries(settings.raw).forEach(([key, value]) => {
        initial[key] = String(value ?? "");
      });
      setFormValues(initial);
      setHasChanges(false);
    }
  }, [settings]);

  const handleChange = (key: string, value: string) => {
    setFormValues((prev) => ({ ...prev, [key]: value }));
    setHasChanges(true);
  };

  const handleToggle = (key: string) => {
    const current = formValues[key]?.toLowerCase();
    const next = current === "true" || current === "enabled" ? "false" : "true";
    setFormValues((prev) => ({ ...prev, [key]: next }));
    setHasChanges(true);
  };

  const handleSave = async () => {
    setPendingAction({
      key: "sunshine.settings.save",
      label: "Saving Sunshine settings",
      detail: "Applying the updated Sunshine configuration on the instance.",
      mode: "indeterminate",
      progress: null,
      startedAt: Date.now()
    });
    const payload: Record<string, unknown> = {};
    Object.entries(formValues).forEach(([key, value]) => {
      const original = settings?.raw[key];
      if (typeof original === "boolean" || value === "true" || value === "false") {
        payload[key] = value === "true";
      } else if (typeof original === "number" || !Number.isNaN(Number(value))) {
        payload[key] = Number(value);
      } else {
        payload[key] = value;
      }
    });
    await onSave(payload, sunshineUsername.trim(), sunshinePassword);
    setHasChanges(false);
    setPendingAction(null);
  };

  const canUseApi = sunshineUsername.trim().length > 0 && sunshinePassword.trim().length > 0;

  const renderInput = (setting: { key: string; value: unknown; label: string; description?: string; valueType: string; requiresRestart: boolean }) => {
    const { key, label, description, valueType } = setting;
    const currentValue = formValues[key] ?? "";

    if (valueType === "boolean") {
      const isEnabled = currentValue.toLowerCase() === "true" || currentValue.toLowerCase() === "enabled";
      return (
        <div key={key} className="flex items-center gap-3">
          <input
            type="checkbox"
            id={`sunshine-${key}`}
            checked={isEnabled}
            onChange={() => handleToggle(key)}
            className="w-4 h-4 accent-neon-cyan"
            disabled={busy}
          />
          <div className="flex-1">
            <label htmlFor={`sunshine-${key}`} className="text-sm text-gray-200 font-medium">
              {label}
            </label>
            {setting.requiresRestart && (
              <p className="text-[10px] uppercase tracking-wide text-[#ffbb66]">Restart required</p>
            )}
            {description && (
              <p className="text-xs text-gray-500">{description}</p>
            )}
          </div>
        </div>
      );
    }

    return (
      <InputField
        key={key}
        label={`${label}${setting.requiresRestart ? " (restart required)" : ""}`}
        value={currentValue}
        onChange={(event) => handleChange(key, event.target.value)}
        placeholder={description}
        disabled={busy}
      />
    );
  };

  const categoryOrder = [
    "General",
    "Input",
    "Audio/Video",
    "Network",
    "Advanced",
    "NVIDIA NVENC",
    "Intel QuickSync",
    "AMD AMF",
    "VideoToolbox",
    "VA-API",
    "Software Encoder",
    "Config Files",
    "Other"
  ];

  const groupedSettings = (settings?.settings ?? []).reduce<Record<string, SunshineSetting[]>>(
    (acc, setting) => {
      const category = setting.category || "Other";
      if (!acc[category]) {
        acc[category] = [];
      }
      acc[category].push(setting);
      return acc;
    },
    {}
  );

  const sortedCategories = Object.keys(groupedSettings).sort((a, b) => {
    const indexA = categoryOrder.indexOf(a);
    const indexB = categoryOrder.indexOf(b);
    const rankA = indexA === -1 ? categoryOrder.length : indexA;
    const rankB = indexB === -1 ? categoryOrder.length : indexB;
    return rankA - rankB;
  });

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm p-4">
      <Card className="w-full max-w-2xl max-h-[80vh] overflow-y-auto p-6">
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-lg font-display text-neon-cyan">Sunshine Settings</h3>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            Close
          </Button>
        </div>

        {!settings ? (
          <div className="space-y-4">
            {pendingAction && <BlockingLoaderOverlay action={pendingAction} inline className="max-w-none p-4" />}
            <p className="text-gray-300">Enter Sunshine credentials to load settings.</p>
            <div className="grid gap-3 md:grid-cols-2">
              <InputField
                label="Sunshine Username"
                value={sunshineUsername}
                onChange={(event) => setSunshineUsername(event.target.value)}
                disabled={busy}
              />
              <InputField
                label="Sunshine Password"
                type="password"
                value={sunshinePassword}
                onChange={(event) => setSunshinePassword(event.target.value)}
                disabled={busy}
              />
            </div>
            <div>
              <Button
                variant="primary"
                onClick={async () => {
                  setPendingAction({
                    key: "sunshine.settings.load",
                    label: "Loading Sunshine settings",
                    detail: "Fetching the current Sunshine configuration from the instance.",
                    mode: "indeterminate",
                    progress: null,
                    startedAt: Date.now()
                  });
                  await onLoad(sunshineUsername.trim(), sunshinePassword);
                  setPendingAction(null);
                }}
                disabled={busy || !canUseApi}
                loading={busy}
                loadingText="Loading..."
              >
                Load Settings
              </Button>
            </div>
          </div>
        ) : (
          <div className="space-y-6">
            {pendingAction && <BlockingLoaderOverlay action={pendingAction} inline className="max-w-none p-4" />}
            <div className="grid gap-3 md:grid-cols-2">
              <InputField
                label="Sunshine Username"
                value={sunshineUsername}
                onChange={(event) => setSunshineUsername(event.target.value)}
                disabled={busy}
              />
              <InputField
                label="Sunshine Password"
                type="password"
                value={sunshinePassword}
                onChange={(event) => setSunshinePassword(event.target.value)}
                disabled={busy}
              />
            </div>

            <div className="space-y-4">
              <p className="text-xs text-[#9bb4d7]">
                Loaded from Sunshine API at <span className="text-neon-cyan">http://10.77.0.1:47990/api/config</span>
              </p>
            </div>

            {sortedCategories.map((category) => (
              <div key={category} className="space-y-4">
                <h4 className="text-sm font-display text-neon-lime uppercase tracking-wider">{category}</h4>
                {groupedSettings[category].map(renderInput)}
              </div>
            ))}

            {hasChanges && (
              <div className="text-xs text-neon-cyan bg-neon-cyan/10 p-2 rounded">
                You have unsaved changes.
              </div>
            )}

            <div className="flex gap-3 pt-2">
              <Button
                variant="ghost"
                onClick={async () => {
                  setPendingAction({
                    key: "sunshine.settings.reset",
                    label: "Resetting Sunshine settings",
                    detail: "Restoring the provisioned Sunshine defaults on the instance.",
                    mode: "indeterminate",
                    progress: null,
                    startedAt: Date.now()
                  });
                  await onReset(sunshineUsername.trim(), sunshinePassword);
                  setPendingAction(null);
                }}
                disabled={busy || !canUseApi}
                loading={busy && pendingAction?.key === "sunshine.settings.reset"}
                loadingText="Resetting..."
              >
                Reset to Provision Defaults
              </Button>
              <Button
                variant="primary"
                onClick={handleSave}
                disabled={busy || !hasChanges || !canUseApi}
                loading={busy && pendingAction?.key === "sunshine.settings.save"}
                loadingText="Saving..."
              >
                Save Settings
              </Button>
              <Button variant="secondary" onClick={onClose} disabled={busy}>
                Cancel
              </Button>
            </div>
          </div>
        )}
      </Card>
    </div>
  );
}
