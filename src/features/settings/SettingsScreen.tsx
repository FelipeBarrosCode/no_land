import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { APP_PROMPTS } from "../../prompts/appPrompts";
import { ArcadeSoundToggle } from "../../components/ui/ArcadeSoundToggle";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { SharedStorageSettings } from "../shared-storage-manager/SharedStorageSettings";
import type {
  MoonlightPreferences,
  PlatformCredentialsUpdate,
  PersistedAppState,
  ServerPreferencesUpdate,
  SharedStorageSettingsResponse,
  SharedStorageSettingsUpdate,
  SshCredentialsUpdate,
} from "../../lib/types";

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
};

interface Props {
  appState: PersistedAppState;
  busy: boolean;
  sharedStorageSettings: SharedStorageSettingsResponse | null;
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
  onSaveSharedStorageSettings: (
    payload: SharedStorageSettingsUpdate,
  ) => Promise<void>;
  onTestSharedStorageConfig: () => Promise<string | null>;
  onLoadSharedStorageSettings: () => Promise<void>;
  onSaveConnectionProvider: (payload: {
    connectionProvider: "wireguard" | "tailscale";
  }) => Promise<void>;
  onSaveTailscaleApiKey: (apiKey: string) => Promise<void>;
}

function toNumber(value: string, fallback: number): number {
  const parsed = Number.parseFloat(value);
  if (Number.isNaN(parsed)) {
    return fallback;
  }

  return parsed;
}

const clientNumericFields: Array<
  keyof Omit<ClientForm, "refreshRateMode" | "displayOutput" | "aspectRatio">
> = [
  "bitrate",
  "fps",
  "width",
  "height",
  "hostaudio",
  "showperfoverlay",
  "keepawake",
  "framepacing",
  "vsync",
  "hdr",
  "videocfg",
  "videodec",
  "yuv444",
  "gameopts",
  "gamepadmouse",
  "detectnetblocking",
];

export function SettingsScreen({
  appState,
  busy,
  sharedStorageSettings,
  onSaveApiKey,
  onSavePlatformCredentials,
  onSaveServerPreferences,
  onSaveMoonlightPreferences,
  onSaveSshCredentials,
  onRegenerateEdid,
  onSaveSharedStorageSettings,
  onTestSharedStorageConfig,
  onLoadSharedStorageSettings,
  onSaveConnectionProvider,
  onSaveTailscaleApiKey,
}: Props) {
  const [section, setSection] = useState<SettingsSection>("profile");
  const [apiKey, setApiKey] = useState(appState.credentials.vastApiKey);
  const [tailscaleApiKey, setTailscaleApiKey] = useState(
    appState.credentials.tailscaleApiKey,
  );
  const [connectionProvider, setConnectionProvider] = useState<
    "wireguard" | "tailscale"
  >(appState.connectionProvider || "wireguard");
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
  const [edidMode, setEdidMode] = useState<"auto_detect" | "manual">(
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
    });
  }, [appState]);

  useEffect(() => {
    void onLoadSharedStorageSettings();
  }, []);

  const profilePanel = (
    <Card className="pixel-frame">
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
      <div className="mt-4 grid gap-3">
        <InputField
          label="Vast API Key"
          value={apiKey}
          type="password"
          onChange={(event) => setApiKey(event.target.value)}
        />
        <div>
          <Button
            disabled={busy || apiKey.trim().length < 16}
            onClick={() => onSaveApiKey(apiKey.trim())}
          >
            Save API Key
          </Button>
        </div>
      </div>
    </Card>
  );

  const serverPanel = (
    <Card className="pixel-frame">
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
    <Card className="pixel-frame">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        Client (Moonlight)
      </h2>
      <div className="mt-4 border border-[#3b4067] bg-[#10152f] p-4">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          Headless EDID
        </h3>
        <p className="mt-1 text-[1.1rem] text-[#a8bed6]">
          Display source: {appState.sunshine.edidSourceLabel || "Unknown"}
        </p>
        <div className="mt-3 grid gap-3 md:grid-cols-2">
          <div className="space-y-2">
            <label className="font-display text-[10px] uppercase tracking-[0.18em] text-neon-lime">
              EDID Mode
            </label>
            <select
              value={edidMode}
              onChange={(event) =>
                setEdidMode(event.target.value as "auto_detect" | "manual")
              }
              className="w-full rounded-md border border-neon-cyan/40 bg-black/60 px-3 py-2 text-sm text-neon-cyan outline-none transition focus:border-neon-lime"
            >
              <option value="auto_detect">Auto Detect</option>
              <option value="manual">
                Manual (use Moonlight width/height)
              </option>
            </select>
          </div>
          <InputField
            label="EDID Refresh Rate (30-240 Hz)"
            value={edidRefreshRateHz}
            onChange={(event) => setEdidRefreshRateHz(event.target.value)}
          />
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
            Regenerate EDID
          </Button>
        </div>
      </div>
      <div className="mt-4 grid gap-3 md:grid-cols-4">
        {clientNumericFields.map((key) => (
          <InputField
            key={key}
            label={key}
            value={clientForm[key]}
            onChange={(event) =>
              setClientForm((prev) => ({ ...prev, [key]: event.target.value }))
            }
          />
        ))}
        <div className="space-y-2">
          <label className="font-display text-[10px] uppercase tracking-[0.18em] text-neon-lime">
            Refresh Timing
          </label>
          <select
            value={clientForm.refreshRateMode}
            onChange={(event) =>
              setClientForm((prev) => ({
                ...prev,
                refreshRateMode: event.target.value,
              }))
            }
            className="w-full rounded-md border border-neon-cyan/40 bg-black/60 px-3 py-2 text-sm text-neon-cyan outline-none transition focus:border-neon-lime"
          >
            <option value="60">60.00 Hz</option>
            <option value="59.94">59.94 Hz</option>
          </select>
        </div>
        <InputField
          label="Display Output"
          value={clientForm.displayOutput}
          onChange={(event) =>
            setClientForm((prev) => ({
              ...prev,
              displayOutput: event.target.value,
            }))
          }
        />
        <div className="space-y-2">
          <label className="font-display text-[10px] uppercase tracking-[0.18em] text-neon-lime">
            Aspect Ratio
          </label>
          <select
            value={clientForm.aspectRatio}
            onChange={(event) =>
              setClientForm((prev) => ({
                ...prev,
                aspectRatio: event.target.value,
              }))
            }
            className="w-full rounded-md border border-neon-cyan/40 bg-black/60 px-3 py-2 text-sm text-neon-cyan outline-none transition focus:border-neon-lime"
          >
            <option value="">Auto (use width/height)</option>
            <option value="16:9">16:9</option>
            <option value="16:10">16:10</option>
            <option value="21:9">21:9</option>
            <option value="4:3">4:3</option>
          </select>
        </div>
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
            })
          }
        >
          Save Client Config
        </Button>
      </div>
    </Card>
  );

  const storagePanel = (
    <Card className="pixel-frame">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        Shared Storage
      </h2>
      <div className="mt-4">
        <SharedStorageSettings
          settings={sharedStorageSettings}
          busy={busy}
          onSave={onSaveSharedStorageSettings}
          onTest={onTestSharedStorageConfig}
        />
      </div>
    </Card>
  );

  const connectionPanel = (
    <Card className="pixel-frame">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
        Connection Provider
      </h2>
      <p className="mt-2 text-[1.1rem] text-[#a8bed6]">
        Choose how to connect to your remote instance. WireGuard creates a
        direct VPN tunnel. Tailscale uses your Tailscale mesh network for
        simpler setup.
      </p>

      <div className="mt-4 space-y-3">
        <label className="flex items-center gap-3 cursor-pointer">
          <input
            type="radio"
            name="connectionProvider"
            value="wireguard"
            checked={connectionProvider === "wireguard"}
            onChange={() => setConnectionProvider("wireguard")}
            className="text-neon-cyan"
          />
          <span className="text-[1.15rem] text-white">WireGuard</span>
        </label>
        <label className="flex items-center gap-3 cursor-pointer">
          <input
            type="radio"
            name="connectionProvider"
            value="tailscale"
            checked={connectionProvider === "tailscale"}
            onChange={() => setConnectionProvider("tailscale")}
            className="text-neon-cyan"
          />
          <span className="text-[1.15rem] text-white">Tailscale</span>
        </label>
      </div>

      <div className="mt-4">
        <Button
          disabled={busy}
          onClick={() =>
            onSaveConnectionProvider({
              connectionProvider,
            })
          }
        >
          Save Connection Provider
        </Button>
      </div>

      <div className="mt-4 border-t border-[#3b4067] pt-4">
        <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
          Tailscale API Key
        </h3>
        <p className="mt-1 text-[1.1rem] text-[#a8bed6]">
          Required if using Tailscale.{" "}
          <a
            className="text-neon-cyan underline decoration-[#61f7ff] underline-offset-2 hover:text-white"
            href="https://login.tailscale.com/admin/settings/keys"
            target="_blank"
            rel="noreferrer"
          >
            Get your Tailscale API key
          </a>
          .
        </p>
        <InputField
          label="Tailscale API Key"
          type="password"
          value={tailscaleApiKey}
          onChange={(event) => setTailscaleApiKey(event.target.value)}
        />
        <div className="mt-3">
          <Button
            disabled={busy}
            onClick={() => onSaveTailscaleApiKey(tailscaleApiKey.trim())}
          >
            Save Tailscale API Key
          </Button>
        </div>
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
    <main className="crt-surface min-h-screen bg-hero-glow px-4 pb-8 pt-6 md:px-8">
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-4">
        <div className="flex items-center justify-between">
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

        <section className="grid gap-4 md:grid-cols-[240px_1fr]">
          <Card className="pixel-frame">
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
                Shared Storage
              </Button>
              <Button
                variant={section === "connection" ? "secondary" : "ghost"}
                onClick={() => setSection("connection")}
              >
                Connection
              </Button>
            </div>
          </Card>

          {panel}
        </section>
      </div>
    </main>
  );
}
