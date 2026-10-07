import { translate, translateSource, useLocalization } from "../../lib/i18n";
import { useEffect, useState } from "react";
import { getInstanceAutoShutdownStatus } from "../../lib/backend";
import { Button } from "../../components/ui/Button";
import { InputField } from "../../components/ui/InputField";
import type {
  AutoShutdownSettings as AutoShutdownSettingsValue,
  AutoShutdownState,
  LifecycleAgentStatus,
} from "../../lib/types";

const TIMEOUT_OPTIONS = [
  { value: 1 / 12, label: "5 minutes" },
  { value: 1, label: "1 hour" },
  { value: 2, label: "2 hours" },
  { value: 3, label: "3 hours" },
  { value: 4, label: "4 hours" },
  { value: 6, label: "6 hours" },
  { value: 8, label: "8 hours" },
] as const;

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
  const option = TIMEOUT_OPTIONS.find(({ value }) => Math.abs(value - hours) < 0.0001);
  return option ? option.value.toString() : "custom";
}

function formatStatus(status: string): string {
  return status.split("_").join(" ");
}

function formatLastRun(value: string | null, formatDate: (value: Date) => string): string {
  if (!value) {
    return translate("generated.6300ef800bb88429");
  }

  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : formatDate(parsed);
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
  const { formatDate } = useLocalization();
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
    inactivityHours < 1 / 12 ||
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
            {translate("generated.f2456fbc93e36a09")}
          </h3>
          <p className="mt-2 text-[1.1rem] leading-snug text-[#a8bed6]">
            {translate("generated.3c8d70539066621e")}
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
          <span>{translate("generated.b4ff14f408a85202")}</span>
        </label>
      </div>

      {!hasActiveStorageProfile ? (
        <div className="mt-4 border border-amber-400/40 bg-amber-950/30 p-3 text-[1.05rem] leading-snug text-amber-200">
          {translate("generated.953e47f2f6f5d8c2")}
        </div>
      ) : null}

      <div className="mt-4 grid gap-4 md:grid-cols-2">
        <label className="flex flex-col gap-2 text-base">
          <span className="font-display text-[10px] uppercase tracking-[0.14em] text-[#9ad9ff]">
            {translate("generated.d96b63117f00e362")}
          </span>
          <select
            value={timeoutMode}
            disabled={busy}
            onChange={(event) => setTimeoutMode(event.currentTarget.value)}
            className="border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-[1.2rem] leading-none text-[#dff8ff] outline-none transition focus:border-neon-cyan focus:shadow-[inset_0_0_0_2px_#121731,0_0_0_2px_rgba(68,214,255,0.28)] disabled:cursor-not-allowed disabled:opacity-50"
          >
            {TIMEOUT_OPTIONS.map(({ value, label }) => (
              <option key={value} value={value}>
                {translateSource(label)}
              </option>
            ))}
            <option value="custom">{translate("generated.494ca78f7374e46f")}</option>
          </select>
          <p className="text-[1rem] leading-snug text-[#8fa9c8]">
            {translate("generated.789d7db22ba48a3a")}
          </p>
        </label>

        {timeoutMode === "custom" ? (
          <InputField
            label={translate("generated.19fddf42c94ff940")}
            type="number"
            min={1 / 12}
            max={24}
            step={1 / 12}
            value={customHours}
            disabled={busy}
            error={
              inactivityHoursInvalid
                ? translateSource("Enter a finite value from 5 minutes to 24 hours")
                : undefined
            }
            onChange={(event) => setCustomHours(event.currentTarget.value)}
          />
        ) : (
          <div className="rounded border border-[#30385d] bg-[#0b0f23]/60 p-3 text-[1.05rem] text-[#8fa9c8]">
            {translate("generated.f753a7568dcf82d0")} {inactivityHours} {translate("generated.1ad85be7aa101f0c")}
            {inactivityHours === 1 ? translate("generated.9ac0add475dd38e6") : translate("generated.404314b1f4bd8fa2")}.
          </div>
        )}

        <div>
          <InputField
            label={translate("generated.e9b821e096de8190")}
            type="number"
            min={1}
            max={10}
            step={1}
            value={backupAppLimit}
            disabled={busy}
            error={
              backupAppLimitInvalid
                ? translateSource("Enter a whole number from 1 to 10")
                : undefined
            }
            onChange={(event) => setBackupAppLimit(event.currentTarget.value)}
          />
          <p className="mt-1 text-[1rem] leading-snug text-[#8fa9c8]">
            {translate("generated.61b46bc620b7f758")}
          </p>
        </div>
      </div>

      <div className="mt-4 grid gap-2 rounded border border-[#30385d] bg-[#0b0f23]/60 p-3 text-[1.05rem] md:grid-cols-3">
        <p className="text-[#a8bed6]">
          {translate("generated.061f04c97492547c")}{" "}
          <span className="capitalize text-[#dff8ff]">
            {formatStatus(state.lastStatus)}
          </span>
        </p>
        <p className="text-[#a8bed6]">
          {translate("generated.f636f90bbb7941ad")}{" "}
          <span className="text-[#dff8ff]">{formatLastRun(state.lastRunAt, (date) => formatDate(date, { dateStyle: "medium", timeStyle: "short" }))}</span>
        </p>
        <p className="text-[#a8bed6]">
          {translate("generated.376438316e1b758e")}{" "}
          <span className="text-[#dff8ff]">
            {hasActiveStorageProfile && hasVastApiKey && hasProvisionedServer
              ? translate("generated.5fa7aac5375c5815")
              : translate("generated.1a367c79441e13b7")}
          </span>
        </p>
      </div>

      {runtimeStatus ? (
        <div className="mt-3 rounded border border-cyan-400/30 bg-cyan-950/20 p-3 text-[1.05rem] text-cyan-100">
          <div className="flex flex-wrap gap-x-5 gap-y-1">
            <span>{translate("generated.2997436a97cea98a")} {runtimeStatus.state}</span>
            <span>
              {translate("generated.9b6d3c5c8412c3bd")} {Math.ceil(runtimeStatus.timeRemainingMs / 60_000)} {translate("generated.1f6fa6f69d185e60")}
            </span>
            <span>{translate("generated.8b29a07bfb78e991")} {runtimeStatus.rankedApps.length}</span>
          </div>
          {runtimeStatus.rankedApps.length > 0 ? (
            <p className="mt-2 text-[1rem] text-cyan-200/80">
              {translate("generated.3f94f7b19f393260")} {runtimeStatus.rankedApps.map((app) => app.appId).join(", ")}
            </p>
          ) : null}
          {runtimeStatus.lastError ? (
            <p className="mt-2 text-amber-200">{translate("generated.24a3e3af823c7da4")} {translateSource(runtimeStatus.lastError)}</p>
          ) : null}
        </div>
      ) : null}

      {state.lastError ? (
        <p className="mt-3 border border-red-500/40 bg-red-950/30 p-3 text-[1.05rem] leading-snug text-red-200">
          {translate("generated.af73af0200229dfd")} {translateSource(state.lastError)}
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
          {translate("generated.1a89dd84d87e7b5d")}
        </Button>
        {!hasVastApiKey ? (
          <span className="text-[1rem] text-amber-200">
            {translate("generated.e3cb540b1c2d0046")}
          </span>
        ) : null}
        {!hasProvisionedServer ? (
          <span className="text-[1rem] text-amber-200">
            {translate("generated.dfbd219f2ca344bc")}
          </span>
        ) : null}
      </div>
    </section>
  );
}
