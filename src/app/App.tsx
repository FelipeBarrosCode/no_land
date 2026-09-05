import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import { BlockingLoaderOverlay } from "../components/ui/BlockingLoaderOverlay";
import { HashRouter, Navigate, Route, Routes } from "react-router-dom";
import { Button } from "../components/ui/Button";
import { Card } from "../components/ui/Card";
import { ModalBody, ModalFrame } from "../components/ui/ModalFrame";
import { DashboardScreen } from "../features/dashboard/DashboardScreen";
import { OnboardingScreen } from "../features/onboarding/OnboardingScreen";
import { ProvisioningScreen } from "../features/provisioning/ProvisioningScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { StreamWindowScreen } from "../features/moonlight/StreamWindowScreen";
import { useAppStore } from "../store/appStore";
import appLogo from "../public/noland.png";
import { refreshStateAgentIndex } from "../lib/backend";
import { checkForGitHubUpdate, type AppUpdateInfo } from "../lib/updateChecker";

function RootRoute() {
  const appState = useAppStore((state) => state.appState);
  const busy = useAppStore((state) => state.busy);
  const offers = useAppStore((state) => state.offers);
  const rentedInstances = useAppStore((state) => state.rentedInstances);
  const embeddedMoonlightStatus = useAppStore(
    (state) => state.embeddedMoonlightStatus,
  );
  const searchingOffers = useAppStore((state) => state.searching);
  const offersPage = useAppStore((state) => state.offersPage);
  const offersHasNextPage = useAppStore((state) => state.offersHasNextPage);
  const instanceActionRunning = useAppStore(
    (state) => state.instanceActionRunning,
  );

  const blockingAction = useAppStore((state) => state.blockingAction);
  const runOnboarding = useAppStore((state) => state.runOnboarding);
  const vastWalletSummary = useAppStore((state) => state.vastWalletSummary);
  const refreshVastWalletSummary = useAppStore(
    (state) => state.refreshVastWalletSummary,
  );
  const discoverOffers = useAppStore((state) => state.discoverOffers);
  const nextOffersPage = useAppStore((state) => state.nextOffersPage);
  const previousOffersPage = useAppStore((state) => state.previousOffersPage);
  const saveManualLocation = useAppStore((state) => state.saveManualLocation);
  const loadRentedInstances = useAppStore((state) => state.loadRentedInstances);
  const chooseOffer = useAppStore((state) => state.chooseOffer);
  const startPlay = useAppStore((state) => state.startPlay);
  const resumeProvisioningExisting = useAppStore(
    (state) => state.resumeProvisioningExisting,
  );
  const startPlayExisting = useAppStore((state) => state.startPlayExisting);
  const launchLibrary = useAppStore((state) => state.launchLibrary);
  const launchLibraryLoading = useAppStore((state) => state.launchLibraryLoading);
  const launchSoftwareJob = useAppStore((state) => state.launchSoftwareJob);
  const launchingSoftwareAppId = useAppStore(
    (state) => state.launchingSoftwareAppId,
  );
  const softwareArtwork = useAppStore((state) => state.softwareArtwork);
  const softwareArtworkLoading = useAppStore(
    (state) => state.softwareArtworkLoading,
  );
  const loadInstanceLaunchLibrary = useAppStore(
    (state) => state.loadInstanceLaunchLibrary,
  );
  const launchInstanceSoftware = useAppStore(
    (state) => state.launchInstanceSoftware,
  );
  const pollLaunchSoftwareJob = useAppStore(
    (state) => state.pollLaunchSoftwareJob,
  );
  const loadSoftwareArtwork = useAppStore(
    (state) => state.loadSoftwareArtwork,
  );
  const clearLaunchLibrary = useAppStore((state) => state.clearLaunchLibrary);
  const saveServerPreferences = useAppStore(
    (state) => state.saveServerPreferences,
  );
  const loadAvailableOfferCountries = useAppStore(
    (state) => state.loadAvailableOfferCountries,
  );

  const setEmbeddedMoonlightPipelineEnabled = useAppStore(
    (state) => state.setEmbeddedMoonlightPipelineEnabled,
  );
  const loadEmbeddedMoonlightStatus = useAppStore(
    (state) => state.loadEmbeddedMoonlightStatus,
  );

  useEffect(() => {
    if (!embeddedMoonlightStatus?.enabled) {
      return;
    }

    const interval = window.setInterval(() => {
      void loadEmbeddedMoonlightStatus(embeddedMoonlightStatus.instanceId);
    }, 1000);

    return () => {
      window.clearInterval(interval);
    };
  }, [embeddedMoonlightStatus?.enabled, embeddedMoonlightStatus?.instanceId, loadEmbeddedMoonlightStatus]);


  const rebootInstanceServices = useAppStore(
    (state) => state.rebootInstanceServices,
  );
  const destroyInstance = useAppStore((state) => state.destroyInstance);
  const syncInstanceStorage = useAppStore((state) => state.syncInstanceStorage);
  const listSyncableStorageObjects = useAppStore(
    (state) => state.listSyncableStorageObjects,
  );
  const saveInstanceStorageSelected = useAppStore(
    (state) => state.saveInstanceStorageSelected,
  );
  const listExportableStorageObjects = useAppStore(
    (state) => state.listExportableStorageObjects,
  );

  if (!appState) {
    return null;
  }

  if (!appState.onboardingCompleted) {
    return (
      <OnboardingScreen busy={busy} onSubmit={runOnboarding} />
    );
  }

  return (
    <DashboardScreen
      appState={appState}
      offers={offers}
      rentedInstances={rentedInstances}
      embeddedMoonlightStatus={embeddedMoonlightStatus}
      searchingOffers={searchingOffers}
      offersPage={offersPage}
      offersHasNextPage={offersHasNextPage}
      busy={busy}
      instanceActionRunning={instanceActionRunning}
      blockingAction={blockingAction}
      vastWalletSummary={vastWalletSummary}
      onSearchOffers={discoverOffers}
      onLoadAvailableOfferCountries={loadAvailableOfferCountries}
      onNextOffersPage={nextOffersPage}
      onPreviousOffersPage={previousOffersPage}
      onManualLocationSave={saveManualLocation}
      onLoadRentedInstances={loadRentedInstances}
      onRefreshVastWalletSummary={refreshVastWalletSummary}
      onResumeProvisioningExisting={resumeProvisioningExisting}
      onStartPlayExisting={startPlayExisting}
      launchLibrary={launchLibrary}
      launchLibraryLoading={launchLibraryLoading}
      launchSoftwareJob={launchSoftwareJob}
      launchingSoftwareAppId={launchingSoftwareAppId}
      softwareArtwork={softwareArtwork}
      softwareArtworkLoading={softwareArtworkLoading}
      onLoadInstanceLaunchLibrary={loadInstanceLaunchLibrary}
      onLaunchInstanceSoftware={launchInstanceSoftware}
      onPollLaunchSoftwareJob={pollLaunchSoftwareJob}
      onLoadSoftwareArtwork={loadSoftwareArtwork}
      onClearLaunchLibrary={clearLaunchLibrary}
      onSelectOffer={chooseOffer}
      onStartPlay={startPlay}
      onSaveServerPreferences={saveServerPreferences}
      onSetEmbeddedMoonlightPipelineEnabled={setEmbeddedMoonlightPipelineEnabled}
      onLoadEmbeddedMoonlightStatus={loadEmbeddedMoonlightStatus}
      onRebootInstanceServices={rebootInstanceServices}
      onDestroyInstance={destroyInstance}
      onSaveInstanceStorageSelected={saveInstanceStorageSelected}
      onSyncInstanceStorage={syncInstanceStorage}
      onListSyncableStorageObjects={listSyncableStorageObjects}
      onListExportableStorageObjects={listExportableStorageObjects}
      onRefreshIndexing={async (instanceId?: number) => {
        if (!instanceId) {
          return;
        }
        await refreshStateAgentIndex(instanceId);
      }}
    />
  );
}

function ProvisioningRoute() {
  const appState = useAppStore((state) => state.appState);
  const logs = useAppStore((state) => state.logs);
  const busy = useAppStore((state) => state.busy);
  const blockingAction = useAppStore((state) => state.blockingAction);
  const provisioningModalDismissed = useAppStore(
    (state) => state.provisioningModalDismissed,
  );
  const dismissProvisioningModal = useAppStore(
    (state) => state.dismissProvisioningModal,
  );
  const reopenProvisioningModal = useAppStore(
    (state) => state.reopenProvisioningModal,
  );
  const setupWireguardAppHandoff = useAppStore(
    (state) => state.setupWireguardAppHandoff,
  );

  const setupMoonlightSunshine = useAppStore(
    (state) => state.setupMoonlightSunshine,
  );
  const activeMoonlightPairing = useAppStore(
    (state) => state.activeMoonlightPairing,
  );
  const prepareEmbeddedMoonlightPairing = useAppStore(
    (state) => state.prepareEmbeddedMoonlightPairing,
  );
  const completeEmbeddedMoonlightPairing = useAppStore(
    (state) => state.completeEmbeddedMoonlightPairing,
  );
  const retrySetupStage = useAppStore((state) => state.retrySetupStage);
  const sleepPreventionActive = useAppStore(
    (state) => state.sleepPreventionActive,
  );
  const startSleepPrevention = useAppStore(
    (state) => state.startSleepPrevention,
  );
  const stopSleepPrevention = useAppStore((state) => state.stopSleepPrevention);

  if (!appState) {
    return null;
  }

  if (!appState.onboardingCompleted) {
    return <Navigate to="/" replace />;
  }

  const provisioningInstanceId =
    appState.postWireguardSetup.currentInstanceId ?? appState.instance.instanceId;

  return (
    <ProvisioningScreen
      appState={appState}
      logs={logs}
      busy={busy}
      provisioningModalDismissed={provisioningModalDismissed}
      onDismissProvisioningModal={dismissProvisioningModal}
      onReopenProvisioningModal={reopenProvisioningModal}
      blockingAction={blockingAction}
      onSetupWireguardAppHandoff={setupWireguardAppHandoff}
      onSetupMoonlightSunshine={setupMoonlightSunshine}
      activeMoonlightPairing={activeMoonlightPairing}
      onPrepareMoonlightPairingHandoff={() => {
        if (!provisioningInstanceId) {
          return Promise.resolve(null);
        }
        return prepareEmbeddedMoonlightPairing(provisioningInstanceId);
      }}
      onCompleteMoonlightPairingHandoff={(sessionId) => {
        if (!provisioningInstanceId) {
          return Promise.resolve(null);
        }
        return completeEmbeddedMoonlightPairing(
          provisioningInstanceId,
          sessionId,
        );
      }}
      onRetrySetupStage={retrySetupStage}
      sleepPreventionActive={sleepPreventionActive}
      onStartSleepPrevention={startSleepPrevention}
      onStopSleepPrevention={stopSleepPrevention}
    />
  );
}

function UpdateAvailableModal({
  update,
  onDismiss,
}: {
  update: AppUpdateInfo;
  onDismiss: () => void;
}) {
  const [opening, setOpening] = useState(false);
  const releaseDate = update.publishedAt
    ? new Date(update.publishedAt).toLocaleDateString()
    : null;

  async function openDownload() {
    setOpening(true);
    try {
      await openUrl(update.releaseUrl);
      onDismiss();
    } finally {
      setOpening(false);
    }
  }

  return (
    <ModalFrame panelClassName="glass-panel pixel-frame max-w-xl" zIndexClassName="z-[120]">
      <div className="flex shrink-0 items-center justify-between border-b-2 border-[#3e4270] px-5 py-4">
        <div>
          <h2
            className="pixel-heading glitch-title font-display text-sm text-white md:text-base"
            data-text="Update Available"
          >
            Update Available
          </h2>
          <p className="text-[1.15rem] leading-none text-[#b4c8de]">
            Noland Connect {update.latestVersion} is ready to download.
          </p>
        </div>
        <Button variant="ghost" onClick={onDismiss}>
          Later
        </Button>
      </div>

      <ModalBody className="px-5 py-4">
        <Card className="text-[1.2rem] text-[#c6dbf4]">
          <div className="grid gap-2">
            <p>
              Current version: <span className="text-[#9ad9ff]">{update.currentVersion}</span>
            </p>
            <p>
              New version: <span className="text-neon-lime">{update.latestVersion}</span>
            </p>
            {releaseDate && <p>Published: {releaseDate}</p>}
          </div>

          <div className="mt-4 max-h-48 overflow-y-auto whitespace-pre-wrap border border-[#3e4270] bg-[#070b1b] p-3 text-[1.05rem] leading-snug text-[#b4c8de]">
            {update.releaseNotes}
          </div>
        </Card>

        <div className="mt-4 flex justify-end gap-3">
          <Button variant="ghost" onClick={onDismiss}>
            Skip for now
          </Button>
          <Button
            variant="secondary"
            loading={opening}
            loadingText="Opening..."
            onClick={openDownload}
          >
            Download Update
          </Button>
        </div>
      </ModalBody>
    </ModalFrame>
  );
}

function BootScreen() {
  return (
    <main className="crt-surface flex min-h-dvh items-center justify-center bg-hero-glow px-4">
      <Card className="pixel-frame animate-fade-in p-6 text-center">
        <img
          src={appLogo}
          alt="Noland logo"
          className="mx-auto mb-4 max-h-40 w-auto border border-[#3d426f]"
        />
        <p
          className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base"
          data-text="Loading Noland Connect..."
        >
          Loading Noland Connect...
        </p>
      </Card>
    </main>
  );
}

export function App() {
  const [windowLabel, setWindowLabel] = useState<string | null>(null);
  const [windowLabelResolved, setWindowLabelResolved] = useState(false);
  const [availableUpdate, setAvailableUpdate] = useState<AppUpdateInfo | null>(null);
  const initialize = useAppStore((state) => state.initialize);
  const bindEvents = useAppStore((state) => state.bindEvents);
  const loading = useAppStore((state) => state.loading);
  const error = useAppStore((state) => state.error);
  const clearError = useAppStore((state) => state.clearError);
  const blockingAction = useAppStore((state) => state.blockingAction);
  const isBlocking = useAppStore((state) => state.isBlocking);
  const cancelSharedStorageOperation = useAppStore(
    (state) => state.cancelSharedStorageOperation,
  );
  const appState = useAppStore((state) => state.appState);
  const busy = useAppStore((state) => state.busy);
  const saveVastApiKey = useAppStore((state) => state.saveVastApiKey);

  const savePlatformCredentials = useAppStore(
    (state) => state.savePlatformCredentials,
  );
  const saveIgdbCredentials = useAppStore((state) => state.saveIgdbCredentials);
  const saveServerPreferences = useAppStore(
    (state) => state.saveServerPreferences,
  );
  const saveMoonlightPreferences = useAppStore(
    (state) => state.saveMoonlightPreferences,
  );
  const saveSshCredentials = useAppStore((state) => state.saveSshCredentials);
  const regenerateEdid = useAppStore((state) => state.regenerateEdid);
  const storageProviders = useAppStore((state) => state.storageProviders);
  const sharedStorageProfiles = useAppStore(
    (state) => state.sharedStorageProfiles,
  );
  const sharedStorageTestResult = useAppStore(
    (state) => state.sharedStorageTestResult,
  );
  const loadStorageProviders = useAppStore(
    (state) => state.loadStorageProviders,
  );
  const connectStorageProvider = useAppStore(
    (state) => state.connectStorageProvider,
  );
  const testStorageConnection = useAppStore(
    (state) => state.testStorageConnection,
  );
  const loadSharedStorageProfiles = useAppStore(
    (state) => state.loadSharedStorageProfiles,
  );
  const setActiveStorageProfile = useAppStore(
    (state) => state.setActiveStorageProfile,
  );
  const disconnectStorageProfile = useAppStore(
    (state) => state.disconnectStorageProfile,
  );
  const oauthSessionId = useAppStore((state) => state.oauthSessionId);
  const beginOauthFlow = useAppStore((state) => state.beginOauthFlow);
  const completeOauthFlow = useAppStore((state) => state.completeOauthFlow);
  const provisioningStopRequested = useAppStore(
    (state) => state.provisioningStopRequested,
  );
  const stopProvisioningAfterCurrentStage = useAppStore(
    (state) => state.stopProvisioningAfterCurrentStage,
  );

  useEffect(() => {
    let cancelled = false;

    async function resolveWindowLabel() {
      if (!("__TAURI_INTERNALS__" in window)) {
        if (!cancelled) {
          setWindowLabel("main");
          setWindowLabelResolved(true);
        }
        return;
      }
      try {
        const currentWindow = getCurrentWindow();
        if (!cancelled) {
          setWindowLabel(currentWindow.label);
          setWindowLabelResolved(true);
        }
      } catch {
        if (!cancelled) {
          setWindowLabel("main");
          setWindowLabelResolved(true);
        }
      }
    }

    void resolveWindowLabel();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!windowLabelResolved || windowLabel === "moonlight-stream") {
      return;
    }
    void initialize();
    void bindEvents();
  }, [bindEvents, initialize, windowLabel, windowLabelResolved]);

  useEffect(() => {
    if (!windowLabelResolved || windowLabel === "moonlight-stream") {
      return;
    }

    let cancelled = false;
    async function checkForUpdate() {
      try {
        const update = await checkForGitHubUpdate();
        if (!cancelled && update) {
          setAvailableUpdate(update);
        }
      } catch (error) {
        console.warn("Update check failed", error);
      }
    }

    void checkForUpdate();
    return () => {
      cancelled = true;
    };
  }, [windowLabel, windowLabelResolved]);

  if (!windowLabelResolved) {
    return <BootScreen />;
  }

  if (windowLabel === "moonlight-stream") {
    return <StreamWindowScreen />;
  }

  if (loading) {
    return <BootScreen />;
  }

  return (
    <>
      {error && (
        <div className="fixed right-4 top-4 z-[100] max-w-md border-2 border-[#ff687d] bg-[#431a28] px-4 py-3 text-[1.2rem] text-[#ffd3dc] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#60243a]">
          <div className="flex items-start justify-between gap-3">
            <p className="break-words break-all">{error}</p>
            <button
              className="font-display text-[10px] uppercase tracking-[0.12em] shrink-0"
              onClick={clearError}
              type="button"
            >
              Dismiss
            </button>
          </div>
        </div>
      )}

      {availableUpdate && (
        <UpdateAvailableModal
          update={availableUpdate}
          onDismiss={() => setAvailableUpdate(null)}
        />
      )}

      {isBlocking && blockingAction && (
        <BlockingLoaderOverlay
          action={blockingAction}
          onCancel={
            blockingAction.instanceId != null
              ? () => {
                  void cancelSharedStorageOperation(blockingAction.instanceId as number);
                }
              : undefined
          }
          onStopProvisioning={
            blockingAction.key === "provisioning.flow"
              ? () => void stopProvisioningAfterCurrentStage()
              : undefined
          }
          stopRequested={provisioningStopRequested}
        />
      )}

      <HashRouter>
        <Routes>
          <Route path="/" element={<RootRoute />} />
          <Route path="/provisioning" element={<ProvisioningRoute />} />
          <Route
            path="/settings"
            element={
              appState?.onboardingCompleted ? (
                <SettingsScreen
                  appState={appState}
                  busy={busy}
                  storageProviders={storageProviders}
                  sharedStorageProfiles={sharedStorageProfiles}
                  sharedStorageTestResult={sharedStorageTestResult}
                  onLoadStorageProviders={loadStorageProviders}
                  onConnectStorageProvider={connectStorageProvider}
                  onTestStorageConnection={testStorageConnection}
                  onLoadSharedStorageProfiles={loadSharedStorageProfiles}
                  onSetActiveStorageProfile={setActiveStorageProfile}
                  onDisconnectStorageProfile={disconnectStorageProfile}
                  oauthSessionId={oauthSessionId}
                  onBeginOauthFlow={beginOauthFlow}
                  onCompleteOauthFlow={completeOauthFlow}
                  onSaveApiKey={saveVastApiKey}
                  onSavePlatformCredentials={savePlatformCredentials}
                  onSaveIgdbCredentials={saveIgdbCredentials}
                  onSaveServerPreferences={saveServerPreferences}
                  onSaveMoonlightPreferences={saveMoonlightPreferences}
                  onSaveSshCredentials={saveSshCredentials}
                  onRegenerateEdid={regenerateEdid}
                />
              ) : (
                <Navigate to="/" replace />
              )
            }
          />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </HashRouter>
    </>
  );
}
