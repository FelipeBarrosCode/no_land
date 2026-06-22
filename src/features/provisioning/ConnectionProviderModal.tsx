import { Button } from "../../components/ui/Button";
import type { OrchestrationState, PersistedAppState } from "../../lib/types";

interface Props {
  open: boolean;
  appState: PersistedAppState;
  busy: boolean;
  onSelectWireguard: () => Promise<unknown>;
  onSelectTailscale: () => Promise<unknown>;
}

const providerSelectionStates = new Set<OrchestrationState>([
  "SelectingConnectionProvider",
]);

export function ConnectionProviderModal({
  open,
  appState,
  busy,
  onSelectWireguard,
  onSelectTailscale,
}: Props) {
  const isOpen =
    open && providerSelectionStates.has(appState.orchestrationState);

  if (!isOpen) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-[#02040bdd] p-4">
      <div className="glass-panel pixel-frame crt-surface w-full max-w-2xl p-6">
        <h3
          className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base"
          data-text="Choose Connection Provider"
        >
          Choose Connection Provider
        </h3>

        <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
          Select how you want to connect to your remote instance.
        </p>

        <div className="mt-6 grid gap-4 md:grid-cols-2">
          {/* WireGuard Option */}
          <div className="border border-[#3d426f] bg-[#10152f] p-5 hover:border-neon-cyan transition-colors">
            <h4 className="font-display text-[12px] uppercase tracking-[0.12em] text-neon-lime">
              WireGuard
            </h4>
            <p className="mt-2 text-[1.05rem] leading-snug text-[#cfe7ff]">
              Direct VPN tunnel between your device and the remote instance.
              Fast and reliable. Requires importing a WireGuard config into the
              WireGuard app.
            </p>
            <div className="mt-4">
              <Button onClick={() => onSelectWireguard()} disabled={busy}>
                Use WireGuard
              </Button>
            </div>
          </div>

          {/* Tailscale Option */}
          <div className="border border-[#3d426f] bg-[#10152f] p-5 hover:border-neon-lime transition-colors">
            <h4 className="font-display text-[12px] uppercase tracking-[0.12em] text-neon-cyan">
              Tailscale
            </h4>
            <p className="mt-2 text-[1.05rem] leading-snug text-[#cfe7ff]">
              Uses your Tailscale mesh network. No port forwarding or complex
              config needed. Requires a Tailscale API key and Tailscale
              installed on your device.
            </p>
            <div className="mt-4">
              <Button onClick={() => onSelectTailscale()} disabled={busy}>
                Use Tailscale
              </Button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
