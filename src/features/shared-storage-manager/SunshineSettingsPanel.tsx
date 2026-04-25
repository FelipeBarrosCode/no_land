import { useState, useEffect } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import type { SunshineSettingsResponse } from "../../lib/types";

interface Props {
  settings: SunshineSettingsResponse | null;
  busy: boolean;
  onSave: (settings: Record<string, unknown>) => Promise<void>;
  onClose: () => void;
}

export function SunshineSettingsPanel({ settings, busy, onSave, onClose }: Props) {
  const [formValues, setFormValues] = useState<Record<string, string>>({});
  const [hasChanges, setHasChanges] = useState(false);

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
    await onSave(payload);
    setHasChanges(false);
  };

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
        label={label}
        value={currentValue}
        onChange={(event) => handleChange(key, event.target.value)}
        placeholder={description}
        disabled={busy}
      />
    );
  };

  const basicSettings = settings?.settings.filter((s) =>
    ["port", "address", "audio_sink", "encoder", "capture", "output_name", "system_tray", "upnp"].includes(s.key)
  ) ?? [];

  const advancedSettings = settings?.settings.filter((s) =>
    !["port", "address", "audio_sink", "encoder", "capture", "output_name", "system_tray", "upnp"].includes(s.key)
  ) ?? [];

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
          <p className="text-gray-400">Loading settings...</p>
        ) : (
          <div className="space-y-6">
            <div className="space-y-4">
              <h4 className="text-sm font-display text-neon-lime uppercase tracking-wider">Basic</h4>
              {basicSettings.map(renderInput)}
            </div>

            {advancedSettings.length > 0 && (
              <div className="space-y-4">
                <h4 className="text-sm font-display text-neon-lime uppercase tracking-wider">Advanced</h4>
                {advancedSettings.map(renderInput)}
              </div>
            )}

            {hasChanges && (
              <div className="text-xs text-neon-cyan bg-neon-cyan/10 p-2 rounded">
                You have unsaved changes.
              </div>
            )}

            <div className="flex gap-3 pt-2">
              <Button
                variant="primary"
                onClick={handleSave}
                disabled={busy || !hasChanges}
              >
                {busy ? "Saving..." : "Save Settings"}
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
