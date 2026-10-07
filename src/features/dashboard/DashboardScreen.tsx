import { translate, translateSource } from "../../lib/i18n";
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
import { SocialLinks } from "../../components/ui/SocialLinks";
import {
  VAST_BILLING_URL,
  VAST_API_KEY_URL,
} from "../../lib/constants";
import { buildDiagnosticIssueUrl } from "../../lib/githubIssue";
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
  SystemHealthReport,
  DiagnosticReportResponse,
  LaunchLibraryResponse,
  LaunchSoftwareJob,
  SoftwareArtworkResult,
} from "../../lib/types";
import { ServerPickerModal } from "../servers/ServerPickerModal";
import { SharedStorageExportModal } from "../shared-storage-manager/SharedStorageExportModal";
import { InstanceCardActions } from "../shared-storage-manager/InstanceCardActions";
import { InstanceDisplayModal } from "./InstanceDisplayModal";
import { InstanceMoonlightOptionsModal } from "./InstanceMoonlightOptionsModal";
import { InstancePerformanceToggle } from "./InstancePerformanceToggle";
import { SharedStorageSyncModal } from "../shared-storage-manager/SharedStorageSyncModal";
import { LaunchLibraryModal } from "../launch-library/LaunchLibraryModal";
import { InstanceTerminalModal } from "./InstanceTerminalModal";
import { InstanceUploadModal } from "./InstanceUploadModal";

import { TutorialModal } from "../onboarding/TutorialModal";
import { tutorialSteps } from "../onboarding/tutorialSteps";

function healthStatusClass(status: "ok" | "warning" | "failed") {
  if (status === "failed") {
    return "border-[#ff8ca2] bg-[#361220] text-[#ffc1cf]";
  }
  if (status === "warning") {
    return "border-[#ffd166] bg-[#3c2c13] text-[#ffe0a3]";
  }
  return "border-[#7bff48] bg-[#142815] text-[#b4ff88]";
}

function healthStatusGlyph(status: "ok" | "warning" | "failed") {
  if (status === "failed") {
    return "✕";
  }
  if (status === "warning") {
    return "!";
  }
  return "✓";
}

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
  systemHealth: SystemHealthReport | null;
  healthChecking: boolean;
  lastDiagnosticReport: DiagnosticReportResponse | null;
  onRefreshSystemHealth: () => Promise<SystemHealthReport | null>;
  onExportCrashReport: (reason?: string, frontendError?: string) => Promise<DiagnosticReportResponse | null>;
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
  onUploadPathsToInstance: (
    instanceId: number,
    paths: string[],
    destination?: string,
  ) => Promise<void>;
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
  systemHealth,
  healthChecking,
  lastDiagnosticReport,
  onRefreshSystemHealth,
  onExportCrashReport,
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
  onUploadPathsToInstance,
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
  const [healthModalOpen, setHealthModalOpen] = useState(false);
  const [tutorialOpen, setTutorialOpen] = useState(false);
  const [tutorialStep, setTutorialStep] = useState(0);
  const [terminalInstanceId, setTerminalInstanceId] = useState<number | null>(null);
  const [uploadInstanceId, setUploadInstanceId] = useState<number | null>(null);
  const [connectionInfoModalType, setConnectionInfoModalType] = useState<
    "wireguard" | null
  >(null);
  const navigate = useNavigate();
  const blockingLabel = blockingAction?.label ?? null;
  const blockingDetail = blockingAction?.detail ?? null;
  const backgroundTransferRunning =
    blockingAction?.key === "instance.storage.export" ||
    blockingAction?.key === "instance.storage.sync" ||
    blockingAction?.key === "instance.files.upload";

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
  const uploadInstance = rentedInstances.find(
    (instance) => instance.instanceId === uploadInstanceId,
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

  async function handleHealthClick() {
    setHealthModalOpen(true);
    await onRefreshSystemHealth();
  }

  async function handleExportDiagnostics() {
    const report = await onExportCrashReport("manual-health-export");
    if (!report) {
      return;
    }
    await openExternalUrl(
      buildDiagnosticIssueUrl({
        report,
        reason: "manual-health-export",
        health: systemHealth,
      }),
    );
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
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-8">
        <header className="flex flex-wrap items-center justify-between gap-4">
          <div>
            <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">
              {translate("generated.939edfedc701440a")}
            </p>
            <h1
              className="pixel-heading glitch-title font-display text-lg text-white md:text-2xl"
              data-text={translate("generated.5abdf7d9b9336afa")}
            >
              {translate("generated.5abdf7d9b9336afa")}
            </h1>
          </div>

          <div className="flex flex-wrap items-center justify-end gap-2">
            <SocialLinks />
            <Button
              variant={systemHealth?.ok === false ? "danger" : "secondary"}
              onClick={handleHealthClick}
              loading={healthChecking}
              loadingText={translate("generated.2e5f79bb94a8c40b")}
              title={systemHealth?.summary ?? "Run local health check"}
            >
              {systemHealth?.ok === false ? "✕" : "✓"} {translate("generated.55898449eb74fb2e")}
            </Button>
            <Button variant="ghost" onClick={() => setWalletModalOpen(true)}>
              {translate("generated.d1c9a01d57e90086")} {walletAmountLabel}
            </Button>
            <Button variant="ghost" onClick={openTutorial}>
              <SpriteIcon icon="help" />
              <span className="ml-1">{translate("generated.b79cac926e0b2e34")}</span>
            </Button>
            <Button variant="ghost" onClick={() => navigate("/settings")}>
              {translate("generated.74a883a037bc227f")}
            </Button>
            <Button variant="secondary" onClick={openServerPicker}>
              {translate("generated.7f67df7f92611db8")}
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
                  {translate("generated.93c334eff4680565")}
                </p>
                <div className="flex items-center gap-2">
                  <AIPromptHelper
                    topic={translate("generated.02ae4e56fc76195a")}
                    promptText={APP_PROMPTS.moonlightCard}
                    variant="icon"
                  />
                  <SpriteIcon icon="moonlight" />
                </div>
              </div>
              <h2 className="mt-3 font-display text-lg text-neon-cyan md:text-xl">
                {translate("generated.5529d73ce1cd5079")}
              </h2>
                <p className="mt-2 max-w-md text-[1.05rem] leading-[1.35] text-[#bfd3ee]">
                {translate("generated.8d59aafff35c681f")}
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
                      {translate("generated.52a74e1ffc3f1baa")}
                    </p>
                    <div className="flex items-center gap-2">
                      <AIPromptHelper
                        topic={translate("generated.f2e5326591175e88")}
                        promptText={APP_PROMPTS.wireguardCard}
                        variant="icon"
                      />
                      <SpriteIcon icon="settings" />
                    </div>
                  </div>
                  <h2 className="mt-2 font-display text-base text-neon-cyan md:text-lg">
                    {translate("generated.2b5c7b7fbbe383ab")}
                  </h2>
                  <p className="mt-1 text-[1.05rem] leading-[1.35] text-[#bfd3ee]">
                    {translate("generated.b9650fee430ae7ef")}
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
                  {translate("generated.63de981f20f1b79e")}
                </p>
                <div className="flex items-center gap-2">
                  <AIPromptHelper
                    topic={translate("generated.0715b661bc09caf0")}
                    promptText={APP_PROMPTS.setServerCard}
                    variant="icon"
                  />
                  <SpriteIcon icon="server" />
                </div>
              </div>
              <h2 className="mt-3 font-display text-lg text-neon-cyan md:text-xl">
                {translate("generated.13a1dea00f2fb883")}
              </h2>
              <p className="mt-2 text-[1.05rem] leading-[1.35] text-[#bfd3ee]">
                {translate("generated.944448cfcf5a0ab6")}
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

        {healthModalOpen && (
          <ModalFrame
            panelClassName="pixel-frame max-w-3xl bg-[#080c1d] text-[#d9efff]"
            labelledBy="health-modal-title"
          >
            <div className="flex items-start justify-between border-b border-[#28345f] p-4">
              <div>
                <p className="font-display text-[10px] uppercase tracking-[0.18em] text-neon-lime">
                  {translate("generated.da665adafe48a74d")}
                </p>
                <h2 id="health-modal-title" className="mt-1 font-display text-lg text-white">
                  {systemHealth?.ok === false ? translate("generated.b8e8c4ff7f1d634d") : translate("generated.3ea7982fb433d012")}
                </h2>
                <p className="mt-1 text-[1.15rem] leading-[1.2] text-[#9ec4df]">
                  {systemHealth?.summary ?? "Run the checker before provisioning to catch local OS, resource, tunnel, and credential issues."}
                </p>
              </div>
              <Button variant="ghost" onClick={() => setHealthModalOpen(false)}>
                {translate("generated.7d9eb7acb13e2462")}
              </Button>
            </div>
            <ModalBody className="space-y-4 p-4">
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  variant="secondary"
                  onClick={onRefreshSystemHealth}
                  loading={healthChecking}
                  loadingText={translate("generated.2e5f79bb94a8c40b")}
                >
                  {translate("generated.9df526d3b6303b71")}
                </Button>
                <Button variant="ghost" onClick={handleExportDiagnostics}>
                  {translate("generated.ab1e59fdbe6ad32c")}
                </Button>
                {lastDiagnosticReport && (
                  <span className="text-[1rem] text-[#9ec4df]">
                    {translate("generated.79eddc25561b75c0")} {lastDiagnosticReport.path}
                  </span>
                )}
              </div>

              {systemHealth ? (
                <div className="space-y-2">
                  {systemHealth.probes.map((probe) => (
                    <div
                      key={probe.id}
                      className={`rounded border p-3 ${healthStatusClass(probe.status)}`}
                    >
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <div className="flex items-center gap-2">
                          <span className="font-display text-sm">{healthStatusGlyph(probe.status)}</span>
                          <div>
                            <p className="font-display text-[11px] uppercase tracking-[0.12em]">
                              {probe.label}
                            </p>
                            <p className="text-[1.08rem] leading-[1.15]">{probe.summary}</p>
                          </div>
                        </div>
                        <span className="font-display text-[10px] uppercase tracking-[0.12em] opacity-80">
                          {probe.category}
                        </span>
                      </div>
                      {probe.details && (
                        <p className="mt-2 break-all text-[0.95rem] opacity-85">{probe.details}</p>
                      )}
                      {probe.fixHint && (
                        <p className="mt-2 text-[1rem] leading-[1.15] opacity-95">{translate("generated.943df968a5021d33")} {probe.fixHint}</p>
                      )}
                    </div>
                  ))}
                </div>
              ) : (
                <p className="text-[1.15rem] text-[#9ec4df]">
                  {translate("generated.823cb6b61328a1d2")}
                </p>
              )}
            </ModalBody>
          </ModalFrame>
        )}

        <section>
          <Card className="pixel-frame">
            <div className="mb-3 flex items-center justify-between">
              <div className="flex items-center gap-2">
                <h3 className="font-display text-sm uppercase tracking-[0.12em] text-white">
                  {translate("generated.377987be4b69f067")}
                </h3>
                <AIPromptHelper
                  topic={translate("generated.1c38c4fc3cc17d82")}
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
                loadingText={translate("generated.69d2daed978a7b05")}
              >
                {translate("generated.6324abb41281d96a")}
              </Button>
            </div>

            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
              {rentedInstances.map((instance) => (
                (() => {
                  const rawStatus = instance.status.toLowerCase();
                  const isActive = rawStatus.includes("run") && !rawStatus.includes("inactive");
                  const isInactive = rawStatus.includes("inactive") || rawStatus.includes("stopped") || rawStatus.includes("exited");
                  return (
                <Card
                  key={instance.instanceId}
                  className={`border-2 ${isInactive ? "border-[#9a6536] bg-[#211a1a]" : "border-[#3a4068]"}`}
                >
                  <div className="flex items-center justify-between gap-2">
                    <h4 className="font-display text-[11px] text-white">
                      {instance.label}
                    </h4>
                    <div className="flex items-center gap-2">
                      <StatusPill
                        state={
                          isActive
                            ? "Ready"
                            : isInactive ? "Inactive" : "Waiting for instance"
                        }
                      />
                      <Button
                        variant="ghost"
                        aria-label={translate("dashboard.upload.for", { instance: instance.label })}
                        title={translate("generated.3a831e097826f713")}
                        className="h-8 w-8 rounded border border-[#3a4068] p-0 font-mono text-lg leading-none"
                         disabled={busy || backgroundTransferRunning || !isActive}
                        onClick={() => setUploadInstanceId(instance.instanceId)}
                      >
                        <span aria-hidden="true">↑</span>
                      </Button>
                      <Button
                        variant="ghost"
                        aria-label={translate("dashboard.terminal.for", { instance: instance.label })}
                        title={translate("generated.786dd1c4619c5886")}
                        className="h-8 w-8 rounded border border-[#3a4068] p-0 font-mono text-lg leading-none"
                         disabled={busy || !isActive}
                        onClick={() => setTerminalInstanceId(instance.instanceId)}
                      >
                        <span aria-hidden="true">{translate("generated.77d57e39b407cbca")}</span>
                      </Button>
                      <Button
                        variant="ghost"
                        aria-label={translate("dashboard.moonlight.for", { instance: instance.label })}
                        title={translate("generated.8a7d65d36229a83f")}
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
                  <div className="mt-2 grid grid-cols-2 gap-2 text-[1rem] leading-[1.25] text-[#bfd3ee]">
                    <p>{translate("generated.3ea36adcd1e02c94")} {instance.instanceId}</p>
                    <p>{translate("generated.755c8b2a9fb11446")} {instance.status}</p>
                    <p>{translate("generated.4728386bbdc046f4")} {instance.gpuName}</p>
                    <p>{translate("generated.2057beebaff849ee")} {instance.sshHost || "pending"}</p>
                  </div>
                  {isInactive && (
                    <div className="mt-3 border border-[#9a6536]/70 bg-[#3a2518]/70 px-3 py-2 text-sm text-[#ffd3a3]">
                      <strong className="uppercase tracking-wide">{translate("generated.25d176847e116da8")}</strong>
                      <p className="mt-1">{translate("generated.071ed7ead80c2626")}</p>
                    </div>
                  )}
                  {instance.embeddedMoonlightPipelineEnabled && (
                    <div className="mt-2 space-y-2">
                      <div className="rounded border border-neon-cyan/30 bg-neon-cyan/10 px-2 py-1 text-[11px] uppercase tracking-wide text-neon-cyan">
                        {translate("generated.90dd80c0a2d8e7d4")}
                      </div>
                      {(instance.embeddedMoonlightSessionState ||
                        instance.embeddedMoonlightLastRuntimeEvent ||
                        instance.embeddedMoonlightLastError ||
                        embeddedMoonlightStatus?.instanceId === instance.instanceId) && (
                        <div className="rounded border border-[#3a4068] bg-[#10152f]/60 px-2 py-2 text-[11px] text-[#bfd3ee]">
                          <p>
                            {translate("generated.35706b2709641cb1")} {instance.embeddedMoonlightSessionState ?? embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? instance.embeddedMoonlightSessionState ?? embeddedMoonlightStatus?.sessionState
                              : translate("generated.b23a6a8439c0dde5")}
                          </p>
                          <p>
                            {translate("generated.725e54082418e563")} {instance.embeddedMoonlightPaired ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.paired
                              : null)
                              ? translate("generated.8a798890fe938171")
                              : translate("generated.9390298f3fb0c5b1")}
                          </p>
                          <p>
                            {translate("generated.a556d13c016c9697")} {instance.embeddedMoonlightRuntimeConnected ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.runtimeConnected
                              : null)
                              ? translate("generated.8a798890fe938171")
                              : translate("generated.9390298f3fb0c5b1")}
                          </p>
                          <p>
                            {translate("generated.2c0ceb233eb03475")} {instance.embeddedMoonlightRendererReady ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.rendererReady
                              : null)
                              ? translate("generated.8a798890fe938171")
                              : translate("generated.9390298f3fb0c5b1")}
                          </p>
                          <p>
                            {translate("generated.9f6b0ae27fd4ff8a")} {instance.embeddedMoonlightVideoSessionActive ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.videoSessionActive
                              : null)
                              ? translate("generated.8a798890fe938171")
                              : translate("generated.9390298f3fb0c5b1")}
                          </p>
                          <p>
                            {translate("generated.d84f0a52f55e9937")} {instance.embeddedMoonlightVideoFrameCount ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.videoFrameCount
                              : 0) ?? 0}
                          </p>
                          <p>
                            {translate("generated.8d69a105ef6dfff2")} {instance.embeddedMoonlightRendererSubmittedFrameCount ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.rendererSubmittedFrameCount
                              : 0) ?? 0}
                          </p>
                          <p>
                            {translate("generated.5f19c10a4dd6983e")} {instance.embeddedMoonlightRendererDroppedFrameCount ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
                              ? embeddedMoonlightStatus?.rendererDroppedFrameCount
                              : 0) ?? 0}
                          </p>
                          <p>
                            {translate("generated.b36b7d106c26870f")} {instance.embeddedMoonlightAudioSampleCount ?? (embeddedMoonlightStatus?.instanceId === instance.instanceId
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
                              {translateSource(instance.embeddedMoonlightLastError ??
                                (embeddedMoonlightStatus?.instanceId === instance.instanceId
                                  ? embeddedMoonlightStatus?.lastError
                                  : null) ?? "")}
                            </p>
                          ) : null}
                        </div>
                      )}
                    </div>
                  )}
                  <InstancePerformanceToggle instance={instance} />
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
                  {isActive && (
                    <div className="mt-3">
                      <MicControls instanceId={instance.instanceId} />
                    </div>
                  )}

                </Card>
                  );
                })()
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
                  {translate("generated.8167946ca4e902a1")}
                </h3>
                <AIPromptHelper
                  topic={translate("generated.4635f03c5fe75af6")}
                  promptText={APP_PROMPTS.selectedServerSection}
                  variant="icon"
                />
              </div>
              <StatusPill state={appState.orchestrationState} />
            </div>

            {appState.selectedOffer ? (
              <div className="mt-4 grid grid-cols-2 gap-x-4 gap-y-3 text-[1.05rem] leading-[1.25] text-[#d9efff] md:grid-cols-4">
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    {translate("generated.4a823118b9ba8baa")}
                  </p>
                  <p>{appState.selectedOffer.hostLabel}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    {translate("generated.15b61974b2707a7b")}
                  </p>
                  <p>{appState.selectedOffer.locationLabel}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    {translate("generated.ea49523d744c40bc")}
                  </p>
                  <p>{appState.selectedOffer.gpuName}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    {translate("generated.38b80f7fd7ffabfa")}
                  </p>
                  <p>${appState.selectedOffer.hourlyPrice.toFixed(3)}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    {translate("generated.b7bdf7a2d6e73e58")}
                  </p>
                  <p>
                    {appState.selectedOffer.estimatedDistanceKm.toFixed(0)} {translate("generated.1f34503f65b4a355")}
                  </p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    {translate("generated.71d0acd465564246")}
                  </p>
                  <p>
                    {(appState.selectedOffer.reliability * 100).toFixed(1)}%
                  </p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    {translate("generated.a69c4dece144a46e")}
                  </p>
                  <p>{appState.serverPreferences.storageGb} {translate("generated.b4043b0b8297e379")}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">
                    {translate("generated.0575f29df888a27e")}
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
                  label={translate("generated.71d0acd465564246")}
                  value={appState.selectedOffer.reliability}
                  valueLabel={`${Math.round(appState.selectedOffer.reliability * 100)}%`}
                />
                <HudBar
                  label={translate("generated.de956947095b0d2d")}
                  value={appState.selectedOffer.gpuRamMb}
                  max={49152}
                  valueLabel={`${(appState.selectedOffer.gpuRamMb / 1024).toFixed(1)} GB`}
                />
                <HudBar
                  label={translate("generated.b7bdf7a2d6e73e58")}
                  value={Math.max(
                    0,
                    1000 - appState.selectedOffer.estimatedDistanceKm,
                  )}
                  max={1000}
                  valueLabel={`${appState.selectedOffer.estimatedDistanceKm.toFixed(0)} km`}
                />
              </div>
            ) : (
                <p className="mt-4 max-w-prose text-[1.1rem] leading-[1.35] text-[#bfd3ee]">
                {translate("generated.2ec222346cf0a907")}
              </p>
            )}
          </Card>

          <Card className="pixel-frame flex flex-col justify-between gap-4">
            <div>
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">
                    {translate("generated.64cff1319d2fd2cb")}
                  </p>
                  <AIPromptHelper
                    topic={translate("generated.b1eb2b632a9b1918")}
                    promptText={APP_PROMPTS.playButtonSection}
                    variant="icon"
                  />
                </div>
                <SpriteIcon icon="play" />
              </div>
              <h3 className="mt-2 font-display text-lg text-neon-cyan">{translate("generated.436e61016e26fcb7")}</h3>
              <p className="mt-2 text-[1.05rem] leading-[1.35] text-[#bfd3ee]">
                {hasProvisioningToResume
                  ? translate("generated.8c63ca6078501e9d")
                  : translate("generated.b6d25b1d2fc7727e")}
              </p>
            </div>
            <Button
              className="h-12 w-full justify-center text-[16px]"
              disabled={busy || !appState.selectedOffer}
              loading={blockingAction?.key === "provisioning.flow"}
              loadingText={translate("generated.ddd6be90f145f44e")}
              onClick={handlePlay}
            >
              <SpriteIcon icon="play" />
              <span className="ml-1">{hasProvisioningToResume ? translate("generated.4f1edcaa1c3caf53") : translate("generated.436e61016e26fcb7")}</span>
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
                  {translate("generated.894e753bb88cb1bd")}
                </p>
                <h3 className="mt-1 font-display text-lg text-white">
                  {walletAmountLabel}
                </h3>
              </div>
              <Button variant="ghost" onClick={() => setWalletModalOpen(false)}>
                {translate("generated.7d9eb7acb13e2462")}
              </Button>
            </div>

            <p className="text-[1.15rem] leading-snug text-[#bfd3ee]">
              {translate("generated.2b3c522de8b33dc6")}
            </p>

            <div className="mt-4 space-y-3">
              <Button
                className="w-full justify-center"
                variant="secondary"
                disabled={busy}
                onClick={() => void handleOpenWalletBilling("open-add-credit")}
              >
                {translate("generated.790569733b9584b7")}
              </Button>
              <Button
                className="w-full justify-center"
                variant="secondary"
                disabled={busy}
                onClick={() => void handleOpenWalletBilling("open-auto-topup")}
              >
                {translate("generated.0a80df7c34ab6f73")}
              </Button>
              <Button
                className="w-full justify-center"
                variant="ghost"
                disabled={busy}
                onClick={() => void handleOpenWalletBilling("snapshot")}
              >
                {translate("generated.3ac15a5b73ee0fba")}
              </Button>
              <Button
                className="w-full justify-center"
                variant="ghost"
                disabled={busy}
                onClick={() => void openExternalUrl(VAST_API_KEY_URL)}
              >
                {translate("generated.c533d3f8d6384008")}
              </Button>
            </div>

            <div className="mt-4 flex flex-wrap items-center justify-between gap-3 border-t border-[#3e4270] pt-4 text-[1rem] text-[#8db7d8]">
              <div className="space-y-1">
                <p>{translate("generated.300c7d1736fdf817")} {walletAmountLabel}</p>
                <p>
                  {translate("generated.c707ee4ecc240442")} {vastWalletSummary?.source === "vast_api" ? translate("generated.795c074989a911aa") : translate("generated.ca184496974204a0")}
                </p>
              </div>
              <Button
                variant="ghost"
                disabled={busy}
                onClick={() => void onRefreshVastWalletSummary()}
              >
                {translate("generated.28258f27617c03b4")}
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
                data-text={translate("generated.91d66582898286d6")}
              >
                {translate("generated.91d66582898286d6")}
              </h3>
              <AIPromptHelper
                topic={translate("generated.baa665733259cd2c")}
                promptText={APP_PROMPTS.wireguardModalInfo}
                variant="both"
              />
            </div>

            <div className="space-y-4 text-[1.2rem] leading-relaxed text-[#c5d8ec]">
              <p>
                {translate("generated.5eafc4cc64f90e35")}
              </p>
              <div>
                <p className="mb-0.5 font-display text-[10px] uppercase tracking-[0.1em] text-neon-lime">
                  {translate("generated.9c870aa6e5e93270")}
                </p>
                <p className="text-[1.15rem] text-[#b9cce2]">
                  {translate("generated.a2090f22f42ac636")}
                </p>
              </div>
              <div>
                <p className="mb-0.5 font-display text-[10px] uppercase tracking-[0.1em] text-neon-lime">
                  {translate("generated.e0cdd07f6a270b82")}
                </p>
                <p className="text-[1.15rem] text-[#b9cce2]">
                  {translate("generated.94e7cc34553f60b3")}
                </p>
              </div>
            </div>

            <div className="mt-6 flex flex-wrap justify-end gap-2 border-t border-[#3e4270] pt-4">
              <Button
                variant="secondary"
                onClick={() => setConnectionInfoModalType(null)}
              >
                {translate("generated.5ad3dbd1242a4cea")}
              </Button>

            </div>
          </ModalBody>
        </ModalFrame>
      )}

      {terminalInstanceId !== null && (
        <InstanceTerminalModal
          instance={rentedInstances.find((candidate) => candidate.instanceId === terminalInstanceId) ?? null}
          onClose={() => setTerminalInstanceId(null)}
        />
      )}

      {uploadInstance && (
        <InstanceUploadModal
          instance={uploadInstance}
          onUpload={onUploadPathsToInstance}
          onClose={() => setUploadInstanceId(null)}
        />
      )}
    </main>
  );
}
