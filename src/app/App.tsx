import { useEffect } from "react";
import { HashRouter, Navigate, Route, Routes } from "react-router-dom";
import { Card } from "../components/ui/Card";
import { DashboardScreen } from "../features/dashboard/DashboardScreen";
import { OnboardingScreen } from "../features/onboarding/OnboardingScreen";
import { ProvisioningScreen } from "../features/provisioning/ProvisioningScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { useAppStore } from "../store/appStore";
import appLogo from "../public/noland.png";

function RootRoute() {
  const appState = useAppStore((state) => state.appState);
  const busy = useAppStore((state) => state.busy);
  const offers = useAppStore((state) => state.offers);
  const rentedInstances = useAppStore((state) => state.rentedInstances);
  const searchingOffers = useAppStore((state) => state.searching);
  const offersPage = useAppStore((state) => state.offersPage);
  const offersHasNextPage = useAppStore((state) => state.offersHasNextPage);
  const instanceActionRunning = useAppStore((state) => state.instanceActionRunning);
  const sunshineSettings = useAppStore((state) => state.sunshineSettings);
  const runOnboarding = useAppStore((state) => state.runOnboarding);
  const discoverOffers = useAppStore((state) => state.discoverOffers);
  const nextOffersPage = useAppStore((state) => state.nextOffersPage);
  const previousOffersPage = useAppStore((state) => state.previousOffersPage);
  const saveManualLocation = useAppStore((state) => state.saveManualLocation);
  const loadRentedInstances = useAppStore((state) => state.loadRentedInstances);
  const chooseOffer = useAppStore((state) => state.chooseOffer);
  const startPlay = useAppStore((state) => state.startPlay);
  const startPlayExisting = useAppStore((state) => state.startPlayExisting);
  const saveServerPreferences = useAppStore((state) => state.saveServerPreferences);
  const loadSunshineSettings = useAppStore((state) => state.loadSunshineSettings);
  const saveSunshineSettings = useAppStore((state) => state.saveSunshineSettings);
  const reconnectWireguard = useAppStore((state) => state.reconnectWireguard);
  const rebootInstanceServices = useAppStore((state) => state.rebootInstanceServices);
  const pauseInstance = useAppStore((state) => state.pauseInstance);
  const destroyInstance = useAppStore((state) => state.destroyInstance);
  const syncInstanceStorage = useAppStore((state) => state.syncInstanceStorage);
  const listSyncableStorageObjects = useAppStore((state) => state.listSyncableStorageObjects);
  const saveInstanceStorageSelected = useAppStore((state) => state.saveInstanceStorageSelected);
  const listExportableStorageObjects = useAppStore((state) => state.listExportableStorageObjects);

  if (!appState) {
    return null;
  }

  if (!appState.onboardingCompleted) {
    return <OnboardingScreen busy={busy} onSubmit={runOnboarding} />;
  }

  return (
    <DashboardScreen
      appState={appState}
      offers={offers}
      rentedInstances={rentedInstances}
      searchingOffers={searchingOffers}
      offersPage={offersPage}
      offersHasNextPage={offersHasNextPage}
      busy={busy}
      instanceActionRunning={instanceActionRunning}
      sunshineSettings={sunshineSettings}
      onSearchOffers={discoverOffers}
      onNextOffersPage={nextOffersPage}
      onPreviousOffersPage={previousOffersPage}
      onManualLocationSave={saveManualLocation}
      onLoadRentedInstances={loadRentedInstances}
      onStartPlayExisting={startPlayExisting}
      onSelectOffer={chooseOffer}
      onStartPlay={startPlay}
      onSaveServerPreferences={saveServerPreferences}
      onLoadSunshineSettings={loadSunshineSettings}
      onSaveSunshineSettings={saveSunshineSettings}
      onReconnectWireguard={reconnectWireguard}
      onRebootInstanceServices={rebootInstanceServices}
      onPauseInstance={pauseInstance}
      onDestroyInstance={destroyInstance}
      onSaveInstanceStorageSelected={saveInstanceStorageSelected}
      onSyncInstanceStorage={syncInstanceStorage}
      onListSyncableStorageObjects={listSyncableStorageObjects}
      onListExportableStorageObjects={listExportableStorageObjects}
    />
  );
}

function ProvisioningRoute() {
  const appState = useAppStore((state) => state.appState);
  const logs = useAppStore((state) => state.logs);
  const busy = useAppStore((state) => state.busy);
  const submitPin = useAppStore((state) => state.submitPin);
  const skipPairing = useAppStore((state) => state.skipPairing);
  const setupLocalWireguardClient = useAppStore((state) => state.setupLocalWireguardClient);

  if (!appState) {
    return null;
  }

  return (
    <ProvisioningScreen
      appState={appState}
      logs={logs}
      busy={busy}
      onSubmitPin={submitPin}
      onSkipPairing={skipPairing}
      onSetupWireguardClient={setupLocalWireguardClient}
    />
  );
}

function BootScreen() {
  return (
    <main className="crt-surface flex min-h-screen items-center justify-center bg-hero-glow px-4">
      <Card className="pixel-frame animate-fade-in p-6 text-center">
        <img
          src={appLogo}
          alt="Noland logo"
          className="mx-auto mb-4 max-h-40 w-auto border border-[#3d426f]"
        />
        <p className="pixel-heading glitch-title font-display text-sm text-neon-cyan md:text-base" data-text="Loading Noland Connect...">
          Loading Noland Connect...
        </p>
      </Card>
    </main>
  );
}

export function App() {
  const initialize = useAppStore((state) => state.initialize);
  const bindEvents = useAppStore((state) => state.bindEvents);
  const loading = useAppStore((state) => state.loading);
  const error = useAppStore((state) => state.error);
  const clearError = useAppStore((state) => state.clearError);
  const appState = useAppStore((state) => state.appState);
  const busy = useAppStore((state) => state.busy);
  const saveVastApiKey = useAppStore((state) => state.saveVastApiKey);
  const savePlatformCredentials = useAppStore((state) => state.savePlatformCredentials);
  const saveServerPreferences = useAppStore((state) => state.saveServerPreferences);
  const saveMoonlightPreferences = useAppStore((state) => state.saveMoonlightPreferences);
  const saveSshCredentials = useAppStore((state) => state.saveSshCredentials);
  const sharedStorageSettings = useAppStore((state) => state.sharedStorageSettings);
  const saveSharedStorageSettings = useAppStore((state) => state.saveSharedStorageSettings);
  const testSharedStorageConfig = useAppStore((state) => state.testSharedStorageConfig);
  const loadSharedStorageSettings = useAppStore((state) => state.loadSharedStorageSettings);

  useEffect(() => {
    void initialize();
    void bindEvents();
  }, [bindEvents, initialize]);

  if (loading) {
    return <BootScreen />;
  }

  return (
    <>
      {error && (
        <div className="fixed right-4 top-4 z-[100] max-w-md border-2 border-[#ff687d] bg-[#431a28] px-4 py-3 text-[1.2rem] text-[#ffd3dc] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#60243a]">
          <div className="flex items-start justify-between gap-3">
            <p>{error}</p>
            <button
              className="font-display text-[10px] uppercase tracking-[0.12em]"
              onClick={clearError}
              type="button"
            >
              Dismiss
            </button>
          </div>
        </div>
      )}

      <HashRouter>
        <Routes>
          <Route path="/" element={<RootRoute />} />
          <Route path="/provisioning" element={<ProvisioningRoute />} />
          <Route
            path="/settings"
            element={
              appState ? (
                <SettingsScreen
                  appState={appState}
                  busy={busy}
                  sharedStorageSettings={sharedStorageSettings}
                  onSaveApiKey={saveVastApiKey}
                  onSavePlatformCredentials={savePlatformCredentials}
                  onSaveServerPreferences={saveServerPreferences}
                  onSaveMoonlightPreferences={saveMoonlightPreferences}
                  onSaveSshCredentials={saveSshCredentials}
                  onSaveSharedStorageSettings={saveSharedStorageSettings}
                  onTestSharedStorageConfig={testSharedStorageConfig}
                  onLoadSharedStorageSettings={loadSharedStorageSettings}
                />
              ) : null
            }
          />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </HashRouter>
    </>
  );
}
