import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { ArcadeSoundToggle } from "../../components/ui/ArcadeSoundToggle";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import type {
  MoonlightPreferences,
  PlatformCredentialsUpdate,
  PersistedAppState,
  ServerPreferencesUpdate,
  SshCredentialsUpdate
} from "../../lib/types";

type SettingsSection = "profile" | "server" | "client";
type ClientForm = Record<keyof MoonlightPreferences, string>;

interface Props {
  appState: PersistedAppState;
  busy: boolean;
  onSaveApiKey: (apiKey: string) => Promise<void>;
  onSavePlatformCredentials: (payload: PlatformCredentialsUpdate) => Promise<void>;
  onSaveServerPreferences: (payload: Partial<ServerPreferencesUpdate>) => Promise<void>;
  onSaveMoonlightPreferences: (payload: MoonlightPreferences) => Promise<void>;
  onSaveSshCredentials: (payload: SshCredentialsUpdate) => Promise<void>;
}

function toNumber(value: string, fallback: number): number {
  const parsed = Number.parseFloat(value);
  if (Number.isNaN(parsed)) {
    return fallback;
  }

  return parsed;
}

const clientFields: Array<keyof MoonlightPreferences> = [
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
  "detectnetblocking"
];

export function SettingsScreen({
  appState,
  busy,
  onSaveApiKey,
  onSavePlatformCredentials,
  onSaveServerPreferences,
  onSaveMoonlightPreferences,
  onSaveSshCredentials
}: Props) {
  const [section, setSection] = useState<SettingsSection>("profile");
  const [apiKey, setApiKey] = useState(appState.credentials.vastApiKey);
  const [platformUsername, setPlatformUsername] = useState(appState.credentials.appUsername);
  const [platformPassword, setPlatformPassword] = useState(appState.credentials.appPassword);
  const [sshUsername, setSshUsername] = useState(
    appState.ssh.sshUsername || appState.credentials.appUsername
  );
  const [sshPassword, setSshPassword] = useState(
    appState.ssh.sshPassword || appState.credentials.appPassword
  );

  const [serverForm, setServerForm] = useState({
    minReliability: appState.serverPreferences.minReliability.toString(),
    storageGb: appState.serverPreferences.storageGb.toString(),
    templateHash: appState.serverPreferences.templateHash
  });

  const [clientForm, setClientForm] = useState<ClientForm>(() => ({
    bitrate: appState.moonlightPreferences.bitrate.toString(),
    fps: appState.moonlightPreferences.fps.toString(),
    width: appState.moonlightPreferences.width.toString(),
    height: appState.moonlightPreferences.height.toString(),
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
    detectnetblocking: appState.moonlightPreferences.detectnetblocking.toString()
  }));

  useEffect(() => {
    setApiKey(appState.credentials.vastApiKey);
    setPlatformUsername(appState.credentials.appUsername);
    setPlatformPassword(appState.credentials.appPassword);
    setSshUsername(appState.ssh.sshUsername || appState.credentials.appUsername);
    setSshPassword(appState.ssh.sshPassword || appState.credentials.appPassword);
    setServerForm({
      minReliability: appState.serverPreferences.minReliability.toString(),
      storageGb: appState.serverPreferences.storageGb.toString(),
      templateHash: appState.serverPreferences.templateHash
    });
    setClientForm({
      bitrate: appState.moonlightPreferences.bitrate.toString(),
      fps: appState.moonlightPreferences.fps.toString(),
      width: appState.moonlightPreferences.width.toString(),
      height: appState.moonlightPreferences.height.toString(),
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
      detectnetblocking: appState.moonlightPreferences.detectnetblocking.toString()
    });
  }, [appState]);

  const profilePanel = (
    <Card className="pixel-frame">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">Profile</h2>
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
          disabled={busy || platformUsername.trim().length < 3 || platformPassword.trim().length < 6}
          onClick={() =>
            onSavePlatformCredentials({
              appUsername: platformUsername.trim(),
              appPassword: platformPassword.trim()
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
          Used after key-based connection when the VM asks for username/password.
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
            disabled={busy || !sshUsername.trim() || sshPassword.trim().length < 4}
            onClick={() =>
              onSaveSshCredentials({
                sshUsername: sshUsername.trim(),
                sshPassword: sshPassword.trim()
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
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">Server Configuration</h2>
      <div className="mt-4 grid gap-3 md:grid-cols-3">
        <InputField
          label="Min Reliability (0.8-1)"
          value={serverForm.minReliability}
          onChange={(event) => setServerForm((prev) => ({ ...prev, minReliability: event.target.value }))}
        />
        <InputField
          label="Storage (GB)"
          value={serverForm.storageGb}
          onChange={(event) => setServerForm((prev) => ({ ...prev, storageGb: event.target.value }))}
        />
        <InputField
          label="Template Hash"
          value={serverForm.templateHash}
          onChange={(event) => setServerForm((prev) => ({ ...prev, templateHash: event.target.value }))}
        />
      </div>
      <div className="mt-4">
        <Button
          disabled={busy || !serverForm.templateHash.trim()}
          onClick={() =>
            onSaveServerPreferences({
              minReliability: Math.max(
                0.8,
                toNumber(serverForm.minReliability, appState.serverPreferences.minReliability)
              ),
              storageGb: Math.max(30, Math.round(toNumber(serverForm.storageGb, appState.serverPreferences.storageGb))),
              templateHash: serverForm.templateHash.trim(),
              maxHourlyPrice: appState.serverPreferences.maxHourlyPrice,
              minHourlyPrice: appState.serverPreferences.minHourlyPrice,
              requireVerified: appState.serverPreferences.requireVerified,
              requireDatacenter: appState.serverPreferences.requireDatacenter,
              includeOnDemand: appState.serverPreferences.includeOnDemand,
              includeInterruptible: appState.serverPreferences.includeInterruptible,
              includeReserved: appState.serverPreferences.includeReserved,
              requireStaticIp: true,
              requireAvx: appState.serverPreferences.requireAvx,
              minGpuCount: 1,
              minGpuRamGb: appState.serverPreferences.minGpuRamGb,
              minCpuCores: appState.serverPreferences.minCpuCores,
              minInetDownMbps: appState.serverPreferences.minInetDownMbps,
              minInetUpMbps: appState.serverPreferences.minInetUpMbps,
              geolocationCountryCode: appState.serverPreferences.geolocationCountryCode
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
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">Client (Moonlight)</h2>
      <div className="mt-4 grid gap-3 md:grid-cols-4">
        {clientFields.map((key) => (
          <InputField
            key={key}
            label={key}
            value={clientForm[key]}
            onChange={(event) => setClientForm((prev) => ({ ...prev, [key]: event.target.value }))}
          />
        ))}
      </div>
      <div className="mt-4">
        <Button
          disabled={busy}
          onClick={() =>
            onSaveMoonlightPreferences({
              bitrate: Math.max(10000, Math.round(toNumber(clientForm.bitrate, appState.moonlightPreferences.bitrate))),
              fps: Math.max(30, Math.round(toNumber(clientForm.fps, appState.moonlightPreferences.fps))),
              width: Math.max(1280, Math.round(toNumber(clientForm.width, appState.moonlightPreferences.width))),
              height: Math.max(720, Math.round(toNumber(clientForm.height, appState.moonlightPreferences.height))),
              hostaudio: Math.round(toNumber(clientForm.hostaudio, appState.moonlightPreferences.hostaudio)),
              showperfoverlay: Math.round(toNumber(clientForm.showperfoverlay, appState.moonlightPreferences.showperfoverlay)),
              keepawake: Math.round(toNumber(clientForm.keepawake, appState.moonlightPreferences.keepawake)),
              framepacing: Math.round(toNumber(clientForm.framepacing, appState.moonlightPreferences.framepacing)),
              vsync: Math.round(toNumber(clientForm.vsync, appState.moonlightPreferences.vsync)),
              hdr: Math.round(toNumber(clientForm.hdr, appState.moonlightPreferences.hdr)),
              videocfg: Math.round(toNumber(clientForm.videocfg, appState.moonlightPreferences.videocfg)),
              videodec: Math.round(toNumber(clientForm.videodec, appState.moonlightPreferences.videodec)),
              yuv444: Math.round(toNumber(clientForm.yuv444, appState.moonlightPreferences.yuv444)),
              gameopts: Math.round(toNumber(clientForm.gameopts, appState.moonlightPreferences.gameopts)),
              gamepadmouse: Math.round(toNumber(clientForm.gamepadmouse, appState.moonlightPreferences.gamepadmouse)),
              detectnetblocking: Math.round(
                toNumber(clientForm.detectnetblocking, appState.moonlightPreferences.detectnetblocking)
              )
            })
          }
        >
          Save Client Config
        </Button>
      </div>
    </Card>
  );

  const panel = section === "profile" ? profilePanel : section === "server" ? serverPanel : clientPanel;

  return (
    <main className="crt-surface min-h-screen bg-hero-glow px-4 pb-8 pt-6 md:px-8">
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-4">
        <div className="flex items-center justify-between">
          <div>
            <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">Settings</p>
            <h1
              className="pixel-heading glitch-title font-display text-lg text-white md:text-xl"
              data-text="Preferences"
            >
              Preferences
            </h1>
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
              <Button variant={section === "profile" ? "secondary" : "ghost"} onClick={() => setSection("profile")}>
                Profile
              </Button>
              <Button variant={section === "server" ? "secondary" : "ghost"} onClick={() => setSection("server")}>
                Server Configuration
              </Button>
              <Button variant={section === "client" ? "secondary" : "ghost"} onClick={() => setSection("client")}>
                Client
              </Button>
            </div>
          </Card>

          {panel}
        </section>
      </div>
    </main>
  );
}
