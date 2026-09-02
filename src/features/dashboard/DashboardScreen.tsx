import { useMemo, useState } from "react";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { APP_PROMPTS } from "../../prompts/appPrompts";
import { useNavigate } from "react-router-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ArcadeSoundToggle } from "../../components/ui/ArcadeSoundToggle";
import type { BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { HudBar } from "../../components/ui/HudBar";
import { MicControls } from "../../components/ui/MicControls";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import { SpriteIcon } from "../../components/ui/SpriteIcon";
import { StatusPill } from "../../components/ui/StatusPill";
import {
  VAST_BILLING_URL,
  VAST_API_KEY_URL,
} from "../../lib/constants";
import type {
  BackupPerformanceMode,
  OfferCandidate,
  OfferCountryAvailability,
  PersistedAppState,
  RentedInstanceSummary,
  ServerPreferences,
  SharedStorageObjectEntry,
  EmbeddedMoonlightInstanceStatus,
  VastBrowserBillingAction,
  VastWalletSummary,
  LaunchLibraryResponse,
  LaunchSoftwareJob,
  SoftwareArtworkResult,
} from "../../lib/types";
import { ServerPickerModal } from "../servers/ServerPickerModal";
import { SharedStorageExportModal } from "../shared-storage-manager/SharedStorageExportModal";
import { InstanceCardActions } from "../shared-storage-manager/InstanceCardActions";
import { InstanceDisplayModal } from "./InstanceDisplayModal";
import { InstanceMoonlightOptionsModal } from "./InstanceMoonlightOptionsModal";
import { SharedStorageSyncModal } from "../shared-storage-manager/SharedStorageSyncModal";
import { LaunchLibraryModal } from "../launch-library/LaunchLibraryModal";

import { TutorialModal } from "../onboarding/TutorialModal";
import { tutorialSteps } from "../onboarding/tutorialSteps";

interface Props {
  appState: PersistedAppState;
  offers: OfferCandidate[];
  rentedInstances: RentedInstanceSummary[];
  embeddedMoonlightStatus: EmbeddedMoonlightInstanceStatus | null;
  vastWalletSummary: VastWalletSummary | null;
  searchingOffers: boolean;
  offersPage: number;
  offersHasNextPage: boolean;
  busy: boolean;
  instanceActionRunning: boolean;
  blockingAction: BlockingActionState | null;
  onSearchOffers: (page?: number) => Promise<void>;
  onLoadAvailableOfferCountries: () => Promise<
    OfferCountryAvailability[] | null
  >;
  onNextOffersPage: () => Promise<void>;
  onPreviousOffersPage: () => Promise<void>;
  onManualLocationSave: (payload: {
    city: string;
    region: string;
    country: string;
    latitude: number;
    longitude: number;
  }) => Promise<void>;
  onLoadRentedInstances: () => Promise<void>;
  onRefreshVastWalletSummary: () => Promise<VastWalletSummary | null>;
  onResumeProvisioningExisting: (instanceId: number) => Promise<string | null>;
  onStartPlayExisting: (instanceId: number) => Promise<string | null>;
  launchLibrary: LaunchLibraryResponse | null;
  launchLibraryLoading: boolean;
  launchSoftwareJob: LaunchSoftwareJob | null;
  launchingSoftwareAppId: string | null;
  softwareArtwork: Record<string, SoftwareArtworkResult>;
  softwareArtworkLoading: Record<string, boolean>;
  onLoadInstanceLaunchLibrary: (
    instanceId: number,
  ) => Promise<LaunchLibraryResponse | null>;
  onLaunchInstanceSoftware: (
    instanceId: number,
    appId: string,
  ) => Promise<LaunchSoftwareJob | null>;
  onPollLaunchSoftwareJob: (
    jobId: string,
  ) => Promise<LaunchSoftwareJob | null>;
  onLoadSoftwareArtwork: (
    name: string,
  ) => Promise<SoftwareArtworkResult | null>;
  onClearLaunchLibrary: () => void;
  onSelectOffer: (offerId: number, storageGb: number) => Promise<boolean>;
  onStartPlay: () => Promise<void>;
  onSaveServerPreferences: (
    payload: Partial<ServerPreferences>,
  ) => Promise<void>;
  onSetEmbeddedMoonlightPipelineEnabled: (
    instanceId: number,
    enabled: boolean,
  ) => Promise<void>;
  onLoadEmbeddedMoonlightStatus: (
    instanceId: number,
  ) => Promise<EmbeddedMoonlightInstanceStatus | null>;
  onRebootInstanceServices: (instanceId: number) => Promise<string | null>;
  onDestroyInstance: (instanceId: number) => Promise<void>;
  onSaveInstanceStorageSelected: (
    instanceId: number,
    selectedPaths: string[],
    performanceMode: BackupPerformanceMode,
  ) => Promise<string | null>;
  onSyncInstanceStorage: (
    instanceId: number,
    selectedPaths: string[],
  ) => Promise<string | null>;
  onListSyncableStorageObjects: (
    instanceId: number,
  ) => Promise<SharedStorageObjectEntry[] | null>;
  onListExportableStorageObjects: (
    instanceId: number,
  ) => Promise<SharedStorageObjectEntry[] | null>;
  onRefreshIndexing?: (instanceId: number) => Promise<void>;
}

export function DashboardScreen({
  appState,
  offers,
  rentedInstances,
  embeddedMoonlightStatus,
  vastWalletSummary,
  searchingOffers,
  offersPage,
  offersHasNextPage,
  busy,
  instanceActionRunning,
  blockingAction,
  onSearchOffers,
  onLoadAvailableOfferCountries,
  onNextOffersPage,
  onPreviousOffersPage,
  onManualLocationSave,
  onLoadRentedInstances,
  onRefreshVastWalletSummary,
  onResumeProvisioningExisting,
  onStartPlayExisting,
  launchLibrary,
  launchLibraryLoading,
  launchSoftwareJob,
  launchingSoftwareAppId,
  softwareArtwork,
  softwareArtworkLoading,
  onLoadInstanceLaunchLibrary,
  onLaunchInstanceSoftware,
  onPollLaunchSoftwareJob,
  onLoadSoftwareArtwork,
  onClearLaunchLibrary,
  onSelectOffer,
  onStartPlay,
  onSaveServerPreferences,
  onSetEmbeddedMoonlightPipelineEnabled,
  onLoadEmbeddedMoonlightStatus,
  onRebootInstanceServices,
  onDestroyInstance,
  onSaveInstanceStorageSelected,
  onSyncInstanceStorage,
  onListSyncableStorageObjects,
  onListExportableStorageObjects,
  onRefreshIndexing,
}: Props) {
  const [pickerOpen, setPickerOpen] = useState(false);
  const [availableOfferCountries, setAvailableOfferCountries] = useState<
    OfferCountryAvailability[]
  >([]);
  const [syncInstanceId, setSyncInstanceId] = useState<number | null>(null);
  const [exportInstanceId, setExportInstanceId] = useState<number | null>(null);
  const [displayInstanceId, setDisplayInstanceId] = useState<number | null>(null);
  const [moonlightOptionsInstanceId, setMoonlightOptionsInstanceId] =
    useState<number | null>(null);
  const [launchLibraryInstanceId, setLaunchLibraryInstanceId] = useState<number | null>(null);
  const [walletModalOpen, setWalletModalOpen] = useState(false);
  const [tutorialOpen, setTutorialOpen] = useState(false);
  const [tutorialStep, setTutorialStep] = useState(0);
  const [connectionInfoModalType, setConnectionInfoModalType] = useState<
    "wireguard" | null
  >(null);
  const navigate = useNavigate();
  const blockingLabel = blockingAction?.label ?? null;
  const blockingDetail = blockingAction?.detail ?? null;

  const openServerPicker = async () => {
    setPickerOpen(true);
    const countries = await onLoadAvailableOfferCountries();
    if (countries && countries.length > 0) {
      setAvailableOfferCountries(countries);
    }
  };
  const showDashboardGuidance = !appState.hasCompletedGuidedSetup;
  const displayInstance = rentedInstances.find(
    (instance) => instance.instanceId === displayInstanceId,
  );
  const moonlightOptionsInstance = rentedInstances.find(
    (instance) => instance.instanceId === moonlightOptionsInstanceId,
  );
  const launchLibraryInstance = rentedInstances.find(
    (instance) => instance.instanceId === launchLibraryInstanceId,
  );

  const hasProvisioningToResume = useMemo(() => {
    const hasActiveProvisioningInstance =
      appState.postWireguardSetup.currentInstanceId !== null ||
      appState.instance.instanceId !== null;
    if (!hasActiveProvisioningInstance) {
      return false;
    }

    if (appState.postWireguardSetup.setupComplete) {
      return false;
    }

    if (
      appState.postWireguardSetup.stage !== "pre_wireguard_existing_flow" &&
      appState.postWireguardSetup.stage !== "setup_complete"
    ) {
      return true;
    }

    return (
      appState.orchestrationState !== "Idle" &&
      appState.orchestrationState !== "Ready"
    );
  }, [appState]);

  const walletAmountLabel = vastWalletSummary?.displayAmount || "--";

  async function openExternalUrl(url: string) {
    try {
      await openUrl(url);
    } catch {
      window.open(url, "_blank", "noopener,noreferrer");
    }
  }

  async function handlePlay() {
    if (hasProvisioningToResume) {
      navigate("/provisioning");
      return;
    }

    await onStartPlay();
    navigate("/provisioning");
  }

  async function handleResumeProvisioning(instanceId: number) {
    const mode = await onResumeProvisioningExisting(instanceId);
    if (mode === "provisioning") {
      navigate("/provisioning");
    }
  }


  async function handlePlayEmbedded(instanceId: number) {
    await onSetEmbeddedMoonlightPipelineEnabled(instanceId, true);
    await onLoadEmbeddedMoonlightStatus(instanceId);
    const mode = await onStartPlayExisting(instanceId);
    if (mode === "provisioning") {
      navigate("/provisioning");
    }
  }

  function handleOpenLaunchLibrary(instanceId: number) {
    onClearLaunchLibrary();
    setLaunchLibraryInstanceId(instanceId);
  }

  function handleCloseLaunchLibrary() {
    setLaunchLibraryInstanceId(null);
    onClearLaunchLibrary();
  }

  function handleDisplay(instanceId: number) {
    setDisplayInstanceId(instanceId);
  }

  async function handleReboot(instanceId: number) {
    await onRebootInstanceServices(instanceId);
  }

  async function handleDestroy(instanceId: number) {
    await onDestroyInstance(instanceId);
    await onLoadRentedInstances();
  }

  async function handleSaveStorage(instanceId: number) {
    setExportInstanceId(instanceId);
  }

  async function handleSyncStorage(instanceId: number) {
    setSyncInstanceId(instanceId);
  }

  async function handleSyncSelection(selectedPaths: string[]) {
    if (syncInstanceId === null) {
      return;
    }

    await onSyncInstanceStorage(syncInstanceId, selectedPaths);
    setSyncInstanceId(null);
  }

  async function handleExportSelection(
    selectedPaths: string[],
    performanceMode: BackupPerformanceMode,
  ) {
    if (exportInstanceId === null) {
      return;
    }

    await onSaveInstanceStorageSelected(
      exportInstanceId,
      selectedPaths,
      performanceMode,
    );
    setExportInstanceId(null);
  }

  async function handleOpenWalletBilling(action?: VastBrowserBillingAction) {
    if (action === "open-auto-topup") {
      await openExternalUrl(VAST_BILLING_URL);
      return;
    }
    await openExternalUrl(VAST_BILLING_URL);
  }

  function openTutorial() {
    setTutorialStep(0);
    setTutorialOpen(true);
  }

  function goToPreviousTutorialStep() {
    setTutorialStep((current) => Math.max(0, current - 1));
  }

  function goToNextTutorialStep() {
    if (tutorialStep === tutorialSteps.length - 1) {
      setTutorialOpen(false);
      return;
    }

    setTutorialStep((current) => current + 1);
  }

  return (
    <main className="crt-surface min-h-dvh bg-hero-glow px-4 pb-8 pt-6 md:px-8">
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-6">
        <header className="flex flex-wrap items-center justify-between gap-4">
          <div>
            <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">
              Noland Connect
            </p>
            <h1
              className="pixel-heading glitch-title font-display text-lg text-white md:text-2xl"
              data-text="Arcade Control Deck"
            >
              Arcade Control Deck
            </h1>
          </div>

          <div className="flex items-center gap-2">
            <Button variant="ghost" onClick={() => setWalletModalOpen(true)}>
              Wallet {walletAmountLabel}
            </Button>
            <Button variant="ghost" onClick={openTutorial}>
              <SpriteIcon icon="help" />
              <span className="ml-1">Help</span>
            </Button>
            <Button variant="ghost" onClick={() => navigate("/settings")}>
              Settings
            </Button>
            <Button variant="secondary" onClick={openServerPicker}>
              Select Server
            </Button>
            <ArcadeSoundToggle />
          </div>
        </header>

        {showDashboardGuidance ? (
          <section className="grid gap-4 md:grid-cols-3">
            <Card
              className="pixel-frame min-h-40 flex flex-col justify-center p-4"
            >
              <div className="flex items-center justify-between">
                <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">
                  Panel 1
                </p>
                <div className="flex items-center gap-2">
                  <AIPromptHelper
                    topic="Embedded Streaming"
                    promptText={APP_PROMPTS.moonlightCard}
                    variant="icon"
                  />
                  <SpriteIcon icon="moonlight" />
                </div>
              </div>
              <h2 className="mt-3 font-display text-lg text-neon-cyan md:text-xl">
                Managed Streaming
              </h2>
              <p className="mt-2 max-w-md text-[1.32rem] leading-[1.25] text-[#bfd3ee]">
                Noland now handles the streaming and connection flow inside the app. Just complete Vast.ai billing and API key setup, then continue from the dashboard.
              </p>
            </Card>

            <div className="flex flex-col gap-4">
              <Card
                interactive
                onClick={() => setConnectionInfoModalType("wireguard")}
                className="pixel-frame flex-1 flex flex-col justify-between p-4"
              >
                <div>
                  <div className="flex items-center justify-between">
                    <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">
                      Connection Type
                    </p>
                    <div className="flex items-center gap-2">
                      <AIPromptHelper
                        topic="Managed Tunnel Connection Option"
                        promptText={APP_PROMPTS.wireguardCard}
                        variant="icon"
                      />
                      <SpriteIcon icon="settings" />
                    </div>
                  </div>
                  <h2 className="mt-2 font-display text-base text-neon-cyan md:text-lg">
                    Managed Secure Connection
                  </h2>
                  <p className="mt-1 text-[1.2rem] leading-[1.1] text-[#bfd3ee]">
                    Noland activates and verifies the secure connection flow for you inside the app before moving on to streaming setup.
                  </p>
                </div>
              </Card>
            </div>

            <Card
              interactive
              onClick={openServerPicker}
              className="pixel-frame min-h-40 flex flex-col justify-center p-4"
            >
              <div className="flex items-center justify-between">
                <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">
                  Panel 3
                </p>
                <div className="flex items-center gap-2">
                  <AIPromptHelper
                    topic="Set Server Selection Offering"
                    promptText={APP_PROMPTS.setServerCard}
                    variant="icon"
                  />
                  <SpriteIcon icon="server" />
                </div>
              </div>
              <h2 className="mt-3 font-display text-lg text-neon-cyan md:text-xl">
                Set Server
              </h2>
              <p className="mt-2 text-[1.32rem] leading-[1.25] text-[#bfd3ee]">
                Discover nearby high-performance GPU server offers filtered by
                price, reliability, and network distance. Adjust template hash
                and storage allocation prior to launching your machine.
              </p>
            </Card>
          </section>
        ) : null}

        <TutorialModal
          open={tutorialOpen}
          stepIndex={tutorialStep}
          steps={tutorialSteps}
          closable
          onBack={goToPreviousTutorialStep}
          onNext={goToNextTutorialStep}
          onClose={() => setTutorialOpen(false)}
        />

        <section>
          <Card className="pixel-frame">
            <div className="mb-3 flex items-center justify-between">
              <div className="flex items-center gap-2">
                <h3 className="font-display text-sm uppercase tracking-[0.12em] text-white">
                  Rented Servers
                </h3>
                <AIPromptHelper
                  topic="Managing Rented Servers"
                  promptText={APP_PROMPTS.rentedServersSection}
                  variant="icon"
                />
                {blockingAction &&
                  blockingAction.key.startsWith("instance.") && (
                    <p
                      className="mt-1 text-[1.1rem] text-[#9ec4df]"
                      aria-live="polite"
                    >
                      {blockingLabel}
                      {blockingDetail ? `: ${blockingDetail}` : "..."}
                    </p>
                  )}
              </div>
              <Button
                variant="secondary"
                onClick={onLoadRentedInstances}
                disabled={busy}
                loading={busy && !blockingAction}
                loadingText="Refreshing..."
              >
                Refresh Rented
              </Button>
            </div>

            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
              {rentedInstances.map((instance) => (
                <Card
                  key={instance.instanceId}
                  className="border-2 border-[#3a4068]"
                >
                  <div className="flex items-center justify-between gap-2">
                    <h4 className="font-display text-[11px] text-white">
                      {instance.label}
                    </h4>
                    <div className="flex items-center gap-2">
                      <StatusPill
                        state={
                          instance.status.toLowerCase().includes("run")
                            ? "Ready"
                            : "WaitingForInstance"
                        }
                      />
                      <Button
                        variant="ghost"
                        aria-label={`Moonlight options for ${instance.label}`}
                        title="Moonlight stream options"
                        className="h-8 w-8 rounded border border-[#3a4068] p-0"
                        disabled={busy}
                        onClick={() =>
                          setMoonlightOptionsInstanceId(instance.instanceId)
                        }
                      >
                        <SpriteIcon icon="settings" className="h-5 w-5" />
                      </Button>
                    </div>
                  </div>
                  <div className="mt-2 grid grid-cols-2 gap-2 text-[1.15rem] text-[#bfd3ee]">
                    <p>ID: {instance.instanceId}</p>
                    <p>Status: {instance.status}</p>
                    <p>GPU: {instance.gpuName}</p>
                    <p>SSH: {instance.sshHost || "pending"}</p>
                  </div>
                  {instance.embeddedMoonlightPipelineEnabled && (
                    <div className="mt-2 space-y-2">
                      <div className="rounded border border-neon-cyan/30 bg-neon-cyan/10 px-2 py-1 text-[11px] uppercase tracking-wide text-neon-cyan">
                        Embedded Moonlight pipeline enabled
                      </div>
                      {(instance.embeddedMoonlightSessionState ||
                        instance.embeddedMoonlightLastRuntimeEvent ||
                        instance.embeddedMoonlightLastError ||
                        embeddedMoonlightStatus?.instanceId === instance.instanceId) && (
                        <div className="rounded border border-[#3a4068] bg-[#10152f]/60 px-2 py-2 text-[11px] text-[#bfd3ee]">
                          <p>
                            Session: {instance.embeddedMoonlightSessionState ?? embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? instance.embeddedMoonlightSessionState ?? embeddedMoonlightStatus?.sessionState
                              : "unknown"}
                          </p>
                          <p>
                            Paired: {instance.embeddedMoonlightPaired ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.paired
                              : null)
                              ? "yes"
                              : "no"}
                          </p>
                          <p>
                            Connected: {instance.embeddedMoonlightRuntimeConnected ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.runtimeConnected
                              : null)
                              ? "yes"
                              : "no"}
                          </p>
                          <p>
                            Renderer ready: {instance.embeddedMoonlightRendererReady ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.rendererReady
                              : null)
                              ? "yes"
                              : "no"}
                          </p>
                          <p>
                            Video active: {instance.embeddedMoonlightVideoSessionActive ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.videoSessionActive
                              : null)
                              ? "yes"
                              : "no"}
                          </p>
                          <p>
                            Video frames: {instance.embeddedMoonlightVideoFrameCount ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.videoFrameCount
                              : 0) ?? 0}
                          </p>
                          <p>
                            Rendered frames: {instance.embeddedMoonlightRendererSubmittedFrameCount ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.rendererSubmittedFrameCount
                              : 0) ?? 0}
                          </p>
                          <p>
                            Dropped frames: {instance.embeddedMoonlightRendererDroppedFrameCount ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.rendererDroppedFrameCount
                              : 0) ?? 0}
                          </p>
                          <p>
                            Audio samples: {instance.embeddedMoonlightAudioSampleCount ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.audioSampleCount
                              : 0) ?? 0}
                          </p>
                          {(instance.embeddedMoonlightLastRuntimeEvent ??
                            (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.lastRuntimeEvent
                              : null)) ? (
                            <p className="mt-1 text-[#8db7d8]">
                              {instance.embeddedMoonlightLastRuntimeEvent ??
                                (embeddedMoonlightStatus?.instanceId === instance.instanceId
                                  ? embeddedMoonlightStatus?.lastRuntimeEvent
                                  : null)}
                            </p>
                          ) : null}
                          {(instance.embeddedMoonlightLastError ??
                            (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.lastError
                              : null)) ? (
                            <p className="mt-1 text-[#ff8fb7]">
                              {instance.embeddedMoonlightLastError ??
                                (embeddedMoonlightStatus?.instanceId === instance.instanceId
                                  ? embeddedMoonlightStatus?.lastError
                                  : null)}
                            </p>
                          ) : null}
                        </div>
                      )}
                    </div>
                  )}
                  <div className="mt-3">
                    <InstanceCardActions
                      instance={instance}
                      busy={busy}
                      instanceActionRunning={instanceActionRunning}
                      blockingAction={blockingAction}
                      onProvisioning={handleResumeProvisioning}
                      onOpenLaunchLibrary={handleOpenLaunchLibrary}
                      onDisplay={handleDisplay}
                      onReboot={handleReboot}
                      onDestroy={handleDestroy}
                      onSaveStorage={handleSaveStorage}
                      onSyncStorage={handleSyncStorage}
                    />
                  </div>
                  {instance.status.toLowerCase().includes("run") && (
                    <div className="mt-3">
                      <MicControls instanceId={instance.instanceId} />
                    </div>
                  )}

                </Card>
              ))}

              <Card
                interactive
                onClick={openServerPicker}
                className="flex items-center justify-center border-2 border-dashed border-[#3a4068] hover:border-neon-cyan hover:bg-[#10152f]/30 transition-colors min-h-[14rem] bg-[#10152f]/10"
              >
                <div className="text-[9rem] text-[#bfd3ee] font-bold transition-transform hover:scale-110 select-none leading-none">
                  +
                </div>
              </Card>
            </div>
          </Card>
        </section>

        <section className="grid gap-4 lg:grid-cols-[1.5fr_1fr]">
          <Card className="pixel-frame min-h-44">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <h3 className="font-display text-sm uppercase tracking-[0.12em] text-white">
                  Selected Server
                </h3>
                <AIPromptHelper
                  topic="Selected Server Specifications"
                  promptText={APP_PROMPTS.selectedServerSection}
                  variant="icon"
                />
              </div>
              <StatusPill state={appState.orchestrationState} />
            </div>

            {appState.selectedOffer ? (
              <div className="mt-4 grid grid-cols-2 gap-3 text-[1.25rem] leading-[1.05] text-[#d9efff] md:grid-cols-4">
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    Host
                  </p>
                  <p>{appState.selectedOffer.hostLabel}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    Location
                  </p>
                  <p>{appState.selectedOffer.locationLabel}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    GPU
                  </p>
                  <p>{appState.selectedOffer.gpuName}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    Price/hour
                  </p>
                  <p>${appState.selectedOffer.hourlyPrice.toFixed(3)}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    Distance
                  </p>
                  <p>
                    {appState.selectedOffer.estimatedDistanceKm.toFixed(0)} km
                  </p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    Reliability
                  </p>
                  <p>
                    {(appState.selectedOffer.reliability * 100).toFixed(1)}%
                  </p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    Storage
                  </p>
                  <p>{appState.serverPreferences.storageGb} GB</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    Template
                  </p>
                  <p className="truncate">
                    {appState.serverPreferences.templateHash}
                  </p>
                </div>
              </div>
            ) : null}

            {appState.selectedOffer ? (
              <div className="mt-4 grid gap-2 md:grid-cols-3">
                <HudBar
                  label="Reliability"
                  value={appState.selectedOffer.reliability}
                  valueLabel={`${Math.round(appState.selectedOffer.reliability * 100)}%`}
                />
                <HudBar
                  label="VRAM"
                  value={appState.selectedOffer.gpuRamMb}
                  max={49152}
                  valueLabel={`${(appState.selectedOffer.gpuRamMb / 1024).toFixed(1)} GB`}
                />
                <HudBar
                  label="Distance"
                  value={Math.max(
                    0,
                    1000 - appState.selectedOffer.estimatedDistanceKm,
                  )}
                  max={1000}
                  valueLabel={`${appState.selectedOffer.estimatedDistanceKm.toFixed(0)} km`}
                />
              </div>
            ) : (
              <p className="mt-4 text-[1.35rem] leading-[1.05] text-[#bfd3ee]">
                No offer selected yet. Use Select Server to pick a machine.
              </p>
            )}
          </Card>

          <Card className="pixel-frame flex flex-col justify-between gap-4">
            <div>
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">
                    Action
                  </p>
                  <AIPromptHelper
                    topic="Provisioning and Play Execution"
                    promptText={APP_PROMPTS.playButtonSection}
                    variant="icon"
                  />
                </div>
                <SpriteIcon icon="play" />
              </div>
              <h3 className="mt-2 font-display text-lg text-neon-cyan">Play</h3>
              <p className="mt-2 text-[1.32rem] leading-[1.1] text-[#bfd3ee]">
                {hasProvisioningToResume
                  ? "Returns to your current provisioning session exactly where it stopped."
                  : "Creates the instance, waits for readiness, runs provisioning, and opens pairing guidance."}
              </p>
            </div>
            <Button
              className="h-12 w-full justify-center text-[16px]"
              disabled={busy || !appState.selectedOffer}
              loading={blockingAction?.key === "provisioning.flow"}
              loadingText="Starting session..."
              onClick={handlePlay}
            >
              <SpriteIcon icon="play" />
              <span className="ml-1">{hasProvisioningToResume ? "Resume Provisioning" : "Play"}</span>
            </Button>
          </Card>
        </section>
      </div>



      <ServerPickerModal
        open={pickerOpen}
        onClose={() => setPickerOpen(false)}
        offers={offers}
        selectedOfferId={appState.selectedOffer?.id ?? null}
        serverPreferences={appState.serverPreferences}
        storageGb={appState.serverPreferences.storageGb}
        availableCountries={availableOfferCountries}
        searchingOffers={searchingOffers}
        offersPage={offersPage}
        offersHasNextPage={offersHasNextPage}
        busy={busy}
        onSearchOffers={onSearchOffers}
        onNextPage={onNextOffersPage}
        onPreviousPage={onPreviousOffersPage}
        onManualLocationSave={onManualLocationSave}
        onSelectOffer={async (offerId, storageGb) => {
          const selected = await onSelectOffer(offerId, storageGb);
          if (!selected) {
            return;
          }

          setPickerOpen(false);
          await onStartPlay();
          navigate("/provisioning");
        }}
        onUpdateServerPreferences={onSaveServerPreferences}
      />

      {walletModalOpen ? (
        <ModalFrame
          panelClassName="glass-panel pixel-frame crt-surface max-w-lg"
          zIndexClassName="z-40"
        >
          <ModalBody className="p-6">
            <div className="mb-4 flex items-center justify-between gap-3 border-b border-[#3e4270] pb-3">
              <div>
                <p className="font-display text-[10px] uppercase tracking-[0.14em] text-neon-cyan">
                  Vast.ai Wallet
                </p>
                <h3 className="mt-1 font-display text-lg text-white">
                  {walletAmountLabel}
                </h3>
              </div>
              <Button variant="ghost" onClick={() => setWalletModalOpen(false)}>
                Close
              </Button>
            </div>

            <p className="text-[1.15rem] leading-snug text-[#bfd3ee]">
              Open the correct Vast.ai page in your normal browser to add account credit, configure automatic top-ups, or manage API keys.
            </p>

            <div className="mt-4 space-y-3">
              <Button
                className="w-full justify-center"
                variant="secondary"
                disabled={busy}
                onClick={() => void handleOpenWalletBilling("open-add-credit")}
              >
                Add More Credits
              </Button>
              <Button
                className="w-full justify-center"
                variant="secondary"
                disabled={busy}
                onClick={() => void handleOpenWalletBilling("open-auto-topup")}
              >
                Add Credits at a Limit
              </Button>
              <Button
                className="w-full justify-center"
                variant="ghost"
                disabled={busy}
                onClick={() => void handleOpenWalletBilling("snapshot")}
              >
                Open Billing Overview
              </Button>
              <Button
                className="w-full justify-center"
                variant="ghost"
                disabled={busy}
                onClick={() => void openExternalUrl(VAST_API_KEY_URL)}
              >
                Open API Key Page
              </Button>
            </div>

            <div className="mt-4 flex flex-wrap items-center justify-between gap-3 border-t border-[#3e4270] pt-4 text-[1rem] text-[#8db7d8]">
              <div className="space-y-1">
                <p>Amount in account: {walletAmountLabel}</p>
                <p>
                  Source: {vastWalletSummary?.source === "vast_api" ? "Vast API" : "Unavailable"}
                </p>
              </div>
              <Button
                variant="ghost"
                disabled={busy}
                onClick={() => void onRefreshVastWalletSummary()}
              >
                Refresh Balance
              </Button>
            </div>
          </ModalBody>
        </ModalFrame>
      ) : null}

      {launchLibraryInstance ? (
        <LaunchLibraryModal
          instanceId={launchLibraryInstance.instanceId}
          instanceLabel={launchLibraryInstance.label}
          library={launchLibrary}
          loading={launchLibraryLoading}
          job={launchSoftwareJob}
          launchingAppId={launchingSoftwareAppId}
          artwork={softwareArtwork}
          artworkLoading={softwareArtworkLoading}
          onLoadLibrary={onLoadInstanceLaunchLibrary}
          onLaunchPc={handlePlayEmbedded}
          onLaunchSoftware={onLaunchInstanceSoftware}
          onPollJob={onPollLaunchSoftwareJob}
          onLoadArtwork={onLoadSoftwareArtwork}
          onClose={handleCloseLaunchLibrary}
        />
      ) : null}

      {displayInstance ? (
        <InstanceDisplayModal
          instance={displayInstance}
          onClose={() => setDisplayInstanceId(null)}
        />
      ) : null}

      {moonlightOptionsInstance ? (
        <InstanceMoonlightOptionsModal
          instance={moonlightOptionsInstance}
          onClose={() => setMoonlightOptionsInstanceId(null)}
        />
      ) : null}

      <SharedStorageSyncModal
        open={syncInstanceId !== null}
        busy={busy || instanceActionRunning}
        instanceId={syncInstanceId}
        onClose={() => setSyncInstanceId(null)}
        onLoadObjects={onListSyncableStorageObjects}
        onConfirmSync={handleSyncSelection}
      />

      <SharedStorageExportModal
        open={exportInstanceId !== null}
        busy={busy || instanceActionRunning}
        instanceId={exportInstanceId}
        onClose={() => setExportInstanceId(null)}
        onLoadObjects={onListExportableStorageObjects}
        onConfirmExport={handleExportSelection}
        onRefreshIndexing={onRefreshIndexing}
      />

      {connectionInfoModalType && (
        <ModalFrame
          panelClassName="glass-panel pixel-frame crt-surface max-w-xl"
          zIndexClassName="z-40"
        >
          <ModalBody className="p-6">
            <div className="mb-4 flex items-center justify-between gap-2 border-b border-[#3e4270] pb-2">
              <h3
                className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base"
                data-text="Managed Tunnel Info"
              >
                Managed Tunnel Info
              </h3>
              <AIPromptHelper
                topic="Managed WireGuard-Compatible Tunnel"
                promptText={APP_PROMPTS.wireguardModalInfo}
                variant="both"
              />
            </div>

            <div className="space-y-4 text-[1.2rem] leading-relaxed text-[#c5d8ec]">
              <p>
                Noland manages the secure desktop connection flow for you inside the app.
              </p>
              <div>
                <p className="mb-0.5 font-display text-[10px] uppercase tracking-[0.1em] text-neon-lime">
                  How it works
                </p>
                <p className="text-[1.15rem] text-[#b9cce2]">
                  Noland generates the connection config, starts the managed link locally, verifies connectivity to the remote instance, and then continues into streaming setup.
                </p>
              </div>
              <div>
                <p className="mb-0.5 font-display text-[10px] uppercase tracking-[0.1em] text-neon-lime">
                  Requirements
                </p>
                <p className="text-[1.15rem] text-[#b9cce2]">
                  No separate streaming client, VPN app, or networking-tool setup is required. If macOS or Linux asks for elevation, approve it so Noland can finish local configuration.
                </p>
              </div>
            </div>

            <div className="mt-6 flex flex-wrap justify-end gap-2 border-t border-[#3e4270] pt-4">
              <Button
                variant="secondary"
                onClick={() => setConnectionInfoModalType(null)}
              >
                Got it
              </Button>

            </div>
          </ModalBody>
        </ModalFrame>
      )}
    </main>
  );
}
