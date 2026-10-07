import { translate, translateSource } from "../../lib/i18n";
import { useMemo } from "react";
import { Button } from "../../components/ui/Button";

import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import { APP_PROMPTS } from "../../prompts/appPrompts";

import type {
  OrchestrationState,
  PersistedAppState,
  SetupStage,
  MoonlightPairingSessionResponse,
} from "../../lib/types";

interface Props {
  open: boolean;
  onClose: () => void;
  appState: PersistedAppState;
  busy: boolean;
  onSetupWireguardAppHandoff: () => Promise<unknown>;
  onSetupMoonlightSunshine: () => Promise<unknown>;
  activeMoonlightPairing: MoonlightPairingSessionResponse | null;
  onPrepareMoonlightPairingHandoff: () => Promise<MoonlightPairingSessionResponse | null>;
  onCompleteMoonlightPairingHandoff: (sessionId: string) => Promise<unknown>;
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
  onClose,
  appState,
  busy,
  onSetupWireguardAppHandoff,
  onSetupMoonlightSunshine,
  activeMoonlightPairing,
  onPrepareMoonlightPairingHandoff,
  onCompleteMoonlightPairingHandoff,
  onRetrySetupStage,
}: Props) {


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

  const isStreamingPrepPhase = streamingPrepStages.has(setup.stage);
  const stageShowsPinSubmission = pinStages.has(setup.stage);
  const showPairingHandoff = stageShowsPinSubmission;
  const orchestrationShowsPinSubmission = new Set<OrchestrationState>([
    "MoonlightPairingStarted",
    "MoonlightPinReceived",
    "SunshinePinSubmitting",
    "MoonlightSunshinePaired",
  ]).has(appState.orchestrationState);
  const streamingReady =
    stageShowsPinSubmission || orchestrationShowsPinSubmission;
  const pinRetryError =
    setup.lastError?.stage === "moonlight_pin_received" ||
    setup.lastError?.stage === "sunshine_pin_submitting";
  const pairingSession = activeMoonlightPairing;
  const instructions = useMemo(
    () => [
      "Click Setup Tunnel below.",
      "Approve elevation if your operating system prompts for it.",
      "Let Noland verify tunnel connectivity automatically before continuing.",
    ],
    [],
  );

  if (!open) {
    return null;
  }


  return (
    <ModalFrame
      panelClassName="glass-panel pixel-frame crt-surface max-w-3xl"
      zIndexClassName="z-40"
    >
        <div className="shrink-0 flex items-center justify-between gap-3 border-b border-[#3e4270] px-6 py-4">
          <h3
            className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base"
            data-text={
              isWireguardPhase
                ? translate("generated.4b4703dd718bd7bd")
                : translate("generated.5d5de3cdf5086b8a")
            }
          >
            {isWireguardPhase
              ? translate("generated.4b4703dd718bd7bd")
              : translate("generated.5d5de3cdf5086b8a")}
          </h3>
          <div className="flex items-center gap-2">
            <AIPromptHelper
              topic={
                isWireguardPhase
                  ? translate("generated.4b4703dd718bd7bd")
                  : translate("generated.dfeadba6578179ba")
              }
              promptText={
                isWireguardPhase
                  ? APP_PROMPTS.wireguardModalInfo
                  : APP_PROMPTS.playButtonSection
              }
              variant="both"
            />
            <Button
              type="button"
              variant="ghost"
              className="px-3 py-1 text-[16px]"
              onClick={onClose}
              aria-label={translate("generated.11972a6796cce45b")}
            >
              {translate("generated.8db71ed28b0f2f14")}
            </Button>
          </div>
        </div>

        <ModalBody className="px-6 pb-6 pt-2">
        {isWireguardPhase ? (
          <>
            <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
              {translate("generated.759dfe1816ba46f1")}
            </p>
            <div className="mt-4 border border-[#3d426f] bg-[#10152f] p-4 text-[1.05rem] text-[#cfe7ff]">
              <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-cyan">
                {translate("generated.3c281f412b025b73")}
              </h4>
              <p className="mt-2">
                {translate("generated.f9be21f6b0244e21")}
              </p>
            </div>

            <ol className="mt-4 list-decimal space-y-2 pl-5 text-[1.08rem] leading-snug text-[#cfe7ff]">
              {instructions.map((instruction) => (
                <li key={instruction}>{translateSource(instruction)}</li>
              ))}
            </ol>

            {!configMatchesActiveInstance && (
              <p className="mt-4 text-[1rem] text-[#9ab0cc]">
                {translate("generated.366e6c7c5e0511f8")}
              </p>
            )}


            <div className="mt-4 flex flex-wrap gap-2">
              {setup.stage !== "wireguard_connected" ? (
                <Button
                  onClick={() => void onSetupWireguardAppHandoff()}
                  disabled={busy}
                >
                  {translate("generated.8c8f4e6f0b52ecb8")}
                </Button>
              ) : (
                <Button
                  onClick={() => void onSetupMoonlightSunshine()}
                  disabled={busy}
                >
                  {translate("generated.31fbef162594de01")}
                </Button>
              )}
            </div>

          </>
        ) : (
          <>
            {isStreamingPrepPhase ? (
              <>
                <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
                  {translate("generated.0cc2285f33cd0c5b")}{" "}
                  <span className="text-neon-cyan">{moonlightHost}</span>{translate("generated.ec16bab82c73e1bf")}
                </p>
                <div className="mt-4 border border-[#3d426f] bg-[#10152f] p-4 text-[1.02rem] text-[#cfe7ff]">
                  <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-cyan">
                    {translate("generated.6fe9008926a3f8c6")}
                  </h4>
                  <ol className="mt-3 list-decimal space-y-2 pl-5 leading-snug">
                    <li>
                      {translate("generated.27df759d31ee7f92")}{" "}
                      <span className="text-neon-cyan">{moonlightHost}</span>.
                    </li>
                    <li>
                      {translate("generated.21927cf13f7ea636")}
                    </li>
                    <li>
                      {translate("generated.064f80dd1cd84efe")}
                    </li>
                  </ol>
                </div>
              </>
            ) : (
              <>
                <p className="mt-3 text-[1.15rem] leading-snug text-[#d9efff]">
                  {translate("generated.d4313d2c678c085e")}
                </p>
                <div className="mt-4 border border-[#3d426f] bg-[#10152f] p-4 text-[1.02rem] text-[#cfe7ff]">
                  <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-cyan">
                    {translate("generated.0f255fba16c5b0be")}
                  </h4>
                  <p className="mt-2">
                    {streamingReady
                      ? translate("generated.794c7a1426d31f52")
                      : translate("generated.e9e9d5359c79860d")}
                  </p>
                </div>
              </>
            )}

            {showPairingHandoff && (
              <div className="mt-5 border border-[#3d426f] bg-[#10152f] p-4">
                <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
                  {translate("generated.33c2d3a6044221ef")}
                </h4>
                <ol className="mt-3 list-decimal space-y-2 pl-5 text-[1.05rem] leading-snug text-[#cfe7ff]">
                  <li>
                    {translate("generated.505233f7c928081a")}{" "}
                    <span className="text-neon-cyan">{moonlightHost}</span>.
                  </li>
                  <li>
                    {translate("generated.4377d6d5260c5a93")}
                  </li>
                  <li>
                    {translate("generated.005afbb26632cc4f")}
                  </li>
                </ol>

                {pairingSession ? (
                  <div className="mt-4 border border-[#4f6a4e] bg-[#152316] p-3 text-[1rem] text-[#e6ffd7]">
                    <h5 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
                      {translate("generated.2077e05030a131fc")}
                    </h5>
                    <p className="mt-2 font-display text-lg text-white">
                      {pairingSession.pin}
                    </p>
                    <p className="mt-1 text-[0.95rem] text-[#c9efb7]">
                      {translate("generated.085db67333fab303")} {pairingSession.expiresInSeconds} {translate("generated.5d992667f7075c53")}
                    </p>
                    <div className="mt-3 flex flex-wrap gap-2">
                      <Button
                        onClick={() =>
                          void onCompleteMoonlightPairingHandoff(
                            pairingSession.sessionId,
                          )
                        }
                        disabled={busy}
                      >
                        {translate("generated.989da04b0aaaa57f")}
                      </Button>
                    </div>

                  </div>
                ) : (
                  <div className="mt-4 flex flex-wrap gap-2">
                    <Button
                      onClick={() => void onPrepareMoonlightPairingHandoff()}
                      disabled={busy}
                    >
                      {translate("generated.f72d57066e49db03")}
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
                      {translate("generated.5501dfe50b30ae90")}
                    </Button>
                  </div>
                )}
              </div>
            )}

            {setup.setupComplete && (
              <div className="mt-4 border border-neon-lime bg-[#1f3223] p-4 text-[1.08rem] text-[#d9ffca]">
                <p>
                  {translate("generated.9dd67acfb78a70d7")}
                </p>
                <div className="mt-3 border border-[#4f6a4e] bg-[#152316] p-3 text-[1rem] text-[#e6ffd7]">
                  <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">
                    {translate("generated.1b01e1514d0308a8")}
                  </h4>
                  <p className="mt-2 break-all">
                    {translate("generated.734fd77b36107c77")} <span className="text-white">{sunshineUrl}</span>
                  </p>
                  <p className="break-all">
                    {translate("generated.3806d61c34063f85")}{" "}
                    <span className="text-white">
                      {sunshineUsername || "(empty)"}
                    </span>
                  </p>
                  <p className="break-all">
                    {translate("generated.569b2482a687d9aa")}{" "}
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
            <p className="mt-2">{translateSource(setup.lastError.message)}</p>
            {setup.lastError.details && (
              <p className="mt-2 whitespace-pre-wrap break-words text-[#ffbdc7]">
                {translateSource(setup.lastError.details)}
              </p>
            )}
            {(setup.lastError.code.includes("sunshine") ||
              setup.lastError.stage === "sunshine_verifying" ||
              setup.lastError.stage ===
                "sunshine_credentials_configuring") && (
              <div className="mt-3 border border-[#7a3f52] bg-[#341723] p-3 text-[1rem] text-[#ffd9df]">
                <h4 className="font-display text-[11px] uppercase tracking-[0.12em] text-[#ffc3cf]">
                  {translate("generated.c07464c44b0fe0ad")}
                </h4>
                <p className="mt-2 break-all">
                  {translate("generated.734fd77b36107c77")} <span className="text-white">{sunshineUrl}</span>
                </p>
                <p className="break-all">
                  {translate("generated.3806d61c34063f85")}{" "}
                  <span className="text-white">
                    {sunshineUsername || "(empty)"}
                  </span>
                </p>
                <p className="break-all">
                  {translate("generated.569b2482a687d9aa")}{" "}
                  <span className="text-white">
                    {sunshinePassword || "(empty)"}
                  </span>
                </p>
                <p className="mt-2 text-[#ffbdc7]">
                  {translate("generated.59930608e02d0d2b")}
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
                  {translate("generated.5501dfe50b30ae90")}
                </Button>
              </div>
            )}
            {pinRetryError && (
              <p className="mt-3 text-[#ffbdc7]">
                {translate("generated.48d1e6eeed1a147e")}
              </p>
            )}
          </div>
        )}
        </ModalBody>
    </ModalFrame>
  );
}
