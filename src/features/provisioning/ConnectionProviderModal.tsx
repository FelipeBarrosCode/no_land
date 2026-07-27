import { Button } from "../../components/ui/Button";
import type { OrchestrationState, PersistedAppState } from "../../lib/types";

interface Props {
  open: boolean;
  appState: PersistedAppState;
  busy: boolean;
  onSelectWireguard: () => Promise<unknown>;
}

const providerSelectionStates = new Set<OrchestrationState>([
  "SelectingConnectionProvider",
]);

export function ConnectionProviderModal({
  open,
  appState,
  busy,
  onSelectWireguard,
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
          data-text="Managed Tunnel Setup"
        >
          Managed Tunnel Setup
        </h3>

        <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
          Noland uses a managed GotaTun userspace tunnel for the desktop
          connection flow. We generate the config, activate the tunnel, and
          verify connectivity before moving on to Moonlight pairing.
        </p>

        <div className="mt-6 border border-[#3d426f] bg-[#10152f] p-5">
          <h4 className="font-display text-[12px] uppercase tracking-[0.12em] text-neon-lime">
            Managed GotaTun / WireGuard Tunnel
          </h4>
          <p className="mt-2 text-[1.05rem] leading-snug text-[#cfe7ff]">
            Direct secure tunnel between your device and the remote instance.
            On macOS and Linux, Noland activates the WireGuard-compatible tunnel
            using GotaTun instead of relying on a separate manual import flow.
          </p>
          <div className="mt-4">
            <Button onClick={() => onSelectWireguard()} disabled={busy}>
              Use Managed Tunnel
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
