import { useState } from "react";
import { Button } from "../../components/ui/Button";

interface Props {
  open: boolean;
  busy: boolean;
  wireguardIp: string;
  wireguardConfigPath: string;
  onSetupWireguardClient: () => Promise<void>;
  onReconnectLocalWireguardClient: () => Promise<string | null>;
  onSkip: () => Promise<void>;
}

export function PairingModal({
  open,
  busy,
  wireguardIp,
  wireguardConfigPath,
  onSetupWireguardClient,
  onReconnectLocalWireguardClient,
  onSkip
}: Props) {
  const [wireguardReadyConfirmed, setWireguardReadyConfirmed] = useState(false);
  const hostToType = wireguardIp || "pending";
  const sunshineUrl = `https://${hostToType}:47990`;

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

        <div className="mt-4 grid gap-3">
          <Button
            disabled={busy}
            onClick={async () => {
              await onSetupWireguardClient();
              setWireguardReadyConfirmed(true);
            }}
            variant="ghost"
          >
            {busy ? "Setting up WireGuard..." : "Setup WireGuard On This PC"}
          </Button>

          <Button
            disabled={busy || !wireguardReadyConfirmed}
            onClick={() => onReconnectLocalWireguardClient()}
            variant="secondary"
          >
            {busy ? "Reconnecting..." : "Reconnect WireGuard"}
          </Button>

          {wireguardReadyConfirmed && (
            <p className="text-[1rem] leading-snug text-neon-lime">
              WireGuard marked as ready. Complete Moonlight + Sunshine PIN steps, then click Continue.
            </p>
          )}

          <Button disabled={busy || !wireguardReadyConfirmed} onClick={() => onSkip()}>
            {busy ? "Continuing..." : "Continue"}
          </Button>
        </div>
      </div>
    </div>
  );
}
