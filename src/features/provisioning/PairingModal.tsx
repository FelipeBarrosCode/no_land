import { useState } from "react";
import { Button } from "../../components/ui/Button";
import { InputField } from "../../components/ui/InputField";

interface Props {
  open: boolean;
  busy: boolean;
  wireguardIp: string;
  wireguardConfigPath: string;
  onSetupWireguardClient: () => Promise<void>;
  onSkip: () => Promise<void>;
  onSubmit: (pin: string) => Promise<void>;
}

const SUNSHINE_PORTS = [47984, 47989, 47990];

function buildSunshineUrls(serverIp: string) {
  return SUNSHINE_PORTS.map((port) => ({
    url: `http://${serverIp}:${port}`,
    port,
  }));
}

export function PairingModal({
  open,
  busy,
  wireguardIp,
  wireguardConfigPath,
  onSetupWireguardClient,
  onSkip,
  onSubmit
}: Props) {
  const [pin, setPin] = useState("");
  const [wireguardReadyConfirmed, setWireguardReadyConfirmed] = useState(false);
  const hostToType = wireguardIp || "pending";
  const sunshineUrls = wireguardIp ? buildSunshineUrls(wireguardIp) : [];

  if (!open) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-[#02040bdd] p-4">
      <div className="glass-panel pixel-frame crt-surface w-full max-w-lg p-6">
        <h3 className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base" data-text="Pair Moonlight">
          Pair Moonlight
        </h3>
        <ol className="mt-3 list-decimal space-y-1 pl-5 text-[1.35rem] leading-none text-[#d9efff]">
          <li>Import and enable this WireGuard client config first: {wireguardConfigPath || "pending"}.</li>
          <li>Disable any other active WireGuard tunnel on your client device.</li>
          <li>Open Moonlight and add PC manually with this IP: {hostToType}.</li>
          <li>When Moonlight shows a PIN, enter it below to complete pairing.</li>
          <li>If pairing fails, open Sunshine Web UI and enter the PIN there. Try these URLs in order:</li>
          <ul className="mt-1 mb-2 list-disc pl-8 text-[1.1rem] leading-snug text-[#d9efff]">
            {sunshineUrls.map(({ url, port }) => (
              <li key={port}>
                <a href={url} target="_blank" rel="noopener noreferrer" className="text-neon-cyan underline break-all">
                  {url}
                </a>
                {port === 47984 && " (try first)"}
              </li>
            ))}
          </ul>
          <li className="text-[1rem] text-[#8b9dc3] italic">
            If the browser says &quot;unsafe&quot; or shows an SSL error, that is normal for this setup. Click &quot;Advanced&quot; and proceed anyway — the connection is safe over WireGuard.
          </li>
        </ol>

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
            disabled={busy}
            onClick={() => onSkip()}
            variant="secondary"
          >
            Skip WireGuard Step And Continue
          </Button>

          {wireguardReadyConfirmed && (
            <p className="text-[1rem] leading-snug text-neon-lime">
              WireGuard marked as ready. Proceed with PIN submission.
            </p>
          )}

          <InputField
            label="Pairing PIN"
            placeholder="1234"
            value={pin}
            onChange={(event) => setPin(event.target.value.replace(/[^0-9]/g, ""))}
          />

          <Button disabled={busy || pin.length < 4 || !wireguardReadyConfirmed} onClick={() => onSubmit(pin)}>
            {busy ? "Submitting..." : "Submit PIN"}
          </Button>
        </div>
      </div>
    </div>
  );
}
