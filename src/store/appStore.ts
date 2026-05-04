import { create } from "zustand";
import {
  completeOnboarding,
  getAppState,
  getRentedInstances,
  getProvisioningLogs,
  searchOffers,
  selectOffer,
  setManualLocation,
  setupWireguardClient,
  startPlayExistingInstance,
  startPlayFlow,
  submitPairingPin,
  skipPairingAndContinue,
  subscribeProvisioningEvents,
  updatePlatformCredentials,
  updateMoonlightPreferences,
  updateServerPreferences,
  updateSshCredentials,
  updateVastApiKey,
  getSharedStorageSettings,
  saveSharedStorageSettings,
  testSharedStorageConfig,
  triggerInstanceBackup,
  triggerInstanceBackupFor,
  getInstanceBackupStatus,
  setupInstanceBackupSchedule,
  removeInstanceBackupSchedule,
  getInstanceSunshineSettings,
  updateInstanceSunshineSettings,
  reconnectInstanceWireguard,
  rebootInstanceServices,
  pauseInstance,
  destroyInstance,
  generateBundleIndex,
  getInstanceRestoreBundles,
  dryRunRestore,
  restoreBundle,
  getRestoreJob,
  getInstanceMicConfig,
  updateInstanceMicSettings,
  enableInstanceMic,
  disableInstanceMic,
  reconnectInstanceMic,
  recreateInstanceMicDevice,
  getInstanceMicStatus,
  syncInstanceFromSharedStorage,
  listInstanceSharedStorageObjects,
  syncInstanceFromSharedStorageSelected
} from "../lib/backend";
import type {
  ManualLocationInput,
  MoonlightPreferences,
  OfferCandidate,
  OnboardingPayload,
  PlatformCredentialsUpdate,
  PersistedAppState,
  ProvisioningEvent,
  RentedInstanceSummary,
  ServerPreferencesUpdate,
  SshCredentialsUpdate,
  SharedStorageSettingsUpdate,
  SharedStorageSettingsResponse,
  BackupStatusResponse,
  SharedStorageInstanceStatus,
  SharedStorageObjectEntry,
  SunshineSettingsResponse,
  BundleIndex,
  RestoreDryRunResult,
  RestoreJob,
  RestoreRequest,
  InstanceMicConfig,
  InstanceMicRuntimeStatus,
  MicSessionResponse,
  MicSettingsUpdate,
  MicQualityProfile
} from "../lib/types";

interface AppStore {
  appState: PersistedAppState | null;
  offers: OfferCandidate[];
  rentedInstances: RentedInstanceSummary[];
  logs: ProvisioningEvent[];
  loading: boolean;
  searching: boolean;
  offersPage: number;
  offersPageSize: number;
  offersHasNextPage: boolean;
  busy: boolean;
  serverPickerOpen: boolean;
  error: string | null;
  _eventsBound: boolean;
  initialize: () => Promise<void>;
  bindEvents: () => Promise<void>;
  setServerPickerOpen: (open: boolean) => void;
  runOnboarding: (payload: OnboardingPayload) => Promise<void>;
  saveManualLocation: (payload: ManualLocationInput) => Promise<void>;
  discoverOffers: (page?: number) => Promise<void>;
  nextOffersPage: () => Promise<void>;
  previousOffersPage: () => Promise<void>;
  chooseOffer: (offerId: number, storageGb: number) => Promise<void>;
  startPlay: () => Promise<void>;
  startPlayExisting: (instanceId: number) => Promise<void>;
  loadRentedInstances: () => Promise<void>;
  saveVastApiKey: (apiKey: string) => Promise<void>;
  savePlatformCredentials: (payload: PlatformCredentialsUpdate) => Promise<void>;
  saveServerPreferences: (payload: Partial<ServerPreferencesUpdate>) => Promise<void>;
  saveMoonlightPreferences: (payload: MoonlightPreferences) => Promise<void>;
  saveSshCredentials: (payload: SshCredentialsUpdate) => Promise<void>;
  submitPin: (pin: string) => Promise<void>;
  skipPairing: () => Promise<void>;
  setupLocalWireguardClient: () => Promise<void>;
  sharedStorageSettings: SharedStorageSettingsResponse | null;
  backupStatus: BackupStatusResponse | null;
  instanceBackupStatus: SharedStorageInstanceStatus | null;
  loadSharedStorageSettings: () => Promise<void>;
  saveSharedStorageSettings: (payload: SharedStorageSettingsUpdate) => Promise<void>;
  testSharedStorageConfig: () => Promise<string | null>;
  triggerBackup: () => Promise<void>;
  triggerBackupForInstance: (instanceId: number) => Promise<void>;
  syncInstanceStorage: (instanceId: number, selectedPaths?: string[]) => Promise<string | null>;
  listSyncableStorageObjects: (instanceId: number) => Promise<SharedStorageObjectEntry[] | null>;
  loadBackupStatus: () => Promise<void>;
  loadInstanceBackupStatus: () => Promise<void>;
  setupBackupSchedule: () => Promise<string | null>;
  removeBackupSchedule: () => Promise<string | null>;
  sunshineSettings: SunshineSettingsResponse | null;
  instanceActionRunning: boolean;
  loadSunshineSettings: (instanceId: number) => Promise<void>;
  saveSunshineSettings: (instanceId: number, settings: Record<string, unknown>) => Promise<void>;
  reconnectWireguard: (instanceId: number) => Promise<string | null>;
  rebootInstanceServices: (instanceId: number) => Promise<string | null>;
  pauseInstance: (instanceId: number) => Promise<void>;
  destroyInstance: (instanceId: number) => Promise<void>;
  bundleIndex: BundleIndex | null;
  restoreJob: RestoreJob | null;
  generateBundleIndex: () => Promise<void>;
  loadRestoreBundles: (instanceId: number) => Promise<void>;
  runDryRunRestore: (instanceId: number, payload: RestoreRequest) => Promise<RestoreDryRunResult | null>;
  runRestoreBundle: (instanceId: number, payload: RestoreRequest) => Promise<RestoreJob | null>;
  pollRestoreJob: (jobId: string) => Promise<void>;
  micConfig: InstanceMicConfig | null;
  micStatus: InstanceMicRuntimeStatus | null;
  micSession: MicSessionResponse | null;
  loadMicConfig: (instanceId: number) => Promise<void>;
  updateMicSettings: (instanceId: number, payload: MicSettingsUpdate) => Promise<void>;
  enableMic: (instanceId: number, qualityProfile?: MicQualityProfile) => Promise<MicSessionResponse | null>;
  disableMic: (instanceId: number) => Promise<void>;
  reconnectMic: (instanceId: number) => Promise<MicSessionResponse | null>;
  recreateMicDevice: (instanceId: number) => Promise<void>;
  loadMicStatus: (instanceId: number) => Promise<void>;
  clearError: () => void;
}

function mapError(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }

  if (typeof error === "object" && error !== null) {
    const details = Reflect.get(error, "details");
    if (typeof details === "string" && details.trim().length > 0) {
      return details;
    }

    const message = Reflect.get(error, "message");
    if (typeof message === "string") {
      return message;
    }
  }

  return "Something went wrong. Check logs and try again.";
}

export const useAppStore = create<AppStore>((set, get) => ({
  appState: null,
  offers: [],
  rentedInstances: [],
  logs: [],
  loading: true,
  searching: false,
  offersPage: 1,
  offersPageSize: 24,
  offersHasNextPage: false,
  busy: false,
  serverPickerOpen: false,
  error: null,
  _eventsBound: false,
  sharedStorageSettings: null,
  backupStatus: null,
  instanceBackupStatus: null,
  sunshineSettings: null,
  instanceActionRunning: false,
  bundleIndex: null,
  restoreJob: null,
  micConfig: null,
  micStatus: null,
  micSession: null,

  initialize: async () => {
    set({ loading: true, error: null });
    try {
      const [appState, logs] = await Promise.all([getAppState(), getProvisioningLogs()]);
      let rentedInstances: RentedInstanceSummary[] = [];
      if (appState.onboardingCompleted && appState.credentials.vastApiKey.trim().length > 0) {
        rentedInstances = await getRentedInstances();
      }

      set({ appState, logs, rentedInstances, loading: false });
    } catch (error) {
      set({ loading: false, error: mapError(error) });
    }
  },

  bindEvents: async () => {
    if (get()._eventsBound) {
      return;
    }

    await subscribeProvisioningEvents((event) => {
      set((state) => {
        const nextLogs = [event, ...state.logs].slice(0, 500);
        const nextState = state.appState
          ? {
              ...state.appState,
              orchestrationState: event.state,
              lastError: event.isError ? event.message : state.appState.lastError
            }
          : state.appState;

        return {
          logs: nextLogs,
          appState: nextState,
          error: event.isError ? event.message : state.error
        };
      });
    });

    set({ _eventsBound: true });
  },

  setServerPickerOpen: (serverPickerOpen) => set({ serverPickerOpen }),

  runOnboarding: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await completeOnboarding(payload);
      const rentedInstances = await getRentedInstances();
      set({ appState, rentedInstances, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveManualLocation: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await setManualLocation(payload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  discoverOffers: async (page) => {
    set({ searching: true, error: null });
    try {
      const state = get();
      const targetPage = Math.max(1, page ?? state.offersPage);
      const offers = await searchOffers(targetPage, state.offersPageSize);
      set({
        offers,
        offersPage: targetPage,
        offersHasNextPage: offers.length === state.offersPageSize,
        searching: false
      });
    } catch (error) {
      set({ searching: false, error: mapError(error) });
    }
  },

  nextOffersPage: async () => {
    const state = get();
    if (state.searching || !state.offersHasNextPage) {
      return;
    }

    await state.discoverOffers(state.offersPage + 1);
  },

  previousOffersPage: async () => {
    const state = get();
    if (state.searching || state.offersPage <= 1) {
      return;
    }

    await state.discoverOffers(state.offersPage - 1);
  },

  chooseOffer: async (offerId, storageGb) => {
    set({ busy: true, error: null });
    try {
      const appState = await selectOffer(offerId, storageGb);
      set({ appState, busy: false, serverPickerOpen: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  startPlay: async () => {
    set({ busy: true, error: null });
    try {
      await startPlayFlow();
      const appState = await getAppState();
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  startPlayExisting: async (instanceId) => {
    set({ busy: true, error: null });
    try {
      await startPlayExistingInstance(instanceId);
      const appState = await getAppState();
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  loadRentedInstances: async () => {
    set({ busy: true, error: null });
    try {
      const rentedInstances = await getRentedInstances();
      set({ rentedInstances, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveVastApiKey: async (apiKey) => {
    set({ busy: true, error: null });
    try {
      const appState = await updateVastApiKey(apiKey);
      const rentedInstances = await getRentedInstances();
      set({ appState, rentedInstances, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  savePlatformCredentials: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await updatePlatformCredentials(payload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveServerPreferences: async (payload) => {
    set({ busy: true, error: null });
    try {
      const state = get();
      const current = state.appState?.serverPreferences;
      if (!current) {
        set({ busy: false, error: "App state not initialized" });
        return;
      }

      const fullPayload: ServerPreferencesUpdate = {
        minReliability: payload.minReliability ?? current.minReliability,
        storageGb: payload.storageGb ?? current.storageGb,
        templateHash: payload.templateHash ?? current.templateHash,
        maxHourlyPrice: payload.maxHourlyPrice ?? current.maxHourlyPrice,
        minHourlyPrice: payload.minHourlyPrice ?? current.minHourlyPrice,
        requireVerified: payload.requireVerified ?? current.requireVerified,
        requireDatacenter: payload.requireDatacenter ?? current.requireDatacenter,
        includeOnDemand: payload.includeOnDemand ?? current.includeOnDemand,
        includeInterruptible: payload.includeInterruptible ?? current.includeInterruptible,
        includeReserved: payload.includeReserved ?? current.includeReserved,
        requireStaticIp: payload.requireStaticIp ?? current.requireStaticIp,
        requireAvx: payload.requireAvx ?? current.requireAvx,
        minGpuCount: payload.minGpuCount ?? current.minGpuCount,
        minGpuRamGb: payload.minGpuRamGb ?? current.minGpuRamGb,
        minCpuCores: payload.minCpuCores ?? current.minCpuCores,
        minInetDownMbps: payload.minInetDownMbps ?? current.minInetDownMbps,
        minInetUpMbps: payload.minInetUpMbps ?? current.minInetUpMbps,
        geolocationCountryCode:
          payload.geolocationCountryCode ?? current.geolocationCountryCode,
      };

      const appState = await updateServerPreferences(fullPayload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveMoonlightPreferences: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await updateMoonlightPreferences(payload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveSshCredentials: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await updateSshCredentials(payload);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  submitPin: async (pin) => {
    set({ busy: true, error: null });
    try {
      const appState = await submitPairingPin(pin);
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  skipPairing: async () => {
    set({ busy: true, error: null });
    try {
      const appState = await skipPairingAndContinue();
      set({ appState, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  setupLocalWireguardClient: async () => {
    set({ busy: true, error: null });
    try {
      await setupWireguardClient();
      set({ busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  loadSharedStorageSettings: async () => {
    set({ busy: true, error: null });
    try {
      const settings = await getSharedStorageSettings();
      set({ sharedStorageSettings: settings, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  saveSharedStorageSettings: async (payload) => {
    set({ busy: true, error: null });
    try {
      const appState = await saveSharedStorageSettings(payload);
      const settings = await getSharedStorageSettings();
      set({ appState, sharedStorageSettings: settings, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  testSharedStorageConfig: async () => {
    set({ busy: true, error: null });
    try {
      const result = await testSharedStorageConfig();
      set({ busy: false });
      return result;
    } catch (error) {
      set({ busy: false, error: mapError(error) });
      return null;
    }
  },

  triggerBackup: async () => {
    set({ busy: true, error: null });
    try {
      const status = await triggerInstanceBackup();

      // Refresh bundle index immediately so restore UI reflects selectable bundles
      // from the latest backup without requiring a manual "Generate Index" action.
      const currentState = get().appState;
      const activeInstanceId = currentState?.instance.instanceId;
      if (activeInstanceId) {
        try {
          const index = await getInstanceRestoreBundles(activeInstanceId);
          set({ backupStatus: status, bundleIndex: index, busy: false });
          return;
        } catch {
          // Backup succeeded even if index retrieval fails; keep success status.
        }
      }

      set({ backupStatus: status, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  triggerBackupForInstance: async (instanceId) => {
    set({ busy: true, error: null });
    try {
      const status = await triggerInstanceBackupFor(instanceId);
      set({ backupStatus: status, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  syncInstanceStorage: async (instanceId, selectedPaths) => {
    set({ busy: true, error: null });
    try {
      console.info("[shared-storage] sync start", {
        instanceId,
        selectedCount: selectedPaths?.length ?? 0
      });
      const message = selectedPaths && selectedPaths.length > 0
        ? await syncInstanceFromSharedStorageSelected(instanceId, selectedPaths)
        : await syncInstanceFromSharedStorage(instanceId);
      console.info("[shared-storage] sync complete", { instanceId, message });
      set({ busy: false });
      return message;
    } catch (error) {
      console.error("[shared-storage] sync failed", { instanceId, error });
      set({ busy: false, error: mapError(error) });
      return null;
    }
  },

  listSyncableStorageObjects: async (instanceId) => {
    set({ error: null });
    try {
      console.info("[shared-storage] listing remote objects start", { instanceId });
      const entries = await listInstanceSharedStorageObjects(instanceId);
      console.info("[shared-storage] listing remote objects complete", {
        instanceId,
        count: entries.length
      });
      return entries;
    } catch (error) {
      console.error("[shared-storage] listing remote objects failed", { instanceId, error });
      set({ error: mapError(error) });
      return null;
    }
  },

  loadBackupStatus: async () => {
    set({ busy: true, error: null });
    try {
      const status = await getInstanceBackupStatus();
      set({
        backupStatus: {
          lastBackupStartedAt: status.lastBackupStartedAt,
          lastBackupFinishedAt: status.lastBackupFinishedAt,
          lastBackupStatus: status.lastBackupStatus,
          lastBackupError: status.lastBackupError
        },
        busy: false
      });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  loadInstanceBackupStatus: async () => {
    set({ busy: true, error: null });
    try {
      const status = await getInstanceBackupStatus();
      set({ instanceBackupStatus: status, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  setupBackupSchedule: async () => {
    set({ busy: true, error: null });
    try {
      const result = await setupInstanceBackupSchedule();
      set({ busy: false });
      return result;
    } catch (error) {
      set({ busy: false, error: mapError(error) });
      return null;
    }
  },

  removeBackupSchedule: async () => {
    set({ busy: true, error: null });
    try {
      const result = await removeInstanceBackupSchedule();
      set({ busy: false });
      return result;
    } catch (error) {
      set({ busy: false, error: mapError(error) });
      return null;
    }
  },

  loadSunshineSettings: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const settings = await getInstanceSunshineSettings(instanceId);
      set({ sunshineSettings: settings, instanceActionRunning: false });
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
    }
  },

  saveSunshineSettings: async (instanceId, settings) => {
    set({ instanceActionRunning: true, error: null });
    try {
      await updateInstanceSunshineSettings(instanceId, settings);
      const refreshed = await getInstanceSunshineSettings(instanceId);
      set({ sunshineSettings: refreshed, instanceActionRunning: false });
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
    }
  },

  reconnectWireguard: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const result = await reconnectInstanceWireguard(instanceId);
      set({ instanceActionRunning: false });
      return result;
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
      return null;
    }
  },

  rebootInstanceServices: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const result = await rebootInstanceServices(instanceId);
      set({ instanceActionRunning: false });
      return result;
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
      return null;
    }
  },

  pauseInstance: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      await pauseInstance(instanceId);
      set({ instanceActionRunning: false });
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
    }
  },

  destroyInstance: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      await destroyInstance(instanceId);
      const appState = await getAppState();
      set({ appState, instanceActionRunning: false });
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
    }
  },

  generateBundleIndex: async () => {
    set({ busy: true, error: null });
    try {
      await generateBundleIndex();
      set({ busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  loadRestoreBundles: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const index = await getInstanceRestoreBundles(instanceId);
      set({ bundleIndex: index, instanceActionRunning: false });
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
    }
  },

  runDryRunRestore: async (instanceId, payload) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const result = await dryRunRestore(instanceId, payload);
      set({ instanceActionRunning: false });
      return result;
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
      return null;
    }
  },

  runRestoreBundle: async (instanceId, payload) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const job = await restoreBundle(instanceId, payload);
      set({ restoreJob: job, instanceActionRunning: false });
      return job;
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
      return null;
    }
  },

  pollRestoreJob: async (jobId) => {
    try {
      const job = await getRestoreJob(jobId);
      set({ restoreJob: job });
    } catch (error) {
      set({ error: mapError(error) });
    }
  },

  loadMicConfig: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const config = await getInstanceMicConfig(instanceId);
      set({ micConfig: config, instanceActionRunning: false });
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
    }
  },

  updateMicSettings: async (instanceId, payload) => {
    set({ busy: true, error: null });
    try {
      const config = await updateInstanceMicSettings(instanceId, payload);
      set({ micConfig: config, busy: false });
    } catch (error) {
      set({ busy: false, error: mapError(error) });
    }
  },

  enableMic: async (instanceId, qualityProfile) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const session = await enableInstanceMic(instanceId, qualityProfile);
      set({ micSession: session, instanceActionRunning: false });
      return session;
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
      return null;
    }
  },

  disableMic: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      await disableInstanceMic(instanceId);
      set({ micSession: null, instanceActionRunning: false });
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
    }
  },

  reconnectMic: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      const session = await reconnectInstanceMic(instanceId);
      set({ micSession: session, instanceActionRunning: false });
      return session;
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
      return null;
    }
  },

  recreateMicDevice: async (instanceId) => {
    set({ instanceActionRunning: true, error: null });
    try {
      await recreateInstanceMicDevice(instanceId);
      set({ instanceActionRunning: false });
    } catch (error) {
      set({ instanceActionRunning: false, error: mapError(error) });
    }
  },

  loadMicStatus: async (instanceId) => {
    try {
      const status = await getInstanceMicStatus(instanceId);
      set({ micStatus: status });
    } catch (error) {
      set({ error: mapError(error) });
    }
  },

  clearError: () => set({ error: null })
}));
