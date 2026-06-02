import { useMemo, useState } from "react";
import { Button } from "../../components/ui/Button";
import type {
  OrchestrationState,
  PersistedAppState,
  SetupStage,
} from "../../lib/types";

interface Props {
  open: boolean;
  appState: PersistedAppState;
  busy: boolean;
  onSetupWireguardAppHandoff: () => Promise<unknown>;
  onOpenWireguardApp: () => Promise<void>;
  onDownloadWireguardConfig: () => Promise<string | null>;
  onVerifyWireguard: () => Promise<unknown>;
  onDetectMoonlight: () => Promise<unknown>;
  onSetupMoonlightSunshine: () => Promise<unknown>;
  onSubmitMoonlightPin: (pin: string) => Promise<unknown>;
  onRetrySetupStage: (stage: SetupStage) => Promise<unknown>;
}

const wireguardStages = new Set<SetupStage>([
  "wireguard_config_generated",
  "wireguard_app_handoff_started",
  "wireguard_waiting_for_import",
  "wireguard_waiting_for_activation",
  "wireguard_verifying",
  "wireguard_connected",
  "failed",
]);

const streamingPrepStages = new Set<SetupStage>([
  "moonlight_sunshine_ready_to_setup",
  "sunshine_credentials_configuring",
  "sunshine_verifying",
  "moonlight_detecting",
]);

const pinStages = new Set<SetupStage>([
  "moonlight_pairing_started",
  "moonlight_pin_received",
  "sunshine_pin_submitting",
  "moonlight_sunshine_paired",
  "setup_complete",
]);

export function PostWireguardModal({
  open,
  appState,
  busy,
  onSetupWireguardAppHandoff,
  onOpenWireguardApp,
  onDownloadWireguardConfig,
  onVerifyWireguard,
  onDetectMoonlight,
  onSetupMoonlightSunshine,
  onSubmitMoonlightPin,
  onRetrySetupStage,
}: Props) {
  const [pin, setPin] = useState("");
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  void onVerifyWireguard;
  void onDetectMoonlight;
  const setup = appState.postWireguardSetup;
  const activeInstanceId = appState.instance.instanceId;
  const configMatchesActiveInstance =
    activeInstanceId !== null &&
    activeInstanceId !== undefined &&
    setup.currentInstanceId === activeInstanceId;
  const isMacManual = setup.wireguardSetupMode === "wireguard_app_macos_manual";
  const isWireguardPhase =
    wireguardStages.has(setup.stage) && !streamingPrepStages.has(setup.stage);
  const hasStartedManualSetup =
    setup.stage !== "wireguard_config_generated" &&
    setup.stage !== "pre_wireguard_existing_flow";
  const canShowWireguardConfig =
    configMatchesActiveInstance && setup.wireguardConfig.trim().length > 0;
  const isStreamingPrepPhase = streamingPrepStages.has(setup.stage);
  const stageShowsPinSubmission = pinStages.has(setup.stage);
  const showPinInput = stageShowsPinSubmission;
  const orchestrationShowsPinSubmission = new Set<OrchestrationState>([
    "MoonlightPairingStarted",
    "MoonlightPinReceived",
    "SunshinePinSubmitting",
    "MoonlightSunshinePaired",
  ]).has(appState.orchestrationState);
  const moonlightChecked =
    stageShowsPinSubmission ||
    orchestrationShowsPinSubmission ||
    setup.moonlightInstalled ||
    !!setup.lastError?.code.includes("moonlight");
  const pinRetryError =
    setup.lastError?.stage === "moonlight_pin_received" ||
    setup.lastError?.stage === "sunshine_pin_submitting";

  const instructions = useMemo(() => {
    if (isMacManual) {
      return [
        "Open the WireGuard app.",
        "Import the downloaded .conf file, or create a tunnel and paste the config.",
        "Activate the tunnel in the WireGuard app.",
        "Return here and click Done to continue.",
      ];
    }

    return [
      "Open the WireGuard app.",
      "Import the generated tunnel if prompted.",
      "Activate the tunnel in WireGuard.",
      "Return here and click Done to continue.",
    ];
  }, [isMacManual]);

  if (!open) {
    return null;
  }

  async function copyConfig() {
    try {
      await navigator.clipboard.writeText(setup.wireguardConfig);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-[#02040bdd] p-4">
      <div className="glass-panel pixel-frame crt-surface w-full max-w-3xl p-6">
        <h3
          className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base"
          data-text="Secure Tunnel Setup"
        >
          {isWireguardPhase
            ? "Secure Tunnel Setup"
            : "Moonlight & Sunshine Setup"}
        </h3>

        {isWireguardPhase ? (
          <>
            <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
              {isMacManual
                ? hasStartedManualSetup
                  ? "To finish the secure connection setup, import this tunnel into the WireGuard app."
                  : "Click Start Manual Setup to load the current tunnel config for this instance, then import it into the WireGuard app."
                : "WireGuard app is required for the tunnel handoff. Import and activate the generated tunnel, then continue."}
            </p>

            <ol className="mt-4 list-decimal space-y-2 pl-5 text-[1.08rem] leading-snug text-[#cfe7ff]">
              {instructions.map((instruction) => (
                <li key={instruction}>{instruction}</li>
              ))}
            </ol>

            {!configMatchesActiveInstance && (
              <p className="mt-4 text-[1rem] text-[#9ab0cc]">
                Loading the current WireGuard config for this instance. If this
                does not update, click Start Manual Setup.
              </p>
            )}

            {canShowWireguardConfig && (
              <textarea
                readOnly
                value={setup.wireguardConfig}
                className="mt-4 min-h-52 w-full border border-[#3d426f] bg-[#10152f] p-3 font-mono text-[0.9rem] text-[#d9efff]"
              />
            )}

            <div className="mt-4 flex flex-wrap gap-2">
              {canShowWireguardConfig && (
                <Button variant="ghost" onClick={() => void copyConfig()}>
                  Copy Config
                </Button>
              )}
              <Button
                variant="secondary"
                onClick={() => void onDownloadWireguardConfig()}
                disabled={busy}
              >
                Download .conf
              </Button>
              <Button
                variant="secondary"
                onClick={() => void onOpenWireguardApp()}
                disabled={busy}
              >
                Open WireGuard
              </Button>
              <Button
                onClick={() => void onSetupWireguardAppHandoff()}
                disabled={busy}
              >
                {isMacManual ? "Start Manual Setup" : "Open WireGuard & Import"}
              </Button>
              <Button
                variant="ghost"
                onClick={() => void onSetupMoonlightSunshine()}
                disabled={busy}
              >
                Done
              </Button>
            </div>

            {copyState === "copied" && (
              <p className="mt-2 text-[1rem] text-neon-lime">
                Config copied to clipboard.
              </p>
            )}
            {copyState === "failed" && (
              <p className="mt-2 text-[1rem] text-[#ffb2bf]">
                Clipboard copy failed. Use Download .conf instead.
              </p>
            )}
            {!!setup.wireguardExportPath && (
              <p className="mt-2 text-[1rem] text-[#9ab0cc]">
                Export path: {setup.wireguardExportPath}
              </p>
            )}
          </>
        ) : (
          <>
            {isStreamingPrepPhase ? (
              <>
                <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
                  Finishing Sunshine and Moonlight setup on{" "}
                  <span className="text-neon-cyan">10.77.0.1</span>. PIN entry
                  unlocks when this preparation is done.
                </p>
              </>
            ) : (
              <>
                <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
                  Sunshine and Moonlight are ready. Generate a PIN in Moonlight
                  and submit it below.
                </p>
                <div className="mt-4 border border-[#3d426f] bg-[#10152f] p-4 text-[1.02rem] text-[#cfe7ff]">
                  <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-cyan">
                    Moonlight Status
                  </h4>
                  <p className="mt-2">
                    {moonlightChecked
                      ? setup.moonlightInstalled
                        ? "Moonlight was detected on this machine. Use the manual PIN fallback below."
                        : "Moonlight was not detected on this machine. Use the manual PIN fallback from another Moonlight client, or install Moonlight here and retry setup."
                      : "Moonlight setup is still in progress."}
                  </p>
                </div>
              </>
            )}

            {showPinInput && (
              <div className="mt-5 border border-[#3d426f] bg-[#10152f] p-4">
                <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
                  Manual PIN Fallback
                </h4>
                <ol className="mt-3 list-decimal space-y-2 pl-5 text-[1.05rem] leading-snug text-[#cfe7ff]">
                  <li>
                    Open Moonlight and add the PC at{" "}
                    <span className="text-neon-cyan">10.77.0.1</span>.
                  </li>
                  <li>Generate the PIN in Moonlight.</li>
                  <li>
                    Paste that PIN below and Noland will submit it to Sunshine
                    automatically.
                  </li>
                  <li>
                    You can use this again anytime, even after setup says it is
                    complete.
                  </li>
                </ol>
                <input
                  value={pin}
                  onChange={(event) =>
                    setPin(event.target.value.replace(/\D+/g, ""))
                  }
                  placeholder="Enter the PIN shown in Moonlight"
                  className="mt-4 w-full border border-[#3d426f] bg-[#0f1430] px-3 py-2 text-[1.05rem] text-white outline-none"
                />
                <div className="mt-3 flex flex-wrap gap-2">
                  <Button
                    onClick={() => void onSubmitMoonlightPin(pin)}
                    disabled={busy || pin.length < 4}
                  >
                    Submit PIN to Sunshine
                  </Button>
                  {setup.lastError?.retryable && !pinRetryError && (
                    <Button
                      variant="ghost"
                      onClick={() =>
                        void onRetrySetupStage(setup.lastError!.stage)
                      }
                      disabled={busy}
                    >
                      Retry Current Step
                    </Button>
                  )}
                </div>
              </div>
            )}

            {setup.setupComplete && (
              <div className="mt-4 border border-neon-lime bg-[#1f3223] p-4 text-[1.08rem] text-[#d9ffca]">
                <p>
                  Setup complete. Your secure streaming connection is ready, and
                  you can still submit a fresh PIN below at any time.
                </p>
                <div className="mt-3 border border-[#4f6a4e] bg-[#152316] p-3 text-[1rem] text-[#e6ffd7]">
                  <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
                    Sunshine Login
                  </h4>
                  <p className="mt-2">
                    Username: <span className="text-white">user</span>
                  </p>
                  <p>
                    Password: <span className="text-white">password</span>
                  </p>
                </div>
              </div>
            )}
          </>
        )}

        {setup.lastError && (
          <div className="mt-4 border border-[#ff687d] bg-[#481b2a] p-4 text-[1.05rem] text-[#ffd3dc]">
            <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-[#ffc3cf]">
              {setup.lastError.code}
            </h4>
            <p className="mt-2">{setup.lastError.message}</p>
            {setup.lastError.details && (
              <p className="mt-2 text-[#ffbdc7]">{setup.lastError.details}</p>
            )}
            {setup.lastError.retryable && !pinRetryError && (
              <div className="mt-3">
                <Button
                  variant="ghost"
                  onClick={() => void onRetrySetupStage(setup.lastError!.stage)}
                  disabled={busy}
                >
                  Retry Current Step
                </Button>
              </div>
            )}
            {pinRetryError && (
              <p className="mt-3 text-[#ffbdc7]">
                Generate a fresh PIN in Moonlight and submit it again here.
              </p>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
