import { useMemo, useState } from "react";
import { Button } from "../../components/ui/Button";
import { SpriteIcon } from "../../components/ui/SpriteIcon";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { APP_PROMPTS } from "../../prompts/appPrompts";
import {
  MOONLIGHT_DOWNLOAD_URL,
  WIREGUARD_DOWNLOAD_URL,
} from "../../lib/constants";
import type {
  OrchestrationState,
  PersistedAppState,
  SetupStage,
  MoonlightPairingSessionResponse,
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
  activeMoonlightPairing: MoonlightPairingSessionResponse | null;
  onPrepareMoonlightPairingHandoff: () => Promise<MoonlightPairingSessionResponse | null>;
  onCompleteMoonlightPairingHandoff: (pin: string) => Promise<unknown>;
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
  activeMoonlightPairing,
  onPrepareMoonlightPairingHandoff,
  onCompleteMoonlightPairingHandoff,
  onRetrySetupStage,
}: Props) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  const [pairingCopyState, setPairingCopyState] = useState<
    "idle" | "copied" | "failed"
  >("idle");
  void onVerifyWireguard;
  void onDetectMoonlight;
  void onOpenWireguardApp;

  const setup = appState.postWireguardSetup;
  const activeInstanceId = appState.instance.instanceId;
  const moonlightHost =
    setup.moonlightHost || appState.moonlight.hostAddress || "10.77.0.1";
  const sunshineUrl = `https://${moonlightHost}:47990/`;
  const sunshineUsername =
    setup.sunshineUsername || appState.credentials.appUsername || "";
  const sunshinePassword = appState.credentials.appPassword || "";
  const configMatchesActiveInstance =
    activeInstanceId !== null &&
    activeInstanceId !== undefined &&
    setup.currentInstanceId === activeInstanceId;
  const isWireguardPhase =
    wireguardStages.has(setup.stage) &&
    !streamingPrepStages.has(setup.stage) &&
    !pinStages.has(setup.stage);
  const canShowWireguardConfig =
    configMatchesActiveInstance && setup.wireguardConfig.trim().length > 0;
  const isStreamingPrepPhase = streamingPrepStages.has(setup.stage);
  const stageShowsPinSubmission = pinStages.has(setup.stage);
  const showPairingHandoff = stageShowsPinSubmission;
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
  const pairingSession = activeMoonlightPairing;
  const installLinkClass =
    "inline-flex items-center justify-center border border-[#61f7ff] bg-[#1b2f4d] px-4 py-2 font-display text-[11px] uppercase tracking-[0.12em] text-[#7cf8ff] transition duration-100 hover:bg-[#22466e] hover:text-white";

  const instructions = useMemo(
    () => [
      "Install GotaTun on this computer if it is not already available.",
      "Click Start Managed Tunnel below.",
      "Approve elevation if your operating system prompts for it.",
      "Let Noland verify tunnel connectivity automatically before continuing.",
    ],
    [],
  );

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

  async function copyPairingPin() {
    if (!pairingSession) {
      return;
    }
    try {
      await navigator.clipboard.writeText(pairingSession.pin);
      setPairingCopyState("copied");
    } catch {
      setPairingCopyState("failed");
    }
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-[#02040bdd] p-4">
      <div className="glass-panel pixel-frame crt-surface w-full max-w-3xl p-6">
        <button
          onClick={() => window.history.back()}
          className="absolute right-4 top-4 text-[#b9caf0] transition hover:text-white"
          aria-label="Close modal"
        >
          <SpriteIcon icon="close" />
        </button>
        <div className="mb-4 flex items-center justify-between gap-3 border-b border-[#3e4270] pb-2">
          <h3
            className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base"
            data-text={
              isWireguardPhase
                ? "Managed Tunnel Setup"
                : "Moonlight & Sunshine Setup"
            }
          >
            {isWireguardPhase
              ? "Managed Tunnel Setup"
              : "Moonlight & Sunshine Setup"}
          </h3>
          <AIPromptHelper
            topic={
              isWireguardPhase
                ? "Managed Tunnel Setup"
                : "Moonlight & Sunshine Pair Setup"
            }
            promptText={
              isWireguardPhase
                ? APP_PROMPTS.wireguardModalInfo
                : APP_PROMPTS.playButtonSection
            }
            variant="both"
          />
        </div>

        {isWireguardPhase ? (
          <>
            <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
              Noland is ready to bring up your managed local tunnel. Start the
              GotaTun-backed tunnel flow below and the app will verify
              connectivity before moving on to Moonlight setup.
            </p>
            <div className="mt-4 border border-[#3d426f] bg-[#10152f] p-4 text-[1.05rem] text-[#cfe7ff]">
              <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-cyan">
                What to install first
              </h4>
              <p className="mt-2">
                Install GotaTun on this computer. Noland uses it as the local
                WireGuard-compatible userspace tunnel backend and will keep using
                the generated config automatically.
              </p>
              <div className="mt-3 flex flex-wrap gap-2">
                <a
                  href={WIREGUARD_DOWNLOAD_URL}
                  target="_blank"
                  rel="noreferrer"
                  className={installLinkClass}
                >
                  Download GotaTun
                </a>
                <a
                  href={MOONLIGHT_DOWNLOAD_URL}
                  target="_blank"
                  rel="noreferrer"
                  className={installLinkClass}
                >
                  Download Moonlight
                </a>
              </div>
            </div>

            <ol className="mt-4 list-decimal space-y-2 pl-5 text-[1.08rem] leading-snug text-[#cfe7ff]">
              {instructions.map((instruction) => (
                <li key={instruction}>{instruction}</li>
              ))}
            </ol>

            {!configMatchesActiveInstance && (
              <p className="mt-4 text-[1rem] text-[#9ab0cc]">
                Loading the current tunnel config for this instance. If this
                does not update, click Start Managed Tunnel.
              </p>
            )}

            {canShowWireguardConfig && (
              <div className="mt-4">
                <div className="mb-2 flex items-center justify-between">
                  <span className="font-display text-[10px] uppercase tracking-[0.12em] text-[#b9caf0]">
                    Managed Tunnel Configuration
                  </span>
                  <Button variant="primary" onClick={() => void copyConfig()}>
                    <SpriteIcon icon="copy" />
                    <span className="ml-1">Copy to Clipboard</span>
                  </Button>
                </div>
                <textarea
                  readOnly
                  value={setup.wireguardConfig}
                  className="min-h-52 w-full border border-[#3d426f] bg-[#10152f] p-3 font-mono text-[0.9rem] text-[#d9efff]"
                />
              </div>
            )}
            <div className="mt-4 flex flex-wrap gap-2">
              <Button
                variant="secondary"
                onClick={() => void onDownloadWireguardConfig()}
                disabled={busy}
              >
                Download .conf
              </Button>
              <Button
                onClick={() => void onSetupWireguardAppHandoff()}
                disabled={busy}
              >
                Start Managed Tunnel
              </Button>
              <Button
                variant="ghost"
                onClick={() => void onSetupMoonlightSunshine()}
                disabled={busy}
              >
                Continue to Moonlight Setup
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
                  <span className="text-neon-cyan">{moonlightHost}</span>.
                  Pairing handoff unlocks when this preparation is done.
                </p>
                <div className="mt-4 border border-[#3d426f] bg-[#10152f] p-4 text-[1.02rem] text-[#cfe7ff]">
                  <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-cyan">
                    Moonlight setup checklist
                  </h4>
                  <ol className="mt-3 list-decimal space-y-2 pl-5 leading-snug">
                    <li>
                      Install Moonlight on this computer if you have not already.
                    </li>
                    <li>
                      Open Moonlight and add the PC at{" "}
                      <span className="text-neon-cyan">{moonlightHost}</span>.
                    </li>
                    <li>
                      When the pairing handoff unlocks, let Noland generate the
                      pairing PIN automatically here.
                    </li>
                  </ol>
                  <div className="mt-3 flex flex-wrap gap-2">
                    <a
                      href={MOONLIGHT_DOWNLOAD_URL}
                      target="_blank"
                      rel="noreferrer"
                      className={installLinkClass}
                    >
                      Download Moonlight
                    </a>
                  </div>
                </div>
              </>
            ) : (
              <>
                <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
                  Sunshine and Moonlight are ready. Use the pairing handoff
                  below and Noland will generate and submit the pairing PIN for
                  you.
                </p>
                <div className="mt-4 border border-[#3d426f] bg-[#10152f] p-4 text-[1.02rem] text-[#cfe7ff]">
                  <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-cyan">
                    Moonlight Status
                  </h4>
                  <p className="mt-2">
                    {moonlightChecked
                      ? setup.moonlightInstalled
                        ? "Moonlight was detected on this machine. Use the pairing handoff below."
                        : "Moonlight was not detected on this machine. You can still use the pairing handoff below, or install Moonlight locally if you want the fallback client available."
                      : "Moonlight setup is still in progress."}
                  </p>
                </div>
              </>
            )}

            {showPairingHandoff && (
              <div className="mt-5 border border-[#3d426f] bg-[#10152f] p-4">
                <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
                  Pairing Handoff
                </h4>
                <ol className="mt-3 list-decimal space-y-2 pl-5 text-[1.05rem] leading-snug text-[#cfe7ff]">
                  <li>
                    Make sure the instance is reachable at{" "}
                    <span className="text-neon-cyan">{moonlightHost}</span>.
                  </li>
                  <li>
                    Let Noland generate the Sunshine pairing PIN automatically.
                  </li>
                  <li>
                    Complete the pairing handoff and wait for Sunshine pairing
                    to finish.
                  </li>
                </ol>

                {pairingSession ? (
                  <div className="mt-4 border border-[#4f6a4e] bg-[#152316] p-3 text-[1rem] text-[#e6ffd7]">
                    <h5 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
                      Generated Pairing PIN
                    </h5>
                    <p className="mt-2 font-display text-lg text-white">
                      {pairingSession.pin}
                    </p>
                    <p className="mt-1 text-[0.95rem] text-[#c9efb7]">
                      Expires in about {pairingSession.expiresInSeconds} seconds.
                    </p>
                    <div className="mt-3 flex flex-wrap gap-2">
                      <Button
                        variant="secondary"
                        onClick={() => void copyPairingPin()}
                      >
                        <SpriteIcon icon="copy" />
                        <span className="ml-1">Copy PIN</span>
                      </Button>
                      <Button
                        onClick={() =>
                          void onCompleteMoonlightPairingHandoff(
                            pairingSession.pin,
                          )
                        }
                        disabled={busy}
                      >
                        Complete Pairing
                      </Button>
                    </div>
                    {pairingCopyState === "copied" && (
                      <p className="mt-2 text-[1rem] text-neon-lime">
                        PIN copied to clipboard.
                      </p>
                    )}
                    {pairingCopyState === "failed" && (
                      <p className="mt-2 text-[1rem] text-[#ffb2bf]">
                        Copy failed. Use the PIN shown above.
                      </p>
                    )}
                  </div>
                ) : (
                  <div className="mt-4 flex flex-wrap gap-2">
                    <Button
                      onClick={() => void onPrepareMoonlightPairingHandoff()}
                      disabled={busy}
                    >
                      Generate Pairing PIN
                    </Button>
                  </div>
                )}

                {setup.lastError?.retryable && !pinRetryError && (
                  <div className="mt-3 flex flex-wrap gap-2">
                    <Button
                      variant="ghost"
                      onClick={() => void onRetrySetupStage(setup.lastError!.stage)}
                      disabled={busy}
                    >
                      Retry Current Step
                    </Button>
                  </div>
                )}
              </div>
            )}

            {setup.setupComplete && (
              <div className="mt-4 border border-neon-lime bg-[#1f3223] p-4 text-[1.08rem] text-[#d9ffca]">
                <p>
                  Setup complete. Your secure streaming connection is ready, and
                  you can generate a fresh pairing handoff again at any time.
                </p>
                <div className="mt-3 border border-[#4f6a4e] bg-[#152316] p-3 text-[1rem] text-[#e6ffd7]">
                  <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
                    Sunshine Login
                  </h4>
                  <p className="mt-2 break-all">
                    URL: <span className="text-white">{sunshineUrl}</span>
                  </p>
                  <p className="break-all">
                    Username:{" "}
                    <span className="text-white">
                      {sunshineUsername || "(empty)"}
                    </span>
                  </p>
                  <p className="break-all">
                    Password:{" "}
                    <span className="text-white">
                      {sunshinePassword || "(empty)"}
                    </span>
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
              <p className="mt-2 whitespace-pre-wrap break-words text-[#ffbdc7]">
                {setup.lastError.details}
              </p>
            )}
            {(setup.lastError.code.includes("sunshine") ||
              setup.lastError.stage === "sunshine_verifying" ||
              setup.lastError.stage ===
                "sunshine_credentials_configuring") && (
              <div className="mt-3 border border-[#7a3f52] bg-[#341723] p-3 text-[1rem] text-[#ffd9df]">
                <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-[#ffc3cf]">
                  Manual Sunshine Login
                </h4>
                <p className="mt-2 break-all">
                  URL: <span className="text-white">{sunshineUrl}</span>
                </p>
                <p className="break-all">
                  Username:{" "}
                  <span className="text-white">
                    {sunshineUsername || "(empty)"}
                  </span>
                </p>
                <p className="break-all">
                  Password:{" "}
                  <span className="text-white">
                    {sunshinePassword || "(empty)"}
                  </span>
                </p>
                <p className="mt-2 text-[#ffbdc7]">
                  Open the Sunshine UI manually, log in with these credentials,
                  confirm the web UI loads, then come back here and retry.
                </p>
              </div>
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
                Generate a fresh pairing PIN handoff and try again here.
              </p>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
