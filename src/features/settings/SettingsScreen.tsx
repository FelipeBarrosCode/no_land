import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { APP_PROMPTS } from "../../prompts/appPrompts";
import { ArcadeSoundToggle } from "../../components/ui/ArcadeSoundToggle";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { SharedStorageSettingsV2 } from "../shared-storage/SharedStorageSettingsV2";
import type {
  MoonlightPreferences,
  PlatformCredentialsUpdate,
  PersistedAppState,
  ProfileReference,
  ProviderDefinition,
  ServerPreferencesUpdate,
  SharedStorageTestResult,
  SshCredentialsUpdate,
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
  | "connection";
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
  onSaveApiKey: (apiKey: string) => Promise<void>;
  onSavePlatformCredentials: (
    payload: PlatformCredentialsUpdate,
  ) => Promise<void>;
  onSaveServerPreferences: (
    payload: Partial<ServerPreferencesUpdate>,
  ) => Promise<void>;
  onSaveMoonlightPreferences: (payload: MoonlightPreferences) => Promise<void>;
  onSaveSshCredentials: (payload: SshCredentialsUpdate) => Promise<void>;
  onRegenerateEdid: (payload: {
    mode: "auto_detect" | "manual";
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
            {option.label}
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
  onSaveApiKey,
  onSavePlatformCredentials,
  onSaveServerPreferences,
  onSaveMoonlightPreferences,
  onSaveSshCredentials,
  onRegenerateEdid,
}: Props) {
  const [section, setSection] = useState<SettingsSection>("profile");
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
  const [sshPassword, setSshPassword] = useState(
    appState.ssh.sshPassword || appState.credentials.appPassword,
  );
  const [edidMode, setEdidMode] = useState<"auto_detect" | "mac_hardware" | "manual">(
    appState.sunshine.edidMode,
  );
  const [edidRefreshRateHz, setEdidRefreshRateHz] = useState(
    appState.sunshine.edidRefreshRateHz.toString(),
  );

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
        Profile
      </h2>
      <div className="mt-4 grid gap-3 md:grid-cols-2">
        <InputField
          label="Platform Username"
          value={platformUsername}
          onChange={(event) => setPlatformUsername(event.target.value)}
        />
        <InputField
          label="Platform Password"
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
          Save Platform Credentials
        </Button>
      </div>
      <div className="mt-4 border-t border-[#3b4067] pt-4">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          SSH Login Credentials
        </h3>
        <p className="mt-1 text-[1.1rem] text-[#a8bed6]">
          Used after key-based connection when the VM asks for
          username/password.
        </p>
        <div className="mt-3 grid gap-3 md:grid-cols-2">
          <InputField
            label="SSH Username"
            value={sshUsername}
            onChange={(event) => setSshUsername(event.target.value)}
          />
          <InputField
            label="SSH Password"
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
            Save SSH Credentials
          </Button>
        </div>
      </div>
      <div className="mt-4 rounded-md border border-[#35506e] bg-[#0d1630]/80 p-4 text-[1.05rem] text-[#b4d7f4]">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          Vast.ai Links
        </h3>
        <p className="mt-2 leading-snug">
          Use your normal browser for Vast.ai account access. Log in there, manage billing, create an API key, and then paste that API key into Noland.
        </p>
        <div className="mt-3 flex flex-wrap gap-3">
          <Button
            variant="secondary"
            disabled={busy}
            onClick={() => void openExternalUrl(VAST_LOGIN_URL)}
          >
            Open Vast.ai Login
          </Button>
          <Button
            variant="ghost"
            disabled={busy}
            onClick={() => void openExternalUrl(VAST_BILLING_URL)}
          >
            Open Vast.ai Billing
          </Button>
          <Button
            variant="ghost"
            disabled={busy}
            onClick={() => void openExternalUrl(VAST_API_KEY_URL)}
          >
            Open API Key Page
          </Button>
        </div>
        <div className="mt-3 space-y-1 text-[1rem] text-[#8fb4d4]">
          <p>Use the normal browser pages above, then save the API key here.</p>

        </div>
      </div>

      <div className="mt-4 grid gap-3">
        <InputField
          label="Vast API Key"
          value={apiKey}
          type="password"
          onChange={(event) => setApiKey(event.target.value)}
        />
        <div className="flex flex-wrap gap-3">
          <Button
            disabled={busy || apiKey.trim().length < 16}
            onClick={() => onSaveApiKey(apiKey.trim())}
          >
            Save API Key
          </Button>
          <Button
            variant="secondary"
            disabled={busy}
            onClick={() => void openExternalUrl(VAST_API_KEY_URL)}
          >
            Open API Key Page
          </Button>
        </div>
      </div>
    </Card>
  );

  const serverPanel = (
    <Card className="pixel-frame min-w-0 overflow-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        Server Configuration
      </h2>
      <div className="mt-4 grid gap-3 md:grid-cols-3">
        <InputField
          label="Min Reliability (0.8-1)"
          value={serverForm.minReliability}
          onChange={(event) =>
            setServerForm((prev) => ({
              ...prev,
              minReliability: event.target.value,
            }))
          }
        />
        <InputField
          label="Storage (GB)"
          value={serverForm.storageGb}
          onChange={(event) =>
            setServerForm((prev) => ({
              ...prev,
              storageGb: event.target.value,
            }))
          }
        />
        <InputField
          label="Template Hash"
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
          Save Server Config
        </Button>
      </div>
    </Card>
  );

  const clientPanel = (
    <Card className="pixel-frame min-w-0 overflow-x-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        Client (Moonlight)
      </h2>
      <p className="mt-2 text-[1.1rem] text-[#a8bed6]">
        Tune how Moonlight streams video, audio, and input to this device. Use
        the text boxes for custom performance targets like bitrate, frame rate,
        and resolution. Use the dropdowns for fixed on/off and codec choices.
      </p>

      <SettingsSubsection
        title="Headless EDID"
        description={`Display source: ${appState.sunshine.edidSourceLabel || "Unknown"}. The app refreshes the native profile at startup; apply it to a running VM from its Display action.`}
      >
        <div className="grid gap-3 md:grid-cols-2">
          <SelectField
            label="EDID Mode"
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
              label="EDID Refresh Rate (30–240 Hz)"
              value={edidRefreshRateHz}
              onChange={(event) => setEdidRefreshRateHz(event.target.value)}
            />
            <SettingHelp>
              Set this to match your local display refresh rate. If that timing
              cannot fit the current EDID format, the app uses a safe 60 Hz
              native-resolution profile.
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
            Refresh EDID Profile
          </Button>
        </div>
      </SettingsSubsection>

      <div className="mt-4 space-y-4">
        <SettingsSubsection
          title="Video Quality"
          description="Choose your target bitrate, frame rate, resolution, and codec preferences."
        >
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            <div>
              <InputField
                label="Bitrate (Kbps)"
                value={clientForm.bitrate}
                onChange={(event) =>
                  setClientForm((prev) => ({ ...prev, bitrate: event.target.value }))
                }
                placeholder="20000"
              />
              <SettingHelp>
                Higher values improve image quality but require more bandwidth.
              </SettingHelp>
            </div>
            <div>
              <InputField
                label="Target FPS"
                value={clientForm.fps}
                onChange={(event) =>
                  setClientForm((prev) => ({ ...prev, fps: event.target.value }))
                }
                placeholder="60"
              />
              <SettingHelp>
                Common values are 30, 60, and 120. Do not exceed your display
                refresh rate.
              </SettingHelp>
            </div>
            <SelectField
              label="Refresh Timing"
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
                label="Resolution Width"
                value={clientForm.width}
                onChange={(event) =>
                  setClientForm((prev) => ({ ...prev, width: event.target.value }))
                }
                placeholder="1920"
              />
              <SettingHelp>
                Horizontal resolution to stream, such as 1920 for 1080p.
              </SettingHelp>
            </div>
            <div>
              <InputField
                label="Resolution Height"
                value={clientForm.height}
                onChange={(event) =>
                  setClientForm((prev) => ({ ...prev, height: event.target.value }))
                }
                placeholder="1080"
              />
              <SettingHelp>
                Vertical resolution to stream, such as 1080 for 1080p.
              </SettingHelp>
            </div>
            <SelectField
              label="Aspect Ratio"
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
                label="Display Output"
                value={clientForm.displayOutput}
                onChange={(event) =>
                  setClientForm((prev) => ({
                    ...prev,
                    displayOutput: event.target.value,
                  }))
                }
                placeholder="Leave blank for default"
              />
              <SettingHelp>
                Optional monitor/output identifier on the cloud machine. Leave
                blank unless you know the exact output to target.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="Preferred Codec"
                value={clientForm.videocfg}
                options={codecOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, videocfg: value }))
                }
              />
              <SettingHelp>
                Automatic is safest. Force a codec only if you are chasing
                compatibility or quality issues.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="Video Decoder"
                value={clientForm.videodec}
                options={decoderOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, videodec: value }))
                }
              />
              <SettingHelp>
                Hardware decode is usually fastest. Software decode can help on
                unsupported systems.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="HDR Streaming"
                value={clientForm.hdr}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, hdr: value }))
                }
              />
              <SettingHelp>
                Enable only when both the cloud machine and your local display
                support HDR.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="YUV444 Colour"
                value={clientForm.yuv444}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, yuv444: value }))
                }
              />
              <SettingHelp>
                Improves text and colour accuracy, but uses more bandwidth.
              </SettingHelp>
            </div>
          </div>
        </SettingsSubsection>

        <SettingsSubsection
          title="Audio and Session"
          description="Control where audio plays and how the client behaves during long sessions."
        >
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            <div>
              <SelectField
                label="Host Audio"
                value={clientForm.hostaudio}
                options={hostAudioOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, hostaudio: value }))
                }
              />
              <SettingHelp>
                Usually you want audio to play locally, not on the remote host.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="Performance Overlay"
                value={clientForm.showperfoverlay}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, showperfoverlay: value }))
                }
              />
              <SettingHelp>
                Shows a live HUD with stream stats like FPS, latency, and
                bitrate.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="Input Debug HUD"
                value={clientForm.showInputDebugHud}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, showInputDebugHud: value }))
                }
              />
              <SettingHelp>
                Shows the yellow native macOS input debug box. Keep this disabled
                unless you are debugging mouse or keyboard capture.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="Keep Device Awake"
                value={clientForm.keepawake}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, keepawake: value }))
                }
              />
              <SettingHelp>
                Prevents your local machine from sleeping during long sessions.
              </SettingHelp>
            </div>
          </div>
        </SettingsSubsection>

        <SettingsSubsection
          title="Smoothness and Compatibility"
          description="Adjust stream behavior for lower latency, smoother motion, and input compatibility."
        >
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            <div>
              <SelectField
                label="Frame Pacing"
                value={clientForm.framepacing}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, framepacing: value }))
                }
              />
              <SettingHelp>
                Helps smooth out motion. Recommended enabled for most users.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="VSync"
                value={clientForm.vsync}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, vsync: value }))
                }
              />
              <SettingHelp>
                Reduces tearing, but may add a little input latency.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="Game Optimizations"
                value={clientForm.gameopts}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, gameopts: value }))
                }
              />
              <SettingHelp>
                Keeps Moonlight tuned for game streaming. Usually best left
                enabled.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="Gamepad Mouse"
                value={clientForm.gamepadmouse}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, gamepadmouse: value }))
                }
              />
              <SettingHelp>
                Lets a connected controller also move the remote mouse cursor.
              </SettingHelp>
            </div>
            <div>
              <SelectField
                label="Detect Network Blocking"
                value={clientForm.detectnetblocking}
                options={binaryOptions}
                onChange={(value) =>
                  setClientForm((prev) => ({ ...prev, detectnetblocking: value }))
                }
              />
              <SettingHelp>
                Helps the app detect network interruptions and blocked stream
                traffic.
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
          Save Client Config
        </Button>
      </div>
    </Card>
  );

  const storagePanel = (
    <Card className="pixel-frame min-w-0 overflow-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        Shared Storage (BETA)
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
        />
      </div>
    </Card>
  );

  const connectionPanel = (
    <Card className="pixel-frame min-w-0 overflow-hidden">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        Connection Provider
      </h2>
      <p className="mt-2 text-[1.1rem] text-[#a8bed6]">
        Noland now uses a managed secure tunnel for the desktop connection flow. The app brings the connection up locally and verifies it before continuing to streaming setup.
      </p>

      <div className="mt-4 rounded-md border border-[#3b4067] bg-[#10152f] p-4">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          Active Desktop Tunnel Mode
        </h3>
        <p className="mt-2 text-[1.15rem] text-white">
          Managed secure tunnel
        </p>
        <p className="mt-2 text-[1.05rem] leading-snug text-[#a8bed6]">
          Keep this set to the managed tunnel option so Noland can configure the local desktop connection automatically.
        </p>
      </div>

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
            : clientPanel;

  return (
    <main className="crt-surface min-h-dvh bg-hero-glow px-4 pb-6 pt-6 md:px-8">
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-4">
        <div className="flex shrink-0 items-center justify-between gap-4">
          <div className="flex items-center gap-3">
            <div>
              <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">
                Settings
              </p>
              <h1
                className="pixel-heading glitch-title font-display text-lg text-white md:text-xl"
                data-text="Preferences"
              >
                Preferences
              </h1>
            </div>
            <AIPromptHelper
              topic="App Configuration & Settings"
              promptText={APP_PROMPTS.settingsPage}
              variant="both"
            />
          </div>

          <div className="flex items-center gap-2">
            <ArcadeSoundToggle />
            <Link to="/">
              <Button variant="ghost">Back</Button>
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
                Profile
              </Button>
              <Button
                variant={section === "server" ? "secondary" : "ghost"}
                onClick={() => setSection("server")}
              >
                Server Configuration
              </Button>
              <Button
                variant={section === "client" ? "secondary" : "ghost"}
                onClick={() => setSection("client")}
              >
                Client
              </Button>
              <Button
                variant={section === "storage" ? "secondary" : "ghost"}
                onClick={() => setSection("storage")}
              >
                Shared Storage (BETA)
              </Button>
              <Button
                variant={section === "connection" ? "secondary" : "ghost"}
                onClick={() => setSection("connection")}
              >
                Connection
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
