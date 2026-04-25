import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ArcadeSoundToggle } from "../../components/ui/ArcadeSoundToggle";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { HudBar } from "../../components/ui/HudBar";
import { SpriteIcon } from "../../components/ui/SpriteIcon";
import { StatusPill } from "../../components/ui/StatusPill";
import { resolveMoonlightDownloadUrl } from "../../lib/backend";
import type { OfferCandidate, PersistedAppState, RentedInstanceSummary, ServerPreferences, SunshineSettingsResponse, BundleIndex, RestoreDryRunResult, RestoreJob } from "../../lib/types";
import { ServerPickerModal } from "../servers/ServerPickerModal";
import { InstanceCardActions } from "../shared-storage-manager/InstanceCardActions";
import { SunshineSettingsPanel } from "../shared-storage-manager/SunshineSettingsPanel";
import { RestoreBundlesPanel } from "../restore/RestoreBundlesPanel";

interface Props {
  appState: PersistedAppState;
  offers: OfferCandidate[];
  rentedInstances: RentedInstanceSummary[];
  searchingOffers: boolean;
  offersPage: number;
  offersHasNextPage: boolean;
  busy: boolean;
  instanceActionRunning: boolean;
  sunshineSettings: SunshineSettingsResponse | null;
  onSearchOffers: (page?: number) => Promise<void>;
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
  onStartPlayExisting: (instanceId: number) => Promise<void>;
  onSelectOffer: (offerId: number, storageGb: number) => Promise<void>;
  onStartPlay: () => Promise<void>;
  onSaveServerPreferences: (payload: Partial<ServerPreferences>) => Promise<void>;
  onLoadSunshineSettings: (instanceId: number) => Promise<void>;
  onSaveSunshineSettings: (instanceId: number, settings: Record<string, unknown>) => Promise<void>;
  onReconnectWireguard: (instanceId: number) => Promise<string | null>;
  onPauseInstance: (instanceId: number) => Promise<void>;
  onDestroyInstance: (instanceId: number) => Promise<void>;
  bundleIndex: BundleIndex | null;
  restoreJob: RestoreJob | null;
  onGenerateBundleIndex: () => Promise<void>;
  onLoadRestoreBundles: (instanceId: number) => Promise<void>;
  onDryRunRestore: (instanceId: number, bundleId: string, folderIds: string[], mode: string) => Promise<RestoreDryRunResult | null>;
  onRestoreBundle: (instanceId: number, bundleId: string, folderIds: string[], mode: string) => Promise<RestoreJob | null>;
  onPollRestoreJob: (jobId: string) => Promise<void>;
}

const placeholders = [
  "Steam Library",
  "Cloud Presets",
  "Latency Tuning",
  "Controller Profiles",
  "Scene Presets"
];

export function DashboardScreen({
  appState,
  offers,
  rentedInstances,
  searchingOffers,
  offersPage,
  offersHasNextPage,
  busy,
  instanceActionRunning,
  sunshineSettings,
  onSearchOffers,
  onNextOffersPage,
  onPreviousOffersPage,
  onManualLocationSave,
  onLoadRentedInstances,
  onStartPlayExisting,
  onSelectOffer,
  onStartPlay,
  onSaveServerPreferences,
  onLoadSunshineSettings,
  onSaveSunshineSettings,
  onReconnectWireguard,
  onPauseInstance,
  onDestroyInstance,
  bundleIndex,
  restoreJob,
  onGenerateBundleIndex,
  onLoadRestoreBundles,
  onDryRunRestore,
  onRestoreBundle,
  onPollRestoreJob
}: Props) {
  const [pickerOpen, setPickerOpen] = useState(false);
  const [settingsInstanceId, setSettingsInstanceId] = useState<number | null>(null);
  const [restoreInstanceId, setRestoreInstanceId] = useState<number | null>(null);
  const navigate = useNavigate();

  async function handleMoonlightDownload() {
    const downloadUrl = await resolveMoonlightDownloadUrl();
    try {
      await openUrl(downloadUrl);
    } catch {
      window.open(downloadUrl, "_blank", "noopener,noreferrer");
    }
  }

  async function handlePlay() {
    await onStartPlay();
    navigate("/provisioning");
  }

  async function handlePlayExisting(instanceId: number) {
    await onStartPlayExisting(instanceId);
    navigate("/provisioning");
  }

  async function handleOpenSettings(instanceId: number) {
    setSettingsInstanceId(instanceId);
    await onLoadSunshineSettings(instanceId);
  }

  async function handleSaveSunshineSettings(settings: Record<string, unknown>) {
    if (settingsInstanceId !== null) {
      await onSaveSunshineSettings(settingsInstanceId, settings);
    }
  }

  function handleCloseSunshineSettings() {
    setSettingsInstanceId(null);
  }

  async function handleReconnect(instanceId: number) {
    await onReconnectWireguard(instanceId);
  }

  async function handlePause(instanceId: number) {
    await onPauseInstance(instanceId);
    await onLoadRentedInstances();
  }

  async function handleDestroy(instanceId: number) {
    await onDestroyInstance(instanceId);
    await onLoadRentedInstances();
  }

  function handleOpenRestore(instanceId: number) {
    setRestoreInstanceId(instanceId);
  }

  function handleCloseRestore() {
    setRestoreInstanceId(null);
  }

  return (
    <main className="crt-surface min-h-screen bg-hero-glow px-4 pb-8 pt-6 md:px-8">
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
            <Button variant="ghost" onClick={() => navigate("/settings")}>Settings</Button>
            <Button variant="secondary" onClick={() => setPickerOpen(true)}>
              Select Server
            </Button>
            <ArcadeSoundToggle />
          </div>
        </header>

        <section className="grid gap-4 md:grid-cols-2">
          <Card interactive onClick={handleMoonlightDownload} className="pixel-frame min-h-40">
            <div className="flex items-center justify-between">
              <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">Panel 1</p>
              <SpriteIcon icon="moonlight" />
            </div>
            <h2 className="mt-3 font-display text-base text-neon-cyan md:text-lg">Download Moonlight</h2>
            <p className="mt-2 max-w-md text-[1.32rem] leading-[1.1] text-[#bfd3ee]">
              Open the official installer for your OS and complete setup. Noland Connect updates
              Moonlight settings after provisioning.
            </p>
          </Card>

          <Card interactive onClick={() => setPickerOpen(true)} className="pixel-frame min-h-40">
            <div className="flex items-center justify-between">
              <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">Panel 2</p>
              <SpriteIcon icon="server" />
            </div>
            <h2 className="mt-3 font-display text-base text-neon-cyan md:text-lg">Set Server</h2>
            <p className="mt-2 text-[1.32rem] leading-[1.1] text-[#bfd3ee]">
              Discover nearby GPU offers by reliability, distance, and price. Adjust storage before
              launch.
            </p>
          </Card>
        </section>

        <section className="grid gap-4 sm:grid-cols-2 xl:grid-cols-5">
          {placeholders.map((item) => (
            <Card key={item} className="min-h-28 animate-slide-up">
              <p className="font-display text-[10px] uppercase tracking-[0.12em] text-[#93b7d6]">Cartridge</p>
              <h3 className="mt-2 font-display text-[11px] leading-[1.4] text-white">{item}</h3>
              <p className="mt-2 text-[1.2rem] leading-none text-[#98adc9]">Coming soon</p>
            </Card>
          ))}
        </section>

        <section>
          <Card className="pixel-frame">
            <div className="mb-3 flex items-center justify-between">
              <h3 className="font-display text-sm uppercase tracking-[0.12em] text-white">Rented Servers</h3>
              <Button variant="secondary" onClick={onLoadRentedInstances} disabled={busy}>
                Refresh Rented
              </Button>
            </div>

            {rentedInstances.length === 0 ? (
              <p className="text-[1.25rem] text-[#bfd3ee]">
                No active rented servers found for this account.
              </p>
            ) : (
              <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
                {rentedInstances.map((instance) => (
                  <Card key={instance.instanceId} className="border-2 border-[#3a4068]">
                    <div className="flex items-center justify-between gap-2">
                      <h4 className="font-display text-[11px] text-white">{instance.label}</h4>
                      <StatusPill
                        state={instance.status.toLowerCase().includes("run") ? "Ready" : "WaitingForInstance"}
                      />
                    </div>
                    <div className="mt-2 grid grid-cols-2 gap-2 text-[1.15rem] text-[#bfd3ee]">
                      <p>ID: {instance.instanceId}</p>
                      <p>Status: {instance.status}</p>
                      <p>GPU: {instance.gpuName}</p>
                      <p>SSH: {instance.sshHost || "pending"}</p>
                    </div>
                    <div className="mt-3">
                      <InstanceCardActions
                        instance={instance}
                        busy={busy}
                        instanceActionRunning={instanceActionRunning}
                        onPlay={handlePlayExisting}
                        onSettings={handleOpenSettings}
                        onRestore={handleOpenRestore}
                        onReconnect={handleReconnect}
                        onPause={handlePause}
                        onDestroy={handleDestroy}
                      />
                    </div>
                  </Card>
                ))}
              </div>
            )}
          </Card>
        </section>

        <section className="grid gap-4 lg:grid-cols-[1.5fr_1fr]">
          <Card className="pixel-frame min-h-44">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h3 className="font-display text-sm uppercase tracking-[0.12em] text-white">Selected Server</h3>
              <StatusPill state={appState.orchestrationState} />
            </div>

            {appState.selectedOffer ? (
              <div className="mt-4 grid grid-cols-2 gap-3 text-[1.25rem] leading-[1.05] text-[#d9efff] md:grid-cols-4">
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">Host</p>
                  <p>{appState.selectedOffer.hostLabel}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">Location</p>
                  <p>{appState.selectedOffer.locationLabel}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">GPU</p>
                  <p>{appState.selectedOffer.gpuName}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">Price/hour</p>
                  <p>${appState.selectedOffer.hourlyPrice.toFixed(3)}</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">Distance</p>
                  <p>{appState.selectedOffer.estimatedDistanceKm.toFixed(0)} km</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">Reliability</p>
                  <p>{(appState.selectedOffer.reliability * 100).toFixed(1)}%</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">Storage</p>
                  <p>{appState.serverPreferences.storageGb} GB</p>
                </div>
                <div>
                  <p className="font-display text-[10px] uppercase text-[#8db7d8]">Template</p>
                  <p className="truncate">{appState.serverPreferences.templateHash}</p>
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
                  value={Math.max(0, 1000 - appState.selectedOffer.estimatedDistanceKm)}
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
                <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">Action</p>
                <SpriteIcon icon="play" />
              </div>
              <h3 className="mt-2 font-display text-lg text-neon-cyan">Play</h3>
              <p className="mt-2 text-[1.32rem] leading-[1.1] text-[#bfd3ee]">
                Creates the instance, waits for readiness, runs provisioning, and opens pairing
                guidance.
              </p>
            </div>
            <Button
              className="h-12 w-full justify-center text-[12px]"
              disabled={busy || !appState.selectedOffer}
              onClick={handlePlay}
            >
              {busy ? "Starting session..." : "Play"}
            </Button>
          </Card>
        </section>
      </div>

      {settingsInstanceId !== null && (
        <SunshineSettingsPanel
          settings={sunshineSettings}
          busy={instanceActionRunning}
          onSave={handleSaveSunshineSettings}
          onClose={handleCloseSunshineSettings}
        />
      )}

      {restoreInstanceId !== null && (
        <RestoreBundlesPanel
          bundleIndex={bundleIndex}
          restoreJob={restoreJob}
          instanceId={restoreInstanceId}
          busy={busy}
          instanceActionRunning={instanceActionRunning}
          onLoadBundles={onLoadRestoreBundles}
          onGenerateIndex={onGenerateBundleIndex}
          onDryRun={onDryRunRestore}
          onRestore={onRestoreBundle}
          onPollJob={onPollRestoreJob}
          onClose={handleCloseRestore}
        />
      )}

      <ServerPickerModal
        open={pickerOpen}
        onClose={() => setPickerOpen(false)}
        offers={offers}
        selectedOfferId={appState.selectedOffer?.id ?? null}
        location={appState.location}
        serverPreferences={appState.serverPreferences}
        storageGb={appState.serverPreferences.storageGb}
        searchingOffers={searchingOffers}
        offersPage={offersPage}
        offersHasNextPage={offersHasNextPage}
        busy={busy}
        onSearchOffers={onSearchOffers}
        onNextPage={onNextOffersPage}
        onPreviousPage={onPreviousOffersPage}
        onManualLocationSave={onManualLocationSave}
        onSelectOffer={onSelectOffer}
        onUpdateServerPreferences={onSaveServerPreferences}
      />
    </main>
  );
}
