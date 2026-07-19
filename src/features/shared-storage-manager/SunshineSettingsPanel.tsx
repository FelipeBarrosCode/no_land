import { useState, useEffect } from "react";
import { BlockingLoaderOverlay, type BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { InputField } from "../../components/ui/InputField";
import type {
  EmbeddedMoonlightInstanceStatus,
  MoonlightPairingSessionResponse,
  SunshineSetting,
  SunshineSettingsResponse,
} from "../../lib/types";

interface Props {
  settings: SunshineSettingsResponse | null;
  busy: boolean;
  instanceLabel: string;
  embeddedStatus: EmbeddedMoonlightInstanceStatus | null;
  activePairing: MoonlightPairingSessionResponse | null;
  defaultUsername: string;
  defaultPassword: string;
  onLoad: (sunshineUsername: string, sunshinePassword: string) => Promise<void>;
  onSave: (settings: Record<string, unknown>, sunshineUsername: string, sunshinePassword: string) => Promise<void>;
  onReset: (sunshineUsername: string, sunshinePassword: string) => Promise<void>;
  onSetEmbeddedEnabled: (enabled: boolean) => Promise<void>;
  onPrepareEmbeddedPairing: () => Promise<void>;
  onCompleteEmbeddedPairing: (sessionId: string) => Promise<void>;
  onClose: () => void;
}

export function SunshineSettingsPanel({
  settings,
  busy,
  instanceLabel,
  embeddedStatus,
  activePairing,
  defaultUsername,
  defaultPassword,
  onLoad,
  onSave,
  onReset,
  onSetEmbeddedEnabled,
  onPrepareEmbeddedPairing,
  onCompleteEmbeddedPairing,
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
    <div className="space-y-4 rounded border border-[#3a4068] bg-[#0c1224] p-4">
      <div className="flex items-center justify-between mb-1">
        <div>
          <h3 className="text-lg font-display text-neon-cyan">Instance Settings</h3>
          <p className="text-xs text-[#9bb4d7] mt-1">{instanceLabel}</p>
        </div>
        <Button variant="ghost" onClick={onClose} disabled={busy}>
          Collapse
        </Button>
      </div>

        <div className="mb-6 rounded border border-neon-cyan/30 bg-[#081120] p-4">
          <div className="flex items-start justify-between gap-3">
            <div>
              <h4 className="font-display text-sm uppercase tracking-wider text-neon-cyan">
                Embedded Moonlight Pipeline
              </h4>
              <p className="mt-1 text-sm text-[#bfd3ee]">
                Use Noland&apos;s built-in Moonlight runtime for this instance instead of the external Moonlight app flow.
              </p>
            </div>
            <Button
              variant={embeddedStatus?.enabled ? "secondary" : "primary"}
              onClick={() => onSetEmbeddedEnabled(!(embeddedStatus?.enabled ?? false))}
              disabled={busy}
            >
              {embeddedStatus?.enabled ? "Disable" : "Enable"}
            </Button>
          </div>

          <div className="mt-3 grid gap-2 text-xs text-[#9bb4d7] md:grid-cols-2">
            <p>Enabled: <span className="text-white">{embeddedStatus?.enabled ? "Yes" : "No"}</span></p>
            <p>Paired: <span className="text-white">{embeddedStatus?.paired ? "Yes" : "No"}</span></p>
            <p>Session: <span className="text-white">{embeddedStatus?.sessionState ?? "idle"}</span></p>
            <p>Host: <span className="text-white">{embeddedStatus?.hostAddress || "Not resolved"}</span></p>
          </div>

          {embeddedStatus?.lastError && (
            <div className="mt-3 rounded border border-red-500/30 bg-red-950/20 p-2 text-xs text-red-200">
              {embeddedStatus.lastError}
            </div>
          )}

          {embeddedStatus?.enabled && !embeddedStatus.paired && (
            <div className="mt-4 space-y-3 rounded border border-[#3a4068] bg-[#0b1325] p-3">
              <p className="text-sm text-[#bfd3ee]">
                Pair the embedded Moonlight client with Sunshine before using Play.
              </p>
              {!activePairing ? (
                <Button variant="primary" onClick={onPrepareEmbeddedPairing} disabled={busy}>
                  Start Pairing
                </Button>
              ) : (
                <div className="space-y-3">
                  <div className="rounded border border-neon-lime/30 bg-neon-lime/10 p-3">
                    <p className="text-xs uppercase tracking-wide text-neon-lime">Pairing PIN</p>
                    <p className="mt-1 font-display text-2xl text-white">{activePairing.pin}</p>
                    <p className="mt-2 text-xs text-[#bfd3ee]">
                      Enter this PIN in Sunshine&apos;s pairing prompt, then click Complete Pairing.
                    </p>
                  </div>
                  <Button
                    variant="secondary"
                    onClick={() => onCompleteEmbeddedPairing(activePairing.sessionId)}
                    disabled={busy}
                  >
                    Complete Pairing
                  </Button>
                </div>
              )}
            </div>
          )}
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
    </div>
  );
}
