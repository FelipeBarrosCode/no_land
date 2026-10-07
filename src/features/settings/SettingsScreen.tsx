import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import { errorMessage } from "../../lib/errorMessage";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { APP_PROMPTS } from "../../prompts/appPrompts";
import { ArcadeSoundToggle } from "../../components/ui/ArcadeSoundToggle";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { SharedStorageSettingsV2 } from "../shared-storage/SharedStorageSettingsV2";
import { AutoShutdownSettings } from "./AutoShutdownSettings";
import { NotificationSettings } from "./NotificationSettings";
import { LOCALE_OPTIONS, translateSource, useLocalization, type LocalePreference, translate } from "../../lib/i18n";
import {
  getInstanceConnectionStatus,
  repairInstanceConnection,
  setInstanceConnectionPreference,
} from "../../lib/backend";
import type {
  AutoShutdownSettings as AutoShutdownSettingsValue,
  MoonlightPreferences,
  PlatformCredentialsUpdate,
  IgdbCredentialsUpdate,
  PersistedAppState,
  ProfileReference,
  ProviderDefinition,
  ServerPreferencesUpdate,
  SharedStorageTestResult,
  SshCredentialsUpdate,
  CloudflareTurnSettingsResponse,
  CloudflareTurnSettingsUpdate,
  CloudflareTurnTestResult,
  ConnectionPreference,
  InstanceConnectionStatusResponse,
} from "../../lib/types";
import {
  VAST_API_KEY_URL,
  VAST_BILLING_URL,
  VAST_LOGIN_URL,
} from "../../lib/constants";

type SettingsSection =
  | "profile"
  | "server"
  | "client"
  | "storage"
  | "connection"
  | "notifications"
  | "language";
type ClientForm = {
  bitrate: string;
  fps: string;
  refreshRateMode: string;
  width: string;
  height: string;
  displayOutput: string;
  aspectRatio: string;
  hostaudio: string;
  showperfoverlay: string;
  keepawake: string;
  framepacing: string;
  vsync: string;
  hdr: string;
  videocfg: string;
  videodec: string;
  yuv444: string;
  gameopts: string;
  gamepadmouse: string;
  detectnetblocking: string;
  showInputDebugHud: string;
};

interface Props {
  appState: PersistedAppState;
  busy: boolean;
  storageProviders: ProviderDefinition[];
  sharedStorageProfiles: ProfileReference[];
  sharedStorageTestResult: SharedStorageTestResult | null;
  onLoadStorageProviders: () => Promise<void>;
  onConnectStorageProvider: (
    provider: string,
    credentials: Record<string, string>,
    bucket: string | null,
    prefix: string | null,
    displayName: string,
  ) => Promise<void>;
  onTestStorageConnection: (profileId: string) => Promise<void>;
  onLoadSharedStorageProfiles: () => Promise<void>;
  onSetActiveStorageProfile: (profileId: string) => Promise<void>;
  onDisconnectStorageProfile: (profileId: string) => Promise<void>;
  oauthSessionId: string | null;
  onBeginOauthFlow: (
    provider: string,
    displayName: string,
    clientId?: string,
    clientSecret?: string | null,
    providerFields?: Record<string, string>,
  ) => Promise<string | null>;
  onCompleteOauthFlow: (sessionId: string) => Promise<void>;
  onCancelOauthFlow: (sessionId: string) => Promise<void>;
  onSaveApiKey: (apiKey: string) => Promise<void>;
  onSavePlatformCredentials: (
    payload: PlatformCredentialsUpdate,
  ) => Promise<void>;
  onSaveIgdbCredentials: (payload: IgdbCredentialsUpdate) => Promise<void>;
  onSaveAutoShutdownSettings: (
    settings: AutoShutdownSettingsValue,
  ) => Promise<void>;
  onSaveServerPreferences: (
    payload: Partial<ServerPreferencesUpdate>,
  ) => Promise<void>;
  onSaveMoonlightPreferences: (payload: MoonlightPreferences) => Promise<void>;
  onSaveSshCredentials: (payload: SshCredentialsUpdate) => Promise<void>;
  cloudflareTurnSettings: CloudflareTurnSettingsResponse | null;
  cloudflareTurnTestResult: CloudflareTurnTestResult | null;
  onLoadCloudflareTurnSettings: () => Promise<void>;
  onTestCloudflareTurnSettings: (
    payload: CloudflareTurnSettingsUpdate,
  ) => Promise<CloudflareTurnTestResult | null>;
  onSaveCloudflareTurnSettings: (
    payload: CloudflareTurnSettingsUpdate,
  ) => Promise<void>;
  onClearCloudflareTurnSettings: () => Promise<void>;
  onRegenerateEdid: (payload: {
    mode: "auto_detect" | "mac_hardware" | "manual";
    refreshRateHz: number;
  }) => Promise<void>;
}

function toNumber(value: string, fallback: number): number {
  const parsed = Number.parseFloat(value);
  if (Number.isNaN(parsed)) {
    return fallback;
  }

  return parsed;
}

type SelectOption = {
  value: string;
  label: string;
};

const binaryOptions: SelectOption[] = [
  { value: "0", label: "Disabled" },
  { value: "1", label: "Enabled" },
];

const hostAudioOptions: SelectOption[] = [
  { value: "0", label: "Play locally" },
  { value: "1", label: "Play on cloud machine" },
];

const codecOptions: SelectOption[] = [
  { value: "0", label: "Automatic" },
  { value: "1", label: "Force H.264" },
  { value: "2", label: "Force HEVC (H.265)" },
  { value: "3", label: "Force AV1" },
];

const decoderOptions: SelectOption[] = [
  { value: "0", label: "Automatic" },
  { value: "1", label: "Force software decode" },
  { value: "2", label: "Force hardware decode" },
];

function SettingsSubsection({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="rounded-md border border-[#3b4067] bg-[#10152f] p-4">
      <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
        {title}
      </h3>
      {description ? (
        <p className="mt-1 text-[1.1rem] text-[#a8bed6]">{description}</p>
      ) : null}
      <div className="mt-4">{children}</div>
    </div>
  );
}

function SettingHelp({ children }: { children: React.ReactNode }) {
  return <p className="mt-1 text-[1rem] leading-snug text-[#8fa9c8]">{children}</p>;
}

function SelectField({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-2 text-base">
      <span className="font-display text-[10px] uppercase tracking-[0.14em] text-[#9ad9ff]">
        {label}
      </span>
      <select
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className="border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-[1.2rem] leading-none text-[#dff8ff] outline-none transition focus:border-neon-cyan focus:shadow-[inset_0_0_0_2px_#121731,0_0_0_2px_rgba(68,214,255,0.28)]"
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {translateSource(option.label)}
          </option>
        ))}
      </select>
    </label>
  );
}

export function SettingsScreen({
  appState,
  busy,
  storageProviders,
  sharedStorageProfiles,
  sharedStorageTestResult,
  onLoadStorageProviders,
  onConnectStorageProvider,
  onTestStorageConnection,
  onLoadSharedStorageProfiles,
  onSetActiveStorageProfile,
  onDisconnectStorageProfile,
  oauthSessionId,
  onBeginOauthFlow,
  onCompleteOauthFlow,
  onCancelOauthFlow,
  onSaveApiKey,
  onSavePlatformCredentials,
  onSaveIgdbCredentials,
  onSaveAutoShutdownSettings,
  onSaveServerPreferences,
  onSaveMoonlightPreferences,
  onSaveSshCredentials,
  cloudflareTurnSettings,
  cloudflareTurnTestResult,
  onLoadCloudflareTurnSettings,
  onTestCloudflareTurnSettings,
  onSaveCloudflareTurnSettings,
  onClearCloudflareTurnSettings,
  onRegenerateEdid,
}: Props) {
  const { preference, setLocale, t } = useLocalization();
  const [searchParams] = useSearchParams();
  const [section, setSection] = useState<SettingsSection>(() =>
    searchParams.get("section") === "storage" ? "storage" : "profile",
  );
  const [apiKey, setApiKey] = useState(appState.credentials.vastApiKey);
  const [platformUsername, setPlatformUsername] = useState(
    appState.credentials.appUsername,
  );
  const [platformPassword, setPlatformPassword] = useState(
    appState.credentials.appPassword,
  );
  const [sshUsername, setSshUsername] = useState(
    appState.ssh.sshUsername || appState.credentials.appUsername,
  );
  const [twitchClientId, setTwitchClientId] = useState(
    appState.credentials.twitchClientId,
  );
  const [twitchClientSecret, setTwitchClientSecret] = useState(
    appState.credentials.twitchClientSecret,
  );
  const [sshPassword, setSshPassword] = useState(
    appState.ssh.sshPassword || appState.credentials.appPassword,
  );
  const [edidMode, setEdidMode] = useState<"auto_detect" | "mac_hardware" | "manual">(
    appState.sunshine.edidMode,
  );
  const [edidRefreshRateHz, setEdidRefreshRateHz] = useState(
    appState.sunshine.edidRefreshRateHz.toString(),
  );
  const [turnEnabled, setTurnEnabled] = useState(
    cloudflareTurnSettings?.enabled ?? appState.cloudflareTurn.enabled,
  );
  const [turnKeyId, setTurnKeyId] = useState("");
  const [turnApiToken, setTurnApiToken] = useState("");
  const [connectionStatuses, setConnectionStatuses] = useState<
    Record<number, InstanceConnectionStatusResponse>
  >({});
  const [switchingInstanceId, setSwitchingInstanceId] = useState<number | null>(null);
  const [connectionStatusError, setConnectionStatusError] = useState<string | null>(null);

  const [serverForm, setServerForm] = useState({
    minReliability: appState.serverPreferences.minReliability.toString(),
    storageGb: appState.serverPreferences.storageGb.toString(),
    templateHash: appState.serverPreferences.templateHash,
  });

  const [clientForm, setClientForm] = useState<ClientForm>(() => ({
    bitrate: appState.moonlightPreferences.bitrate.toString(),
    fps: appState.moonlightPreferences.fps.toString(),
    refreshRateMode: appState.moonlightPreferences.refreshRateMode,
    width: appState.moonlightPreferences.width.toString(),
    height: appState.moonlightPreferences.height.toString(),
    displayOutput: appState.moonlightPreferences.displayOutput ?? "",
    aspectRatio: appState.moonlightPreferences.aspectRatio ?? "",
    hostaudio: appState.moonlightPreferences.hostaudio.toString(),
    showperfoverlay: appState.moonlightPreferences.showperfoverlay.toString(),
    keepawake: appState.moonlightPreferences.keepawake.toString(),
    framepacing: appState.moonlightPreferences.framepacing.toString(),
    vsync: appState.moonlightPreferences.vsync.toString(),
    hdr: appState.moonlightPreferences.hdr.toString(),
    videocfg: appState.moonlightPreferences.videocfg.toString(),
    videodec: appState.moonlightPreferences.videodec.toString(),
    yuv444: appState.moonlightPreferences.yuv444.toString(),
    gameopts: appState.moonlightPreferences.gameopts.toString(),
    gamepadmouse: appState.moonlightPreferences.gamepadmouse.toString(),
    detectnetblocking:
      appState.moonlightPreferences.detectnetblocking.toString(),
    showInputDebugHud:
      appState.moonlightPreferences.showInputDebugHud.toString(),
  }));

  useEffect(() => {
    setApiKey(appState.credentials.vastApiKey);
    setPlatformUsername(appState.credentials.appUsername);
    setPlatformPassword(appState.credentials.appPassword);
    setSshUsername(
      appState.ssh.sshUsername || appState.credentials.appUsername,
    );
    setTwitchClientId(appState.credentials.twitchClientId);
    setTwitchClientSecret(appState.credentials.twitchClientSecret);
    setSshPassword(
      appState.ssh.sshPassword || appState.credentials.appPassword,
    );
    setEdidMode(appState.sunshine.edidMode);
    setEdidRefreshRateHz(appState.sunshine.edidRefreshRateHz.toString());
    setServerForm({
      minReliability: appState.serverPreferences.minReliability.toString(),
      storageGb: appState.serverPreferences.storageGb.toString(),
      templateHash: appState.serverPreferences.templateHash,
    });
    setClientForm({
      bitrate: appState.moonlightPreferences.bitrate.toString(),
      fps: appState.moonlightPreferences.fps.toString(),
      refreshRateMode: appState.moonlightPreferences.refreshRateMode,
      width: appState.moonlightPreferences.width.toString(),
      height: appState.moonlightPreferences.height.toString(),
      displayOutput: appState.moonlightPreferences.displayOutput ?? "",
      aspectRatio: appState.moonlightPreferences.aspectRatio ?? "",
      hostaudio: appState.moonlightPreferences.hostaudio.toString(),
      showperfoverlay: appState.moonlightPreferences.showperfoverlay.toString(),
      keepawake: appState.moonlightPreferences.keepawake.toString(),
      framepacing: appState.moonlightPreferences.framepacing.toString(),
      vsync: appState.moonlightPreferences.vsync.toString(),
      hdr: appState.moonlightPreferences.hdr.toString(),
      videocfg: appState.moonlightPreferences.videocfg.toString(),
      videodec: appState.moonlightPreferences.videodec.toString(),
      yuv444: appState.moonlightPreferences.yuv444.toString(),
      gameopts: appState.moonlightPreferences.gameopts.toString(),
      gamepadmouse: appState.moonlightPreferences.gamepadmouse.toString(),
      detectnetblocking:
        appState.moonlightPreferences.detectnetblocking.toString(),
      showInputDebugHud:
        appState.moonlightPreferences.showInputDebugHud.toString(),
    });
  }, [appState]);

  useEffect(() => {
    void onLoadCloudflareTurnSettings();
  }, [onLoadCloudflareTurnSettings]);

  useEffect(() => {
    setTurnEnabled(cloudflareTurnSettings?.enabled ?? appState.cloudflareTurn.enabled);
  }, [appState.cloudflareTurn.enabled, cloudflareTurnSettings]);

  useEffect(() => {
    let cancelled = false;
    async function loadStatuses() {
      const results = await Promise.allSettled(
        appState.provisionedServers.map((server) =>
          getInstanceConnectionStatus(server.instanceId),
        ),
      );
      if (cancelled) return;
      const next: Record<number, InstanceConnectionStatusResponse> = {};
      for (const result of results) {
        if (result.status === "fulfilled") {
          next[result.value.instanceId] = result.value;
        }
      }
      setConnectionStatuses(next);
    }
    void loadStatuses();
    return () => {
      cancelled = true;
    };
  }, [appState.provisionedServers]);

  async function changeConnectionPreference(
    instanceId: number,
    preference: ConnectionPreference,
  ) {
    setSwitchingInstanceId(instanceId);
    setConnectionStatusError(null);
    try {
      const status = await setInstanceConnectionPreference(instanceId, preference);
      setConnectionStatuses((current) => ({ ...current, [instanceId]: status }));
    } catch (error) {
      setConnectionStatusError(errorMessage(error));
    } finally {
      setSwitchingInstanceId(null);
    }
  }

  async function repairConnection(instanceId: number) {
    setSwitchingInstanceId(instanceId);
    setConnectionStatusError(null);
    try {
      const status = await repairInstanceConnection(instanceId);
      setConnectionStatuses((current) => ({ ...current, [instanceId]: status }));
    } catch (error) {
      setConnectionStatusError(errorMessage(error));
    } finally {
      setSwitchingInstanceId(null);
    }
  }


  async function openExternalUrl(url: string) {
    try {
      await openUrl(url);
    } catch {
      window.open(url, "_blank", "noopener,noreferrer");
    }
  }

  const profilePanel = (
    <Card className="pixel-frame min-w-0 overflow-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        {translate("generated.d696a35bdd1883da")}
      </h2>
      <div className="mt-4 grid gap-3 md:grid-cols-2">
        <InputField
          label={translate("generated.c08537e7f04a3efa")}
          value={platformUsername}
          onChange={(event) => setPlatformUsername(event.target.value)}
        />
        <InputField
          label={translate("generated.05b11713ab17a2e3")}
          type="password"
          value={platformPassword}
          onChange={(event) => setPlatformPassword(event.target.value)}
        />
      </div>
      <div className="mt-3">
        <Button
          disabled={
            busy ||
            platformUsername.trim().length < 3 ||
            platformPassword.trim().length < 6
          }
          onClick={() =>
            onSavePlatformCredentials({
              appUsername: platformUsername.trim(),
              appPassword: platformPassword.trim(),
            })
          }
        >
          {translate("generated.40d82323d391ccb1")}
        </Button>
      </div>
      <div className="mt-4 border-t border-[#3b4067] pt-4">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          {translate("generated.96e6b34fbf9d39dd")}
        </h3>
        <p className="mt-1 text-[1.1rem] text-[#a8bed6]">
          {translate("generated.9bd3d7ccb5f02d10")}
        </p>
        <div className="mt-3 grid gap-3 md:grid-cols-2">
          <InputField
            label={translate("generated.303e54ef24533234")}
            value={sshUsername}
            onChange={(event) => setSshUsername(event.target.value)}
          />
          <InputField
            label={translate("generated.02f98228994bcb2c")}
            type="password"
            value={sshPassword}
            onChange={(event) => setSshPassword(event.target.value)}
          />
        </div>
        <div className="mt-3">
          <Button
            disabled={
              busy || !sshUsername.trim() || sshPassword.trim().length < 4
            }
            onClick={() =>
              onSaveSshCredentials({
                sshUsername: sshUsername.trim(),
                sshPassword: sshPassword.trim(),
              })
            }
          >
            {translate("generated.c039ea9cff322096")}
          </Button>
        </div>
      </div>
      <div className="mt-4 border-t border-[#3b4067] pt-4">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          {translate("generated.a2c4b0522b462de6")}
        </h3>
        <p className="mt-1 text-[1.1rem] text-[#a8bed6]">
          {translate("generated.174e20f522dcf24d")}
        </p>
        <div className="mt-3 grid gap-3 md:grid-cols-2">
          <InputField
            label={translate("generated.07eb3cc614eb6f23")}
            value={twitchClientId}
            onChange={(event) => setTwitchClientId(event.target.value)}
            placeholder={translate("generated.46b176be62e06122")}
          />
          <InputField
            label={translate("generated.fe496d7aa0227730")}
            type="password"
            value={twitchClientSecret}
            onChange={(event) => setTwitchClientSecret(event.target.value)}
            placeholder={translate("generated.33d9856abb59955f")}
          />
        </div>
        <div className="mt-3 flex flex-wrap gap-3">
          <Button
            disabled={
              busy ||
              (twitchClientId.trim().length === 0) !==
                (twitchClientSecret.trim().length === 0)
            }
            onClick={() =>
              onSaveIgdbCredentials({
                twitchClientId: twitchClientId.trim(),
                twitchClientSecret: twitchClientSecret.trim(),
              })
            }
          >
            {translate("generated.d8eeb242249ba7ea")}
          </Button>
          <Button
            variant="ghost"
            disabled={busy}
            onClick={() =>
              onSaveIgdbCredentials({
                twitchClientId: "",
                twitchClientSecret: "",
              })
            }
          >
            {translate("generated.83b12c2216efb4fd")}
          </Button>
          <Button
            variant="secondary"
            disabled={busy}
            onClick={() => void openExternalUrl("https://dev.twitch.tv/console/apps")}
          >
            {translate("generated.b323fecaa6164d1f")}
          </Button>
        </div>
        <div className="mt-3 space-y-1 text-[1rem] text-[#8fb4d4]">
          <p>{translate("generated.35c5f3a7ba14f7f0")}</p>
          <p>{translate("generated.f4bd0220e25db120")}</p>
        </div>
      </div>

      <div className="mt-4 rounded-md border border-[#35506e] bg-[#0d1630]/80 p-4 text-[1.05rem] text-[#b4d7f4]">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          {translate("generated.a2e96e42782998b6")}
        </h3>
        <p className="mt-2 leading-snug">
          {translate("generated.79720ad3c8d7b854")}
        </p>
        <div className="mt-3 flex flex-wrap gap-3">
          <Button
            variant="secondary"
            disabled={busy}
            onClick={() => void openExternalUrl(VAST_LOGIN_URL)}
          >
            {translate("generated.a248fc9201615139")}
          </Button>
          <Button
            variant="ghost"
            disabled={busy}
            onClick={() => void openExternalUrl(VAST_BILLING_URL)}
          >
            {translate("generated.c90601c5e840eab8")}
          </Button>
          <Button
            variant="ghost"
            disabled={busy}
            onClick={() => void openExternalUrl(VAST_API_KEY_URL)}
          >
            {translate("generated.c533d3f8d6384008")}
          </Button>
        </div>
        <div className="mt-3 space-y-1 text-[1rem] text-[#8fb4d4]">
          <p>{translate("generated.6f996752a0b0926b")}</p>

        </div>
      </div>

      <div className="mt-4 grid gap-3">
        <InputField
          label={translate("generated.3923f29377d4ce31")}
          value={apiKey}
          type="password"
          onChange={(event) => setApiKey(event.target.value)}
        />
        <div className="flex flex-wrap gap-3">
          <Button
            disabled={busy || apiKey.trim().length < 16}
            onClick={() => onSaveApiKey(apiKey.trim())}
          >
            {translate("generated.89e24d7c78a1182a")}
          </Button>
          <Button
            variant="secondary"
            disabled={busy}
            onClick={() => void openExternalUrl(VAST_API_KEY_URL)}
          >
            {translate("generated.c533d3f8d6384008")}
          </Button>
        </div>
      </div>
    </Card>
  );

  const serverPanel = (
    <Card className="pixel-frame min-w-0 overflow-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        {translate("generated.65f170073c221c60")}
      </h2>
      <div className="mt-4 grid gap-3 md:grid-cols-3">
        <InputField
          label={translate("generated.7e04950e0f3e2234")}
          value={serverForm.minReliability}
          onChange={(event) =>
            setServerForm((prev) => ({
              ...prev,
              minReliability: event.target.value,
            }))
          }
        />
        <InputField
          label={translate("generated.a40284164df96090")}
          value={serverForm.storageGb}
          onChange={(event) =>
            setServerForm((prev) => ({
              ...prev,
              storageGb: event.target.value,
            }))
          }
        />
        <InputField
          label={translate("generated.ba60293671c08c9d")}
          value={serverForm.templateHash}
          onChange={(event) =>
            setServerForm((prev) => ({
              ...prev,
              templateHash: event.target.value,
            }))
          }
        />
      </div>
      <div className="mt-4">
        <Button
          disabled={busy || !serverForm.templateHash.trim()}
          onClick={() =>
            onSaveServerPreferences({
              minReliability: Math.max(
                0.8,
                toNumber(
                  serverForm.minReliability,
                  appState.serverPreferences.minReliability,
                ),
              ),
              storageGb: Math.max(
                30,
                Math.round(
                  toNumber(
                    serverForm.storageGb,
                    appState.serverPreferences.storageGb,
                  ),
                ),
              ),
              templateHash: serverForm.templateHash.trim(),
              maxHourlyPrice: appState.serverPreferences.maxHourlyPrice,
              minHourlyPrice: appState.serverPreferences.minHourlyPrice,
              requireVerified: appState.serverPreferences.requireVerified,
              requireDatacenter: appState.serverPreferences.requireDatacenter,
              includeOnDemand: appState.serverPreferences.includeOnDemand,
              includeInterruptible:
                appState.serverPreferences.includeInterruptible,
              includeReserved: appState.serverPreferences.includeReserved,
              requireStaticIp: false,
              requireAvx: appState.serverPreferences.requireAvx,
              minGpuCount: 1,
              minGpuRamGb: appState.serverPreferences.minGpuRamGb,
              minCpuCores: appState.serverPreferences.minCpuCores,
              minInetDownMbps: appState.serverPreferences.minInetDownMbps,
              minInetUpMbps: appState.serverPreferences.minInetUpMbps,
              geolocationCountryCode:
                appState.serverPreferences.geolocationCountryCode,
            })
          }
        >
          {translate("generated.2f84105cebcdfbc2")}
        </Button>
      </div>
    </Card>
  );

  const clientPanel = (
    <Card className="pixel-frame min-w-0 overflow-x-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        {translate("generated.37170ceaa4a77b3c")}
      </h2>
      <p className="mt-2 text-[1.1rem] text-[#a8bed6]">
        {translate("generated.7c3ce72f82bf0f7a")}
      </p>

      <SettingsSubsection
        title={translate("generated.9a12d65bf2f96b18")}
        description={translate("settings.display.source", { source: appState.sunshine.edidSourceLabel || translate("common.unknown") })}
      >
        <div className="grid gap-3 md:grid-cols-2">
          <SelectField
            label={translate("generated.4efbc56987dc1373")}
            value={edidMode}
            options={[
              { value: "auto_detect", label: "Auto detect (Scaling matched)" },
              { value: "mac_hardware", label: "Native Hardware (2560x1664 Panel)" },
              {
                value: "manual",
                label: "Manual (use Moonlight width and height)",
              },
            ]}
            onChange={(value) =>
              setEdidMode(value as "auto_detect" | "mac_hardware" | "manual")
            }
          />
          <div>
            <InputField
              label={translate("generated.2ef8a7821645caab")}
              value={edidRefreshRateHz}
              onChange={(event) => setEdidRefreshRateHz(event.target.value)}
            />
            <SettingHelp>
              {translate("generated.5eaebf46eb9441b5")}
            </SettingHelp>
          </div>
        </div>
        <div className="mt-3">
          <Button
            disabled={
              busy ||
              Math.round(
                toNumber(
                  edidRefreshRateHz,
                  appState.sunshine.edidRefreshRateHz,
                ),
              ) < 30 ||
              Math.round(
                toNumber(
                  edidRefreshRateHz,
                  appState.sunshine.edidRefreshRateHz,
                ),
              ) > 240
            }
            onClick={() =>
              onRegenerateEdid({
                mode: edidMode,
                refreshRateHz: Math.round(
                  toNumber(
                    edidRefreshRateHz,
                    appState.sunshine.edidRefreshRateHz,
                  ),
                ),
              })
            }
          >
            {translate("generated.d6df35e700b6035e")}
          </Button>
        </div>
      </SettingsSubsection>

      <div className="mt-4 space-y-4">
        <SettingsSubsection
          title={translate("generated.255da5c20861b5ef")}
          description={translate("generated.7289eed4407cdf13")}
        >
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            <div>
              <InputField
                label={translate("generated.b90eb0818758391f")}
                value={clientForm.bitrate}
                onChange={(event) =>
                  setClientForm((prev) => ({ ...prev, bitrate: event.target.value }))
                }
                placeholder="20000"
              />
              <SettingHelp>
                {translate("generated.2bfd5d867dafe410")}
              </SettingHelp>
            </div>
            <div>
              <InputField
                label={translate("generated.b22a096dcccd743c")}
                value={clientForm.fps}
                onChange={(event) =>
                  setClientForm((prev) => ({ ...prev, fps: event.target.value }))
                }
                placeholder="60"
              />
              <SettingHelp>
                {translate("generated.fa976965ffa51518")}
              </SettingHelp>
            </div>
            <SelectField
              label={translate("generated.21cf51fa69ed0901")}
              value={clientForm.refreshRateMode}
              options={[
                { value: "60", label: "60.00 Hz" },
                { value: "59.94", label: "59.94 Hz" },
              ]}
              onChange={(value) =>
                setClientForm((prev) => ({ ...prev, refreshRateMode: value }))
              }
            />
            <div>
              <InputField
                label={translate("generated.c0b66d7b2e5374e7")}
                value={clientForm.width}
                onChange={(event) =>
                  setClientForm((prev) => ({ ...prev, width: event.target.value }))
                }
                placeholder="1920"
              />
              <SettingHelp>
                {translate("generated.5408dc4ec4438c08")}
              </SettingHelp>
            </div>
            <div>
              <InputField
                label={translate("generated.7cc52d7673689833")}
                value={clientForm.height}
                onChange={(event) =>
                  setClientForm((prev) => ({ ...prev, height: event.target.value }))
                }
                placeholder="1080"
              />
              <SettingHelp>
                {translate("generated.c7fb95547160e90b")}
              </SettingHelp>
            </div>
            <SelectField
              label={translate("generated.556f85efa2beec52")}
              value={clientForm.aspectRatio}
              options={[
                { value: "", label: "Automatic (use width and height)" },
                { value: "16:9", label: "16:9" },
                { value: "16:10", label: "16:10" },
                { value: "21:9", label: "21:9" },
                { value: "4:3", label: "4:3" },
              ]}
              onChange={(value) =>
                setClientForm((prev) => ({ ...prev, aspectRatio: value }))
              }
            />
            <div>
              <InputField
                label={translate("generated.72b02f11664acc57")}
                value={clientForm.displayOutput}
                onChange={(event) =>
                  setClientForm((prev) => ({
                    ...prev,
                    displayOutput: event.target.value,
                  }))
                }
                placeholder={translate("generated.cdfa93bbd715a2e3")}
              />
              <SettingHelp>
                {translate("generated.bfe983dd7d4eefe9")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.4fd5c321a4cb1c71")}
                value={clientForm.videocfg}
                options={codecOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, videocfg: value }))
                }
              />
              <SettingHelp>
                {translate("generated.49fb42d1f367f2e2")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.27ba55a55b481cdc")}
                value={clientForm.videodec}
                options={decoderOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, videodec: value }))
                }
              />
              <SettingHelp>
                {translate("generated.759435568bdbd6b1")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.212f66f8e53f6911")}
                value={clientForm.hdr}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, hdr: value }))
                }
              />
              <SettingHelp>
                {translate("generated.5d28b3fdf85470fa")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.69726e40d09b2887")}
                value={clientForm.yuv444}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, yuv444: value }))
                }
              />
              <SettingHelp>
                {translate("generated.1ba9c5b8f4ae31ab")}
              </SettingHelp>
            </div>
          </div>
        </SettingsSubsection>

        <SettingsSubsection
          title={translate("generated.2e616a54da2f24b2")}
          description={translate("generated.f6c4a1e6457c2848")}
        >
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            <div>
              <SelectField
                label={translate("generated.83ee39129dfebf6a")}
                value={clientForm.hostaudio}
                options={hostAudioOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, hostaudio: value }))
                }
              />
              <SettingHelp>
                {translate("generated.b3115760b96af5f4")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.acfe057e2170a661")}
                value={clientForm.showperfoverlay}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, showperfoverlay: value }))
                }
              />
              <SettingHelp>
                {translate("generated.eb05426e0a537a9c")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.620826e20f92aa2a")}
                value={clientForm.showInputDebugHud}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, showInputDebugHud: value }))
                }
              />
              <SettingHelp>
                {translate("generated.a538cf0e6b35f405")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.1bc024181f58d2e3")}
                value={clientForm.keepawake}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, keepawake: value }))
                }
              />
              <SettingHelp>
                {translate("generated.071adfaf3fa01245")}
              </SettingHelp>
            </div>
          </div>
        </SettingsSubsection>

        <SettingsSubsection
          title={translate("generated.933883a62afc9e5e")}
          description={translate("generated.696d66b7576733db")}
        >
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            <div>
              <SelectField
                label={translate("generated.4da963d9d49530a3")}
                value={clientForm.framepacing}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, framepacing: value }))
                }
              />
              <SettingHelp>
                {translate("generated.554120fa3bafd2f6")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.052810dae19607f5")}
                value={clientForm.vsync}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, vsync: value }))
                }
              />
              <SettingHelp>
                {translate("generated.c9f5a66692d7c752")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.6c8f53fd15dc7140")}
                value={clientForm.gameopts}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, gameopts: value }))
                }
              />
              <SettingHelp>
                {translate("generated.ddc6fb0c53ddd6d3")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.193276f2323cb566")}
                value={clientForm.gamepadmouse}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, gamepadmouse: value }))
                }
              />
              <SettingHelp>
                {translate("generated.50c3b4093d619442")}
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label={translate("generated.edaaebf7b6d865cf")}
                value={clientForm.detectnetblocking}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, detectnetblocking: value }))
                }
              />
              <SettingHelp>
                {translate("generated.68f8901e5d60611b")}
              </SettingHelp>
            </div>
          </div>
        </SettingsSubsection>
      </div>

      <div className="mt-4">
        <Button
          disabled={busy}
          onClick={() =>
            onSaveMoonlightPreferences({
              bitrate: Math.max(
                10000,
                Math.round(
                  toNumber(
                    clientForm.bitrate,
                    appState.moonlightPreferences.bitrate,
                  ),
                ),
              ),
              fps: Math.max(
                30,
                Math.round(
                  toNumber(clientForm.fps, appState.moonlightPreferences.fps),
                ),
              ),
              refreshRateMode:
                clientForm.refreshRateMode === "59.94" ? "59.94" : "60",
              width: Math.max(
                1280,
                Math.round(
                  toNumber(
                    clientForm.width,
                    appState.moonlightPreferences.width,
                  ),
                ),
              ),
              height: Math.max(
                720,
                Math.round(
                  toNumber(
                    clientForm.height,
                    appState.moonlightPreferences.height,
                  ),
                ),
              ),
              displayOutput: clientForm.displayOutput.trim()
                ? clientForm.displayOutput.trim()
                : null,
              aspectRatio: clientForm.aspectRatio.trim()
                ? clientForm.aspectRatio.trim()
                : null,
              hostaudio: Math.round(
                toNumber(
                  clientForm.hostaudio,
                  appState.moonlightPreferences.hostaudio,
                ),
              ),
              showperfoverlay: Math.round(
                toNumber(
                  clientForm.showperfoverlay,
                  appState.moonlightPreferences.showperfoverlay,
                ),
              ),
              keepawake: Math.round(
                toNumber(
                  clientForm.keepawake,
                  appState.moonlightPreferences.keepawake,
                ),
              ),
              framepacing: Math.round(
                toNumber(
                  clientForm.framepacing,
                  appState.moonlightPreferences.framepacing,
                ),
              ),
              vsync: Math.round(
                toNumber(clientForm.vsync, appState.moonlightPreferences.vsync),
              ),
              hdr: Math.round(
                toNumber(clientForm.hdr, appState.moonlightPreferences.hdr),
              ),
              videocfg: Math.round(
                toNumber(
                  clientForm.videocfg,
                  appState.moonlightPreferences.videocfg,
                ),
              ),
              videodec: Math.round(
                toNumber(
                  clientForm.videodec,
                  appState.moonlightPreferences.videodec,
                ),
              ),
              yuv444: Math.round(
                toNumber(
                  clientForm.yuv444,
                  appState.moonlightPreferences.yuv444,
                ),
              ),
              gameopts: Math.round(
                toNumber(
                  clientForm.gameopts,
                  appState.moonlightPreferences.gameopts,
                ),
              ),
              gamepadmouse: Math.round(
                toNumber(
                  clientForm.gamepadmouse,
                  appState.moonlightPreferences.gamepadmouse,
                ),
              ),
              detectnetblocking: Math.round(
                toNumber(
                  clientForm.detectnetblocking,
                  appState.moonlightPreferences.detectnetblocking,
                ),
              ),
              showInputDebugHud: Math.round(
                toNumber(
                  clientForm.showInputDebugHud,
                  appState.moonlightPreferences.showInputDebugHud,
                ),
              ),
            })
          }
        >
          {translate("generated.17f8dec4dbda2261")}
        </Button>
      </div>
    </Card>
  );

  const storagePanel = (
    <Card className="pixel-frame min-w-0 overflow-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        {translate("generated.370fbbd74fb331da")}
      </h2>
      <div className="mt-4">
        <SharedStorageSettingsV2
          busy={busy}
          providers={storageProviders}
          profiles={sharedStorageProfiles}
          testResult={sharedStorageTestResult}
          oauthSessionId={oauthSessionId}
          onConnectProvider={onConnectStorageProvider}
          onTestConnection={onTestStorageConnection}
          onSetActiveProfile={onSetActiveStorageProfile}
          onDisconnect={onDisconnectStorageProfile}
          onLoadProviders={onLoadStorageProviders}
          onLoadProfiles={onLoadSharedStorageProfiles}
          onBeginOauthFlow={onBeginOauthFlow}
          onCompleteOauthFlow={onCompleteOauthFlow}
          onCancelOauthFlow={onCancelOauthFlow}
        />
      </div>
      <div className="mt-6">
        <AutoShutdownSettings
          state={appState.autoShutdown}
          busy={busy}
          hasActiveStorageProfile={sharedStorageProfiles.some(
            (profile) => profile.active,
          )}
          hasVastApiKey={appState.credentials.vastApiKey.trim().length > 0}
          hasProvisionedServer={appState.provisionedServers.length > 0}
          instanceId={appState.provisionedServers[0]?.instanceId ?? null}
          onSave={onSaveAutoShutdownSettings}
        />
      </div>
    </Card>
  );

  const connectionPanel = (
    <Card className="pixel-frame min-w-0 overflow-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        {translate("generated.5b4122b3c427915c")}
      </h2>
      <p className="mt-2 text-[1.1rem] text-[#a8bed6]">
        {translate("generated.4dc31a0d7900db71")}
      </p>

      <div className="mt-4 rounded-md border border-[#3b4067] bg-[#10152f] p-4">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          {translate("generated.5915dd34a63038c4")}
        </h3>
        <p className="mt-2 text-[1.15rem] text-white">
          {translate("generated.9030dbbe6db96245")}
        </p>
        <p className="mt-2 text-[1.05rem] leading-snug text-[#a8bed6]">
          {translate("generated.61392d867c0e5960")}
        </p>
      </div>

      <div className="mt-4 rounded-md border border-[#3b4067] bg-[#10152f] p-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
              {translate("generated.6557737f10551aa5")}
            </h3>
            <p className="mt-2 max-w-3xl text-[1.05rem] leading-snug text-[#a8bed6]">
              {translate("generated.bee2301df48007b8")}
            </p>
          </div>
          <span className="rounded border border-[#48527a] px-2 py-1 font-display text-[9px] uppercase tracking-[0.12em] text-[#b7d7f2]">
            {cloudflareTurnSettings?.status ?? "loading"}
          </span>
        </div>

        <label className="mt-4 flex items-center gap-3 text-[1.05rem] text-white">
          <input
            type="checkbox"
            checked={turnEnabled}
            onChange={(event) => setTurnEnabled(event.target.checked)}
          />
          {translate("generated.04c9f19b783de730")}
        </label>

        {cloudflareTurnSettings?.tokenSet ? (
          <p className="mt-3 text-[1rem] text-[#8fb4d4]">
            {translate("generated.9cb5175c6c6c397f")} {cloudflareTurnSettings.keyIdHint ?? "configured"}{translate("generated.689736c74f9ae689")}
          </p>
        ) : null}

        <div className="mt-4 grid gap-3 md:grid-cols-2">
          <InputField
            label={translate("generated.479f3da799e56d1a")}
            value={turnKeyId}
            onChange={(event) => setTurnKeyId(event.target.value)}
            placeholder={translate("generated.9fec9ced2af2afcf")}
          />
          <InputField
            label={translate("generated.3a6eafb10eeb3368")}
            type="password"
            value={turnApiToken}
            onChange={(event) => setTurnApiToken(event.target.value)}
            placeholder={translate("generated.37905870b7054d0b")}
          />
        </div>

        <div className="mt-4 flex flex-wrap gap-3">
          <Button
            variant="secondary"
            disabled={busy || !turnKeyId.trim() || !turnApiToken.trim()}
            onClick={() =>
              void onTestCloudflareTurnSettings({
                enabled: turnEnabled,
                keyId: turnKeyId.trim(),
                apiToken: turnApiToken.trim(),
              })
            }
          >
            {translate("generated.2643f644855bb881")}
          </Button>
          <Button
            disabled={busy || !turnKeyId.trim() || !turnApiToken.trim()}
            onClick={() =>
              void onSaveCloudflareTurnSettings({
                enabled: turnEnabled,
                keyId: turnKeyId.trim(),
                apiToken: turnApiToken.trim(),
              })
            }
          >
            {translate("generated.3772aaca2ec5db8e")}
          </Button>
          <Button
            variant="ghost"
            disabled={busy || !cloudflareTurnSettings?.tokenSet}
            onClick={() => void onClearCloudflareTurnSettings()}
          >
            {translate("generated.385a5247e7736bc0")}
          </Button>
        </div>

        {cloudflareTurnTestResult?.valid ? (
          <p className="mt-3 text-[1rem] text-neon-lime">
            {translate("generated.fc90b0c389b8b0dd")} {cloudflareTurnTestResult.udpUrls.length} {translate("generated.990dd54f914614f2")}{cloudflareTurnTestResult.udpUrls.length === 1 ? "" : translate("generated.043a718774c572bd")}.
          </p>
        ) : null}
      </div>

      <div className="mt-4 rounded-md border border-[#3b4067] bg-[#10152f] p-4">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          {translate("generated.457aaf87691571fa")}
        </h3>
        <p className="mt-2 text-[1.05rem] leading-snug text-[#a8bed6]">
          {translate("generated.ee3d1dff1b2b6f7c")}
        </p>
        <div className="mt-4 grid gap-3">
          {appState.provisionedServers.length === 0 ? (
            <p className="text-[1rem] text-[#8fb4d4]">{translate("generated.317c3454ab9a1050")}</p>
          ) : (
            appState.provisionedServers.map((server) => {
              const status = connectionStatuses[server.instanceId];
              const network = status?.network ?? server.network;
              return (
                <div
                  key={server.instanceId}
                  className="flex flex-wrap items-center justify-between gap-3 rounded border border-[#343b61] bg-[#0b1027] p-3"
                >
                  <div>
                    <p className="font-display text-[9px] uppercase tracking-[0.12em] text-white">
                      {translate("generated.425f233626b3cd02")} {server.instanceId}
                    </p>
                    <p className="mt-1 text-[1rem] text-[#8fb4d4]">
                      {translate("generated.65d7deb02f4bc24b")} {network.preference} {translate("generated.fd53dfb399f155ec")} {network.activeTransport ?? "not validated"}
                      {network.lastEvaluation
                        ? ` · ${network.lastEvaluation.reason}`
                        : ""}
                    </p>
                    <p className="mt-1 text-[0.95rem] text-[#789aba]">
                      {network.connectionProfile
                        ? `Verified profile r${network.connectionProfile.profileRevision} · MTU ${network.connectionProfile.innerMtu} · ${network.connectionProfile.packetLimits.measurementMethod}`
                        : translate("generated.b1feaf1905d89b26")}
                      {network.lastTransition
                        ? ` · Last transition: ${network.lastTransition.phase}`
                        : ""}
                    </p>
                  </div>
                  <div className="flex flex-wrap items-center gap-2">
                    <select
                      className="min-w-52 rounded border border-[#48527a] bg-[#111936] px-3 py-2 text-[1rem] text-white"
                      value={network.preference}
                      disabled={busy || switchingInstanceId === server.instanceId || !status}
                      onChange={(event) =>
                        void changeConnectionPreference(
                          server.instanceId,
                          event.target.value as ConnectionPreference,
                        )
                      }
                    >
                      {status?.automaticSelectionEnabled ? (
                        <option value="auto">{translate("generated.8e302a5e0a46fca1")}</option>
                      ) : network.preference === "auto" ? (
                        <option value="auto" disabled>{translate("generated.99ece9a6d062c12b")}</option>
                      ) : null}
                      <option value="direct">{translate("generated.941a194944d452cc")}</option>
                      {status?.manualTurnSwitchingEnabled &&
                      network.cloudflareTurn.enabled ? (
                        <option value="cloudflare_turn">{translate("generated.8b51f06b7f19cf3f")}</option>
                      ) : null}
                    </select>
                    <Button
                      variant="ghost"
                      disabled={busy || switchingInstanceId === server.instanceId || !status}
                      onClick={() => void repairConnection(server.instanceId)}
                    >
                      {switchingInstanceId === server.instanceId
                        ? translate("generated.fb4c616eb4907704")
                        : translate("generated.483aefecfb711622")}
                    </Button>
                  </div>
                </div>
              );
            })
          )}
        </div>
        {connectionStatusError ? (
          <p className="mt-3 text-[1rem] text-[#ff9aae]">{connectionStatusError}</p>
        ) : null}
        {appState.provisionedServers.some(
          (server) => !connectionStatuses[server.instanceId]?.manualTurnSwitchingEnabled,
        ) ? (
          <p className="mt-3 text-[0.95rem] text-[#8fb4d4]">
            {translate("generated.3d768a955278c50b")}
          </p>
        ) : null}
      </div>

    </Card>
  );

  const notificationsPanel = <NotificationSettings />;
  const languagePanel = (
    <Card className="pixel-frame">
      <SettingsSubsection
        title={t("settings.language")}
        description={t("settings.language.description")}
      >
        <SelectField
          label={t("settings.language")}
          value={preference}
          options={LOCALE_OPTIONS.map((option) => ({
            value: option.value,
            label: option.value === "system" ? t("settings.language.system") : option.label,
          }))}
          onChange={(value) => setLocale(value as LocalePreference)}
        />
      </SettingsSubsection>
    </Card>
  );

  const panel =
    section === "profile"
      ? profilePanel
      : section === "server"
        ? serverPanel
        : section === "storage"
          ? storagePanel
          : section === "connection"
            ? connectionPanel
            : section === "notifications"
              ? notificationsPanel
              : section === "language"
                ? languagePanel
                : clientPanel;

  return (
    <main className="crt-surface min-h-dvh bg-hero-glow px-4 pb-6 pt-6 md:px-8">
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-4">
        <div className="flex shrink-0 items-center justify-between gap-4">
          <div className="flex items-center gap-3">
            <div>
              <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">
                {t("app.settings")}
              </p>
              <h1
                className="pixel-heading glitch-title font-display text-lg text-white md:text-xl"
                data-text={t("app.preferences")}
              >
                {t("app.preferences")}
              </h1>
            </div>
            <AIPromptHelper
              topic={translate("generated.329d5f59f3636df1")}
              promptText={APP_PROMPTS.settingsPage}
              variant="both"
            />
          </div>

          <div className="flex items-center gap-2">
            <ArcadeSoundToggle />
            <Link to="/">
              <Button variant="ghost">{t("settings.back")}</Button>
            </Link>
          </div>
        </div>

        <section className="grid items-start gap-4 md:grid-cols-[240px_minmax(0,1fr)]">
          <Card className="pixel-frame self-start overflow-hidden md:sticky md:top-6">
            <div className="grid gap-2">
              <Button
                variant={section === "profile" ? "secondary" : "ghost"}
                onClick={() => setSection("profile")}
              >
                {t("settings.profile")}
              </Button>
              <Button
                variant={section === "server" ? "secondary" : "ghost"}
                onClick={() => setSection("server")}
              >
                {t("settings.server")}
              </Button>
              <Button
                variant={section === "client" ? "secondary" : "ghost"}
                onClick={() => setSection("client")}
              >
                {t("settings.client")}
              </Button>
              <Button
                variant={section === "storage" ? "secondary" : "ghost"}
                onClick={() => setSection("storage")}
              >
                {t("settings.storage")}
              </Button>
              <Button
                variant={section === "connection" ? "secondary" : "ghost"}
                onClick={() => setSection("connection")}
              >
                {t("settings.connection")}
              </Button>
              <Button
                variant={section === "notifications" ? "secondary" : "ghost"}
                onClick={() => setSection("notifications")}
              >
                {t("settings.notifications")}
              </Button>
              <Button
                variant={section === "language" ? "secondary" : "ghost"}
                onClick={() => setSection("language")}
              >
                {t("settings.language")}
              </Button>
            </div>
          </Card>

          <div className="min-w-0 pr-1">
            {panel}
          </div>
        </section>
      </div>
    </main>
  );
}
