import { useEffect, useState } from "react";
import { getInstanceAutoShutdownStatus } from "../../lib/backend";
import { Button } from "../../components/ui/Button";
import { InputField } from "../../components/ui/InputField";
import type {
  AutoShutdownSettings as AutoShutdownSettingsValue,
  AutoShutdownState,
  LifecycleAgentStatus,
} from "../../lib/types";

const TIMEOUT_OPTIONS = [1, 2, 3, 4, 6, 8] as const;

interface Props {
  state: AutoShutdownState;
  busy: boolean;
  hasActiveStorageProfile: boolean;
  hasVastApiKey: boolean;
  hasProvisionedServer: boolean;
  instanceId: number | null;
  onSave: (settings: AutoShutdownSettingsValue) => Promise<void>;
}

function timeoutModeFor(hours: number): string {
  return TIMEOUT_OPTIONS.includes(hours as (typeof TIMEOUT_OPTIONS)[number])
    ? hours.toString()
    : "custom";
}

function formatStatus(status: string): string {
  return status.split("_").join(" ");
}

function formatLastRun(value: string | null): string {
  if (!value) {
    return "Never";
  }

  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

export function AutoShutdownSettings({
  state,
  busy,
  hasActiveStorageProfile,
  hasVastApiKey,
  hasProvisionedServer,
  instanceId,
  onSave,
}: Props) {
  const [enabled, setEnabled] = useState(state.settings.enabled);
  const [timeoutMode, setTimeoutMode] = useState(() =>
    timeoutModeFor(state.settings.inactivityHours),
  );
  const [customHours, setCustomHours] = useState(
    state.settings.inactivityHours.toString(),
  );
  const [backupAppLimit, setBackupAppLimit] = useState(
    state.settings.backupAppLimit.toString(),
  );
  const [runtimeStatus, setRuntimeStatus] = useState<LifecycleAgentStatus | null>(null);

  useEffect(() => {
    setEnabled(state.settings.enabled);
    setTimeoutMode(timeoutModeFor(state.settings.inactivityHours));
    setCustomHours(state.settings.inactivityHours.toString());
    setBackupAppLimit(state.settings.backupAppLimit.toString());
  }, [state]);

  useEffect(() => {
    if (instanceId == null) {
      setRuntimeStatus(null);
      return;
    }

    let cancelled = false;
    const refresh = async () => {
      try {
        const status = await getInstanceAutoShutdownStatus(instanceId);
        if (!cancelled) {
          setRuntimeStatus(status);
        }
      } catch {
        if (!cancelled) {
          setRuntimeStatus(null);
        }
      }
    };
    void refresh();
    const interval = window.setInterval(() => void refresh(), 10_000);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [instanceId, state.settings.enabled]);

  const inactivityHours =
    timeoutMode === "custom" ? Number(customHours) : Number(timeoutMode);
  const parsedBackupAppLimit = Number(backupAppLimit);
  const inactivityHoursInvalid =
    !Number.isFinite(inactivityHours) ||
    inactivityHours < 0.25 ||
    inactivityHours > 24;
  const backupAppLimitInvalid =
    !Number.isInteger(parsedBackupAppLimit) ||
    parsedBackupAppLimit < 1 ||
    parsedBackupAppLimit > 10;
  const enablingBlocked =
    enabled &&
    (!hasActiveStorageProfile || !hasVastApiKey || !hasProvisionedServer);

  return (
    <section className="rounded-md border border-[#3b4067] bg-[#10152f] p-4">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="max-w-3xl">
          <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
            Automatic Backup & Shutdown
          </h3>
          <p className="mt-2 text-[1.1rem] leading-snug text-[#a8bed6]">
            After continuous inactivity, No Land backs up your top-used apps to
            the active shared-storage profile, then releases the cloud instance
            to stop further instance charges. This feature is disabled by
            default.
          </p>
        </div>

        <label className="flex items-center gap-3 rounded border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-[1.05rem] text-[#dff8ff]">
          <input
            type="checkbox"
            className="h-4 w-4 accent-cyan-400"
            checked={enabled}
            disabled={busy || (!hasActiveStorageProfile && !enabled)}
            onChange={(event) => setEnabled(event.currentTarget.checked)}
          />
          <span>Enable automatic backup & shutdown</span>
        </label>
      </div>

      {!hasActiveStorageProfile ? (
        <div className="mt-4 border border-amber-400/40 bg-amber-950/30 p-3 text-[1.05rem] leading-snug text-amber-200">
          Connect and activate a shared-storage profile above before enabling
          this feature. Storage credentials and a repository key are verified
          again when you save.
        </div>
      ) : null}

      <div className="mt-4 grid gap-4 md:grid-cols-2">
        <label className="flex flex-col gap-2 text-base">
          <span className="font-display text-[10px] uppercase tracking-[0.14em] text-[#9ad9ff]">
            Inactivity timeout
          </span>
          <select
            value={timeoutMode}
            disabled={busy}
            onChange={(event) => setTimeoutMode(event.currentTarget.value)}
            className="border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-[1.2rem] leading-none text-[#dff8ff] outline-none transition focus:border-neon-cyan focus:shadow-[inset_0_0_0_2px_#121731,0_0_0_2px_rgba(68,214,255,0.28)] disabled:cursor-not-allowed disabled:opacity-50"
          >
            {TIMEOUT_OPTIONS.map((hours) => (
              <option key={hours} value={hours}>
                {hours} {hours === 1 ? "hour" : "hours"}
              </option>
            ))}
            <option value="custom">Custom</option>
          </select>
          <p className="text-[1rem] leading-snug text-[#8fa9c8]">
            The timer resets whenever activity is detected.
          </p>
        </label>

        {timeoutMode === "custom" ? (
          <InputField
            label="Custom timeout (0.25–24 hours)"
            type="number"
            min={0.25}
            max={24}
            step={0.25}
            value={customHours}
            disabled={busy}
            error={
              inactivityHoursInvalid
                ? "Enter a finite value from 0.25 to 24"
                : undefined
            }
            onChange={(event) => setCustomHours(event.currentTarget.value)}
          />
        ) : (
          <div className="rounded border border-[#30385d] bg-[#0b0f23]/60 p-3 text-[1.05rem] text-[#8fa9c8]">
            Shutdown starts after {inactivityHours} continuous inactive
            {inactivityHours === 1 ? " hour" : " hours"}.
          </div>
        )}

        <div>
          <InputField
            label="Top apps to back up (1–10)"
            type="number"
            min={1}
            max={10}
            step={1}
            value={backupAppLimit}
            disabled={busy}
            error={
              backupAppLimitInvalid
                ? "Enter a whole number from 1 to 10"
                : undefined
            }
            onChange={(event) => setBackupAppLimit(event.currentTarget.value)}
          />
          <p className="mt-1 text-[1rem] leading-snug text-[#8fa9c8]">
            No Land will select up to this many of the most-used apps for the
            automatic backup.
          </p>
        </div>
      </div>

      <div className="mt-4 grid gap-2 rounded border border-[#30385d] bg-[#0b0f23]/60 p-3 text-[1.05rem] md:grid-cols-3">
        <p className="text-[#a8bed6]">
          Last status:{" "}
          <span className="capitalize text-[#dff8ff]">
            {formatStatus(state.lastStatus)}
          </span>
        </p>
        <p className="text-[#a8bed6]">
          Last run:{" "}
          <span className="text-[#dff8ff]">{formatLastRun(state.lastRunAt)}</span>
        </p>
        <p className="text-[#a8bed6]">
          Prerequisites:{" "}
          <span className="text-[#dff8ff]">
            {hasActiveStorageProfile && hasVastApiKey && hasProvisionedServer
              ? "Ready"
              : "Setup required"}
          </span>
        </p>
      </div>

      {runtimeStatus ? (
        <div className="mt-3 rounded border border-cyan-400/30 bg-cyan-950/20 p-3 text-[1.05rem] text-cyan-100">
          <div className="flex flex-wrap gap-x-5 gap-y-1">
            <span>Runtime: {runtimeStatus.state}</span>
            <span>
              Time remaining: {Math.ceil(runtimeStatus.timeRemainingMs / 60_000)} min
            </span>
            <span>Tracked apps: {runtimeStatus.rankedApps.length}</span>
          </div>
          {runtimeStatus.rankedApps.length > 0 ? (
            <p className="mt-2 text-[1rem] text-cyan-200/80">
              Current ranking: {runtimeStatus.rankedApps.map((app) => app.appId).join(", ")}
            </p>
          ) : null}
          {runtimeStatus.lastError ? (
            <p className="mt-2 text-amber-200">Runtime warning: {runtimeStatus.lastError}</p>
          ) : null}
        </div>
      ) : null}

      {state.lastError ? (
        <p className="mt-3 border border-red-500/40 bg-red-950/30 p-3 text-[1.05rem] leading-snug text-red-200">
          Last error: {state.lastError}
        </p>
      ) : null}

      <div className="mt-4 flex flex-wrap items-center gap-3">
        <Button
          disabled={
            busy ||
            inactivityHoursInvalid ||
            backupAppLimitInvalid ||
            enablingBlocked
          }
          onClick={() =>
            onSave({
              enabled,
              inactivityHours,
              backupAppLimit: parsedBackupAppLimit,
            })
          }
        >
          Save Automatic Backup Settings
        </Button>
        {!hasVastApiKey ? (
          <span className="text-[1rem] text-amber-200">
            A Vast.ai API key is required before enabling.
          </span>
        ) : null}
        {!hasProvisionedServer ? (
          <span className="text-[1rem] text-amber-200">
            At least one provisioned server is required before enabling.
          </span>
        ) : null}
      </div>
    </section>
  );
}
