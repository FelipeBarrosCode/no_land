import { useMemo } from "react";
import { Link } from "react-router-dom";
import { ArcadeSoundToggle } from "../../components/ui/ArcadeSoundToggle";
import type { BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { PROVISIONING_ORDER } from "../../lib/constants";
import type { PersistedAppState, ProvisioningEvent } from "../../lib/types";
import { PairingModal } from "./PairingModal";

interface Props {
  appState: PersistedAppState;
  logs: ProvisioningEvent[];
  busy: boolean;
  blockingAction: BlockingActionState | null;
  onSkipPairing: () => Promise<void>;
  onSetupWireguardClient: () => Promise<void>;
  onReconnectLocalWireguardClient: () => Promise<string | null>;
  sleepPreventionActive: boolean;
  onStartSleepPrevention: () => Promise<string | null>;
  onStopSleepPrevention: () => Promise<string | null>;
}

function formatTime(timestamp: string): string {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return "--";
  }

  return date.toLocaleTimeString();
}

export function ProvisioningScreen({
  appState,
  logs,
  busy,
  blockingAction,
  onSkipPairing,
  onSetupWireguardClient,
  onReconnectLocalWireguardClient,
  sleepPreventionActive,
  onStartSleepPrevention,
  onStopSleepPrevention
}: Props) {
  const currentIndex = useMemo(() => {
    const index = PROVISIONING_ORDER.indexOf(appState.orchestrationState as (typeof PROVISIONING_ORDER)[number]);
    return index === -1 ? 0 : index;
  }, [appState.orchestrationState]);

  return (
    <main className="crt-surface min-h-screen bg-hero-glow px-4 pb-8 pt-6 md:px-8">
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div>
            <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">Provisioning</p>
            <h1
              className="pixel-heading glitch-title font-display text-lg text-white md:text-xl"
              data-text="Session Setup Timeline"
            >
              Session Setup Timeline
            </h1>
          </div>

          <div className="flex items-center gap-2">
            <ArcadeSoundToggle />
            <Link to="/">
              <Button variant="ghost">Back to Dashboard</Button>
            </Link>
          </div>
        </div>

        <section className="grid gap-4 lg:grid-cols-[1.1fr_1fr]">
          <Card className="pixel-frame">
            {blockingAction?.key === "provisioning.flow" && (
              <div className="mb-4" aria-busy="true" aria-live="polite">
                <div className="mb-2 flex items-start justify-between gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center justify-between gap-2 text-[1.05rem] text-[#9ec4df]">
                      <span>{blockingAction.label}</span>
                      <span>
                        {typeof blockingAction.progress === "number"
                          ? `${Math.round(blockingAction.progress)}%`
                          : "Working..."}
                      </span>
                    </div>
                  </div>
                  <Link to="/">
                    <Button variant="ghost" className="px-3 py-1 text-[10px]">
                      Close
                    </Button>
                  </Link>
                </div>
                <div className="h-2 overflow-hidden border border-[#3f476c] bg-[#0b0f23] shadow-[inset_0_0_0_1px_#121731]">
                  <div
                    className="h-full bg-gradient-to-r from-[#2d5844] via-[#61f7ff] to-[#7bff48] transition-[width] duration-300"
                    style={{ width: `${Math.max(0, Math.min(100, blockingAction.progress ?? 0))}%` }}
                  />
                </div>
                {blockingAction.detail && (
                  <p className="mt-2 text-[1.05rem] text-[#8fb5d4]">{blockingAction.detail}</p>
                )}
              </div>
            )}
            <h2 className="font-display text-sm uppercase tracking-[0.12em] text-neon-lime">Pipeline Steps</h2>
            <ul className="mt-4 space-y-2">
              {PROVISIONING_ORDER.map((step, index) => {
                const complete = index < currentIndex;
                const active = step === appState.orchestrationState;

                return (
                  <li
                    key={step}
                    className={`border px-3 py-2 text-[1.2rem] leading-none transition ${
                      active
                        ? "border-neon-cyan bg-[#182a43]"
                        : complete
                          ? "border-neon-lime bg-[#233720]"
                          : "border-[#3d426f] bg-[#10152f]"
                    }`}
                  >
                    <span className="font-display text-[10px] uppercase tracking-[0.1em] text-slate-100">{step}</span>
                  </li>
                );
              })}
            </ul>
          </Card>

          <Card className="pixel-frame">
            <div className="flex items-center justify-between gap-2">
              <h2 className="font-display text-sm uppercase tracking-[0.12em] text-neon-lime">Progress Logs</h2>
              <Button
                variant={sleepPreventionActive ? "secondary" : "ghost"}
                disabled={busy}
                loading={busy && blockingAction?.key !== "provisioning.flow"}
                loadingText={sleepPreventionActive ? "Stopping..." : "Starting..."}
                onClick={() => (sleepPreventionActive ? onStopSleepPrevention() : onStartSleepPrevention())}
              >
                {sleepPreventionActive ? "Stop Awake" : "Keep PC Awake"}
              </Button>
            </div>
            <details className="mt-3 border border-[#3d426f] bg-[#10152f] p-3" open>
              <summary className="cursor-pointer font-display text-[10px] uppercase text-[#b4c8de]">Show details</summary>
              <div className="mt-3 max-h-[440px] space-y-2 overflow-auto pr-1 text-xs">
                {logs.length === 0 ? (
                  <p className="text-[1.25rem] text-[#98adc9]">No events yet.</p>
                ) : (
                  logs.map((entry, index) => (
                    <div
                      key={`${entry.timestamp}-${index}`}
                      className={`border p-2 ${
                        entry.isError ? "border-[#ff687d] bg-[#481b2a]" : "border-[#3d426f] bg-[#161b3b]"
                      }`}
                    >
                      <div className="mb-1 flex items-center justify-between gap-2">
                        <span className="font-display text-[10px] uppercase text-slate-100">{entry.state}</span>
                        <span className="text-[1.1rem] text-[#98adc9]">{formatTime(entry.timestamp)}</span>
                      </div>
                      <p className="text-[1.2rem] leading-none text-[#d9efff]">{entry.message}</p>
                      {entry.details && <p className="mt-1 text-[1.1rem] leading-none text-[#98adc9]">{entry.details}</p>}
                    </div>
                  ))
                )}
              </div>
            </details>
          </Card>
        </section>
      </div>

      <PairingModal
        open={appState.orchestrationState === "AwaitingPairPin" || appState.orchestrationState === "Pairing"}
        busy={busy}
        wireguardConfigPath={appState.wireguard.configPath}
        onSetupWireguardClient={onSetupWireguardClient}
        onReconnectLocalWireguardClient={onReconnectLocalWireguardClient}
        onSkip={onSkipPairing}
      />
    </main>
  );
}
