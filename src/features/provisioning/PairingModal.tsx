import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../../components/ui/Button";
import {
  configureMoonlightClient,
  launchMoonlightClient,
  resolveMoonlightDownloadUrl,
} from "../../lib/backend";
import type { MoonlightConfigureResult } from "../../lib/types";

interface Props {
  open: boolean;
  busy: boolean;
  wireguardConfigPath: string;
  onSetupWireguardClient: () => Promise<void>;
  onReconnectLocalWireguardClient: () => Promise<string | null>;
  onSkip: () => Promise<void>;
}

export function PairingModal({
  open,
  busy,
  wireguardConfigPath,
  onSetupWireguardClient,
  onReconnectLocalWireguardClient,
  onSkip
}: Props) {
  const [wireguardReadyConfirmed, setWireguardReadyConfirmed] = useState(false);
  const [launchingMoonlight, setLaunchingMoonlight] = useState(false);
  const [moonlightError, setMoonlightError] = useState<MoonlightConfigureResult | null>(null);
  const sunshineUrl = "https://10.77.0.1:47990";

  async function openMoonlightOrFallback() {
    try {
      await launchMoonlightClient();
      return;
    } catch {
      const downloadUrl = await resolveMoonlightDownloadUrl();

      try {
        await openUrl(downloadUrl);
      } catch {
        window.open(downloadUrl, "_blank", "noopener,noreferrer");
      }
    }
  }

  async function handleContinue() {
    setMoonlightError(null);
    setLaunchingMoonlight(true);

    try {
      await onSkip();

      const result = await configureMoonlightClient({
        apply: true,
        network: "auto",
        preferCodec: "auto",
      });

      if (!result.installed) {
        await openMoonlightOrFallback();
        return;
      }

      if (!result.success) {
        setMoonlightError(result);
        return;
      }

      await openMoonlightOrFallback();
    } finally {
      setLaunchingMoonlight(false);
    }
  }

  if (!open) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-[#02040bdd] p-4">
      <div className="glass-panel pixel-frame crt-surface w-full max-w-lg p-6">
        <h3 className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base" data-text="Pair Moonlight">
          Pair Moonlight
        </h3>
        <ol className="mt-3 list-decimal space-y-2 pl-5 text-[1.25rem] leading-snug text-[#d9efff]">
          <li>
            Click <span className="text-neon-lime">Setup WireGuard On This PC</span>.
            <div className="mt-1 text-[1.05rem] text-[#9ab0cc]">Config: {wireguardConfigPath || "pending"}</div>
          </li>
          <li>
            Open Moonlight and add PC manually with IP: <span className="text-neon-cyan">10.77.0.1</span>.
          </li>
          <li>
            Open Sunshine in browser: <a href={sunshineUrl} target="_blank" rel="noopener noreferrer" className="text-neon-cyan underline break-all">{sunshineUrl}</a>
          </li>
          <li>
            In Sunshine top bar, click <span className="text-neon-lime">PIN</span> and enter the PIN shown by Moonlight.
          </li>
          <li>
            When Moonlight connects successfully, click <span className="text-neon-lime">Continue</span> below.
          </li>
        </ol>

        <p className="mt-3 text-[1rem] text-[#8b9dc3] italic">
          If the browser shows an SSL warning, click Advanced and continue. This is expected on this local WireGuard path.
        </p>

        {moonlightError && (
          <div className="mt-4 rounded border border-red-500/40 bg-red-950/30 p-4 text-[1.05rem] text-red-100">
            <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-red-200">
              Moonlight Setup Incomplete
            </h4>
            <p className="mt-2">
              Moonlight is installed, but Noland could not safely update its settings.
            </p>
            {moonlightError.error && <p className="mt-2 text-red-200">{moonlightError.error}</p>}
            {moonlightError.settingsLocation && (
              <p className="mt-2 text-red-200">Settings store: {moonlightError.settingsLocation}</p>
            )}
            <p className="mt-2 text-red-200">
              Paired hosts, credentials, and saved connection data were not modified.
            </p>
            <div className="mt-3 flex flex-wrap gap-2">
              <Button
                variant="secondary"
                onClick={() => void openMoonlightOrFallback()}
                disabled={launchingMoonlight}
              >
                Launch Moonlight Anyway
              </Button>
              <Button variant="ghost" onClick={() => setMoonlightError(null)} disabled={launchingMoonlight}>
                Close
              </Button>
            </div>
          </div>
        )}

        <div className="mt-4 grid gap-3">
          <Button
            disabled={busy}
            loading={busy}
            loadingText="Setting up WireGuard..."
            onClick={async () => {
              await onSetupWireguardClient();
              setWireguardReadyConfirmed(true);
            }}
            variant="ghost"
          >
            Setup WireGuard On This PC
          </Button>

          <Button
            disabled={busy || !wireguardReadyConfirmed}
            loading={busy && wireguardReadyConfirmed}
            loadingText="Reconnecting..."
            onClick={() => onReconnectLocalWireguardClient()}
            variant="secondary"
          >
            Reconnect WireGuard
          </Button>

          {wireguardReadyConfirmed && (
            <p className="text-[1rem] leading-snug text-neon-lime">
              WireGuard marked as ready. Complete Moonlight + Sunshine PIN steps, then click Continue.
            </p>
          )}

          <Button
            disabled={busy || launchingMoonlight || !wireguardReadyConfirmed}
            loading={launchingMoonlight}
            loadingText="Continuing..."
            onClick={() => void handleContinue()}
          >
            Continue To Moonlight
          </Button>
        </div>
      </div>
    </div>
  );
}
