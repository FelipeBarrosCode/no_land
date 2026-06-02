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
  setupWireguardAppHandoff,
  startPlayExistingInstance,
  startPlayFlow,
  submitMoonlightPinToSunshine,
  submitPairingPin,
  skipPairingAndContinue,
  subscribeProvisioningEvents,
  verifyWireguard,
  openWireguardApp,
  downloadWireguardConfig,
  getSetupStatus,
  verifySunshine,
  detectMoonlight,
  setupMoonlightSunshine,
  retrySetupStage,
  updatePlatformCredentials,
  updateMoonlightPreferences,
  regenerateEdid,
  updateServerPreferences,
  updateSshCredentials,
  updateVastApiKey,
  getSharedStorageSettings,
  saveSharedStorageSettings,
  testSharedStorageConfig,
  triggerInstanceBackup,
  triggerInstanceBackupFor,
  getInstanceBackupStatus,
  getInstanceSunshineSettings,
  updateInstanceSunshineSettings,
  resetInstanceSunshineSettings,
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
  startLocalSleepPrevention,
  stopLocalSleepPrevention,
  listInstanceSharedStorageObjects,
  syncInstanceFromSharedStorageSelected,
  listInstanceExportableStorageObjects,
  saveInstanceToSharedStorageSelected,
} from "../lib/backend";
import { PROVISIONING_ORDER } from "../lib/constants";
import type { BlockingActionState } from "../components/ui/BlockingLoaderOverlay";
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
  MicQualityProfile,
  MoonlightDetectionResult,
  OrchestrationState,
  PostWireGuardSetupState,
  ReachabilityResult,
  SetupStage,
  SunshineVerificationResult,
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
  blockingAction: BlockingActionState | null;
  isBlocking: boolean;
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
  savePlatformCredentials: (
    payload: PlatformCredentialsUpdate,
  ) => Promise<void>;
  saveServerPreferences: (
    payload: Partial<ServerPreferencesUpdate>,
  ) => Promise<void>;
  saveMoonlightPreferences: (payload: MoonlightPreferences) => Promise<void>;
  saveSshCredentials: (payload: SshCredentialsUpdate) => Promise<void>;
  regenerateEdid: (payload: {
    mode: "auto_detect" | "manual";
    refreshRateHz: number;
  }) => Promise<void>;
  submitPin: (pin: string) => Promise<void>;
  skipPairing: () => Promise<void>;
  setupLocalWireguardClient: () => Promise<void>;
  reconnectLocalWireguardClient: () => Promise<string | null>;
  setupWireguardAppHandoff: () => Promise<PostWireGuardSetupState | null>;
  verifyWireguardConnection: () => Promise<ReachabilityResult | null>;
  openWireguardApp: () => Promise<void>;
  downloadWireguardConfig: () => Promise<string | null>;
  verifySunshine: () => Promise<SunshineVerificationResult | null>;
  detectMoonlight: () => Promise<MoonlightDetectionResult | null>;
  setupMoonlightSunshine: () => Promise<PostWireGuardSetupState | null>;
  submitMoonlightPin: (pin: string) => Promise<PostWireGuardSetupState | null>;
  retrySetupStage: (
    stage: SetupStage,
  ) => Promise<PostWireGuardSetupState | null>;
  sleepPreventionActive: boolean;
  startSleepPrevention: () => Promise<string | null>;
  stopSleepPrevention: () => Promise<string | null>;
  sharedStorageSettings: SharedStorageSettingsResponse | null;
  backupStatus: BackupStatusResponse | null;
  instanceBackupStatus: SharedStorageInstanceStatus | null;
  loadSharedStorageSettings: () => Promise<void>;
  saveSharedStorageSettings: (
    payload: SharedStorageSettingsUpdate,
  ) => Promise<void>;
  testSharedStorageConfig: () => Promise<string | null>;
  triggerBackup: () => Promise<void>;
  triggerBackupForInstance: (instanceId: number) => Promise<void>;
  syncInstanceStorage: (
    instanceId: number,
    selectedPaths: string[],
  ) => Promise<string | null>;
  listSyncableStorageObjects: (
    instanceId: number,
  ) => Promise<SharedStorageObjectEntry[] | null>;
  saveInstanceStorageSelected: (
    instanceId: number,
    selectedPaths: string[],
  ) => Promise<string | null>;
  listExportableStorageObjects: (
    instanceId: number,
  ) => Promise<SharedStorageObjectEntry[] | null>;
  loadBackupStatus: () => Promise<void>;
  loadInstanceBackupStatus: () => Promise<void>;
  setupBackupSchedule: () => Promise<string | null>;
  removeBackupSchedule: () => Promise<string | null>;
  sunshineSettings: SunshineSettingsResponse | null;
  instanceActionRunning: boolean;
  loadSunshineSettings: (
    instanceId: number,
    sunshineUsername: string,
    sunshinePassword: string,
  ) => Promise<void>;
  saveSunshineSettings: (
    instanceId: number,
    settings: Record<string, unknown>,
    sunshineUsername: string,
    sunshinePassword: string,
  ) => Promise<void>;
  resetSunshineSettings: (
    instanceId: number,
    sunshineUsername: string,
    sunshinePassword: string,
  ) => Promise<void>;
  reconnectWireguard: (instanceId: number) => Promise<string | null>;
  rebootInstanceServices: (instanceId: number) => Promise<string | null>;
  pauseInstance: (instanceId: number) => Promise<void>;
  destroyInstance: (instanceId: number) => Promise<void>;
  bundleIndex: BundleIndex | null;
  restoreJob: RestoreJob | null;
  generateBundleIndex: () => Promise<void>;
  loadRestoreBundles: (instanceId: number) => Promise<void>;
  runDryRunRestore: (
    instanceId: number,
    payload: RestoreRequest,
  ) => Promise<RestoreDryRunResult | null>;
  runRestoreBundle: (
    instanceId: number,
    payload: RestoreRequest,
  ) => Promise<RestoreJob | null>;
  pollRestoreJob: (jobId: string) => Promise<void>;
  micConfig: InstanceMicConfig | null;
  micStatus: InstanceMicRuntimeStatus | null;
  micSession: MicSessionResponse | null;
  loadMicConfig: (instanceId: number) => Promise<void>;
  updateMicSettings: (
    instanceId: number,
    payload: MicSettingsUpdate,
  ) => Promise<void>;
  enableMic: (
    instanceId: number,
    qualityProfile?: MicQualityProfile,
  ) => Promise<MicSessionResponse | null>;
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

interface AsyncActionOptions {
  key: string;
  label: string;
  detail?: string;
  blocking?: boolean;
}

const PROVISIONING_INTERACTIVE_STATES = new Set<OrchestrationState>([
  "WireGuardConfigGenerated",
  "WireGuardAppHandoffStarted",
  "WireGuardWaitingForImport",
  "WireGuardWaitingForActivation",
  "WireGuardConnected",
  "MoonlightSunshineReadyToSetup",
  "MoonlightPairingStarted",
  "MoonlightPinReceived",
  "MoonlightSunshinePaired",
  "AwaitingPairPin",
  "Pairing",
  "Ready",
  "Error",
]);

const POST_WIREGUARD_EVENT_STAGE_MAP: Partial<
  Record<OrchestrationState, SetupStage>
> = {
  WireGuardConfigGenerated: "wireguard_config_generated",
  WireGuardAppHandoffStarted: "wireguard_app_handoff_started",
  WireGuardWaitingForImport: "wireguard_waiting_for_import",
  WireGuardWaitingForActivation: "wireguard_waiting_for_activation",
  WireGuardVerifying: "wireguard_verifying",
  WireGuardConnected: "wireguard_connected",
  MoonlightSunshineReadyToSetup: "moonlight_sunshine_ready_to_setup",
  SunshineCredentialsConfiguring: "sunshine_credentials_configuring",
  SunshineVerifying: "sunshine_verifying",
  MoonlightDetecting: "moonlight_detecting",
  MoonlightPairingStarted: "moonlight_pairing_started",
  MoonlightPinReceived: "moonlight_pin_received",
  SunshinePinSubmitting: "sunshine_pin_submitting",
  MoonlightSunshinePaired: "moonlight_sunshine_paired",
};

function applyPostWireguardEventState(
  appState: PersistedAppState,
  orchestrationState: OrchestrationState,
): PersistedAppState {
  const stage = POST_WIREGUARD_EVENT_STAGE_MAP[orchestrationState];
  if (!stage) {
    return appState;
  }

  return {
    ...appState,
    postWireguardSetup: {
      ...appState.postWireguardSetup,
      stage,
      wireguardSetupStatus:
        orchestrationState === "WireGuardConnected"
          ? "connected"
          : appState.postWireguardSetup.wireguardSetupStatus,
      setupComplete:
        orchestrationState === "Ready"
          ? true
          : appState.postWireguardSetup.setupComplete,
      paired:
        orchestrationState === "MoonlightSunshinePaired" ||
        orchestrationState === "Ready"
          ? true
          : appState.postWireguardSetup.paired,
    },
  };
}

async function refreshProvisioningState(
  set: (partial: Partial<AppStore>) => void,
): Promise<void> {
  try {
    const [appState, postWireguardSetup] = await Promise.all([
      getAppState(),
      getSetupStatus(),
    ]);
    set({
      appState: {
        ...appState,
        postWireguardSetup,
      },
    });
  } catch {
    // Keep the original frontend error if the best-effort refresh fails.
  }
}

async function applyProvisioningEventState(
  event: ProvisioningEvent,
  set: (partial: Partial<AppStore> | ((state: AppStore) => Partial<AppStore>)) => void,
): Promise<void> {
  let latestPostWireguardSetup: PostWireGuardSetupState | null = null;
  if (PROVISIONING_INTERACTIVE_STATES.has(event.state)) {
    try {
      latestPostWireguardSetup = await getSetupStatus();
    } catch {
      latestPostWireguardSetup = null;
    }
  }

  set((state) => {
    const nextLogs = [event, ...state.logs].slice(0, 500);
    const nextBaseState = state.appState
      ? {
          ...state.appState,
          orchestrationState: event.state,
          lastError: event.isError ? event.message : state.appState.lastError,
          ...(latestPostWireguardSetup
            ? { postWireguardSetup: latestPostWireguardSetup }
            : {}),
        }
      : state.appState;
    const nextState = nextBaseState
      ? applyPostWireguardEventState(nextBaseState, event.state)
      : nextBaseState;

    const updates: Partial<AppStore> = {
      logs: nextLogs,
      appState: nextState,
      error: event.isError ? event.message : state.error,
    };

    if (event.isError || PROVISIONING_INTERACTIVE_STATES.has(event.state)) {
      updates.busy = false;
      if (state.blockingAction?.key === "provisioning.flow") {
        updates.blockingAction = null;
        updates.isBlocking = false;
      }
      return updates;
    }

    updates.busy = true;
    updates.isBlocking = true;
    updates.blockingAction = createBlockingAction(state, {
      key: "provisioning.flow",
      label: "Provisioning session",
      detail:
        event.message ||
        PROVISIONING_STEP_LABELS[event.state] ||
        "Preparing your instance",
      progress: getProvisioningProgress(event.state),
      mode: "determinate",
    });

    return updates;
  });
}

const PROVISIONING_STEP_LABELS: Partial<Record<OrchestrationState, string>> = {
  GeneratingSshKey: "Generating SSH key",
  UploadingSshKeyToVast: "Uploading SSH key to Vast.ai",
  CreatingInstance: "Creating rented instance",
  WaitingForInstance: "Waiting for instance readiness",
  VerifyingReservation: "Verifying reservation",
  ConnectingSsh: "Connecting over SSH",
  ConfiguringSunshine: "Configuring Sunshine",
  ConfiguringWireGuard: "Configuring WireGuard",
  ConfiguringNvidiaHeadless: "Configuring NVIDIA headless mode",
  WireGuardConfigGenerated: "WireGuard config generated",
  WireGuardAppHandoffStarted: "Opening WireGuard app",
  WireGuardWaitingForImport: "Waiting for WireGuard import",
  WireGuardWaitingForActivation: "Waiting for WireGuard activation",
  WireGuardVerifying: "Verifying secure tunnel",
  WireGuardConnected: "Secure tunnel connected",
  MoonlightSunshineReadyToSetup: "Ready to set up Moonlight and Sunshine",
  SunshineCredentialsConfiguring: "Configuring Sunshine credentials",
  SunshineVerifying: "Verifying Sunshine",
  MoonlightDetecting: "Finding Moonlight",
  MoonlightPairingStarted: "Starting Moonlight pairing",
  MoonlightPinReceived: "Moonlight PIN received",
  SunshinePinSubmitting: "Submitting PIN to Sunshine",
  MoonlightSunshinePaired: "Moonlight and Sunshine paired",
  ConfiguringMoonlight: "Preparing Moonlight pairing",
  AwaitingPairPin: "Awaiting Moonlight PIN",
  Pairing: "Completing Moonlight pairing",
  Ready: "Session ready",
};

function getProvisioningProgress(state: OrchestrationState): number | null {
  const index = PROVISIONING_ORDER.indexOf(
    state as (typeof PROVISIONING_ORDER)[number],
  );
  if (index === -1) {
    return null;
  }

  return ((index + 1) / PROVISIONING_ORDER.length) * 100;
}

function createBlockingAction(
  state: AppStore,
  next: Omit<BlockingActionState, "startedAt"> & { startedAt?: number },
): BlockingActionState {
  const startedAt =
    state.blockingAction?.key === next.key
      ? state.blockingAction.startedAt
      : (next.startedAt ?? Date.now());

  return {
    ...next,
    startedAt,
  };
}

function shouldClearBlockingAction(
  current: BlockingActionState | null,
  completedKey: string,
  isBlockingTask: boolean | undefined,
): boolean {
  if (!isBlockingTask || !current) {
    return false;
  }

  return current.key === completedKey || current.key === "provisioning.flow";
}

export const useAppStore = create<AppStore>((set, get) => {
  const runBusyTask = async <T>(
    options: AsyncActionOptions,
    task: () => Promise<T>,
    fallback: T,
  ): Promise<T> => {
    set({ busy: true, error: null });

    if (options.blocking) {
      set((state) => ({
        blockingAction: createBlockingAction(state, {
          key: options.key,
          label: options.label,
          detail: options.detail,
          progress: null,
          mode: "indeterminate",
        }),
        isBlocking: true,
      }));
    }

    try {
      return await task();
    } catch (error) {
      set({ error: mapError(error) });
      return fallback;
    } finally {
      set((state) => ({
        busy: false,
        ...(shouldClearBlockingAction(
          state.blockingAction,
          options.key,
          options.blocking,
        )
          ? { blockingAction: null, isBlocking: false }
          : {}),
      }));
    }
  };

  const runInstanceTask = async <T>(
    options: AsyncActionOptions,
    task: () => Promise<T>,
    fallback: T,
  ): Promise<T> => {
    set({ instanceActionRunning: true, error: null });

    if (options.blocking) {
      set((state) => ({
        blockingAction: createBlockingAction(state, {
          key: options.key,
          label: options.label,
          detail: options.detail,
          progress: null,
          mode: "indeterminate",
        }),
        isBlocking: true,
      }));
    }

    try {
      return await task();
    } catch (error) {
      set({ error: mapError(error) });
      return fallback;
    } finally {
      set((state) => ({
        instanceActionRunning: false,
        ...(shouldClearBlockingAction(
          state.blockingAction,
          options.key,
          options.blocking,
        )
          ? { blockingAction: null, isBlocking: false }
          : {}),
      }));
    }
  };

  const beginProvisioningBlock = (detail: string) => {
    set((state) => ({
      busy: true,
      error: null,
      blockingAction: createBlockingAction(state, {
        key: "provisioning.flow",
        label: "Provisioning session",
        detail,
        progress:
          getProvisioningProgress(
            state.appState?.orchestrationState ?? "CreatingInstance",
          ) ?? 0,
        mode: "determinate",
      }),
      isBlocking: true,
    }));
  };

  const endProvisioningBlock = () => {
    set((state) => ({
      busy: false,
      ...(state.blockingAction?.key === "provisioning.flow"
        ? { blockingAction: null, isBlocking: false }
        : {}),
    }));
  };

  return {
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
    blockingAction: null,
    isBlocking: false,
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
    sleepPreventionActive: false,

    initialize: async () => {
      set({ loading: true, error: null });
      try {
        const [appState, logs, postWireguardSetup] = await Promise.all([
          getAppState(),
          getProvisioningLogs(),
          getSetupStatus(),
        ]);
        let rentedInstances: RentedInstanceSummary[] = [];
        if (
          appState.onboardingCompleted &&
          appState.credentials.vastApiKey.trim().length > 0
        ) {
          rentedInstances = await getRentedInstances();
        }

        set({
          appState: {
            ...appState,
            postWireguardSetup,
          },
          logs,
          rentedInstances,
          loading: false,
        });
      } catch (error) {
        set({ loading: false, error: mapError(error) });
      }
    },

    bindEvents: async () => {
      if (get()._eventsBound) {
        return;
      }

      await subscribeProvisioningEvents((event) => {
        void applyProvisioningEventState(event, set);
      });

      set({ _eventsBound: true });
    },

    setServerPickerOpen: (serverPickerOpen) => set({ serverPickerOpen }),

    runOnboarding: async (payload) => {
      await runBusyTask(
        {
          key: "onboarding.setup",
          label: "Configuring Noland Connect",
          detail: "Saving local credentials and preparing your account.",
          blocking: true,
        },
        async () => {
          const appState = await completeOnboarding(payload);
          const rentedInstances = await getRentedInstances();
          set({ appState, rentedInstances });
        },
        undefined,
      );
    },

    saveManualLocation: async (payload) => {
      await runBusyTask(
        {
          key: "server.location",
          label: "Updating search location",
          detail: "Saving your server region filters.",
          blocking: true,
        },
        async () => {
          const appState = await setManualLocation(payload);
          set({ appState });
        },
        undefined,
      );
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
          searching: false,
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
      await runBusyTask(
        {
          key: "server.select",
          label: "Selecting server offer",
          detail: "Applying the selected GPU host and storage size.",
          blocking: true,
        },
        async () => {
          const appState = await selectOffer(offerId, storageGb);
          set({ appState, serverPickerOpen: false });
        },
        undefined,
      );
    },

    startPlay: async () => {
      beginProvisioningBlock(
        "Reserving hardware and starting your cloud gaming session.",
      );
      try {
        await startPlayFlow();
        const appState = await getAppState();
        set({ appState });
        if (PROVISIONING_INTERACTIVE_STATES.has(appState.orchestrationState)) {
          endProvisioningBlock();
        }
      } catch (error) {
        endProvisioningBlock();
        set({ error: mapError(error) });
      }
    },

    startPlayExisting: async (instanceId) => {
      beginProvisioningBlock("Reconnecting to your existing gaming instance.");
      try {
        await startPlayExistingInstance(instanceId);
        const appState = await getAppState();
        set({ appState });
        if (PROVISIONING_INTERACTIVE_STATES.has(appState.orchestrationState)) {
          endProvisioningBlock();
        }
      } catch (error) {
        endProvisioningBlock();
        set({ error: mapError(error) });
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
      await runBusyTask(
        {
          key: "settings.server",
          label: "Saving server preferences",
          detail: "Updating your offer filters and hardware requirements.",
          blocking: true,
        },
        async () => {
          const state = get();
          const current = state.appState?.serverPreferences;
          if (!current) {
            set({ error: "App state not initialized" });
            return;
          }

          const fullPayload: ServerPreferencesUpdate = {
            minReliability: payload.minReliability ?? current.minReliability,
            storageGb: payload.storageGb ?? current.storageGb,
            templateHash: payload.templateHash ?? current.templateHash,
            maxHourlyPrice: payload.maxHourlyPrice ?? current.maxHourlyPrice,
            minHourlyPrice: payload.minHourlyPrice ?? current.minHourlyPrice,
            requireVerified: payload.requireVerified ?? current.requireVerified,
            requireDatacenter:
              payload.requireDatacenter ?? current.requireDatacenter,
            includeOnDemand: payload.includeOnDemand ?? current.includeOnDemand,
            includeInterruptible:
              payload.includeInterruptible ?? current.includeInterruptible,
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
          set({ appState });
        },
        undefined,
      );
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

    regenerateEdid: async (payload) => {
      await runBusyTask(
        {
          key: "settings.edid.regenerate",
          label: "Regenerating EDID",
          detail: "Rebuilding headless display profile and saving it to state.",
          blocking: true,
        },
        async () => {
          await regenerateEdid(payload);
          const appState = await getAppState();
          set({ appState });
        },
        undefined,
      );
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
      await runBusyTask(
        {
          key: "wireguard.local.setup",
          label: "Setting up WireGuard",
          detail: "Installing the local tunnel configuration on this PC.",
        },
        async () => {
          await setupWireguardClient();
        },
        undefined,
      );
    },

    reconnectLocalWireguardClient: async () => {
      return await runBusyTask(
        {
          key: "wireguard.local.reconnect",
          label: "Opening WireGuard",
          detail: "Open the WireGuard app and manage the tunnel manually.",
        },
        async () => {
          await openWireguardApp();
          return "Opened WireGuard app.";
        },
        null,
      );
    },

    setupWireguardAppHandoff: async () => {
      return runBusyTask(
        {
          key: "wireguard.appHandoff",
          label: "Opening WireGuard app",
          detail: "Preparing your generated tunnel for the WireGuard app.",
          blocking: true,
        },
        async () => {
          const setup = await setupWireguardAppHandoff();
          const appState = await getAppState();
          set({ appState });
          return setup;
        },
        null,
      );
    },

    verifyWireguardConnection: async () => {
      return runBusyTask(
        {
          key: "wireguard.verify",
          label: "Verifying secure tunnel",
          detail: "Checking 10.77.0.1 over the WireGuard tunnel.",
          blocking: true,
        },
        async () => {
          const result = await verifyWireguard();
          const appState = await getAppState();
          set({ appState });
          return result;
        },
        null,
      );
    },

    openWireguardApp: async () => {
      await runBusyTask(
        {
          key: "wireguard.open",
          label: "Opening WireGuard",
          detail: "Generating client config and launching the WireGuard app.",
        },
        async () => {
          await downloadWireguardConfig();
          const appState = await getAppState();
          set({ appState });
          await openWireguardApp();
        },
        undefined,
      );
    },

    downloadWireguardConfig: async () => {
      return runBusyTask(
        {
          key: "wireguard.download",
          label: "Exporting WireGuard config",
          detail: "Preparing a WireGuard config file you can import.",
        },
        async () => {
          const path = await downloadWireguardConfig();
          const appState = await getAppState();
          set({ appState });
          return path;
        },
        null,
      );
    },

    verifySunshine: async () => {
      return runBusyTask(
        {
          key: "sunshine.verify",
          label: "Verifying Sunshine",
          detail: "Checking Sunshine over 10.77.0.1.",
        },
        async () => {
          const result = await verifySunshine();
          await refreshProvisioningState(set);
          return result;
        },
        null,
      );
    },

    detectMoonlight: async () => {
      return runBusyTask(
        {
          key: "moonlight.detect",
          label: "Finding Moonlight",
          detail: "Looking for Moonlight on this machine.",
        },
        async () => {
          const result = await detectMoonlight();
          await refreshProvisioningState(set);
          return result;
        },
        null,
      );
    },

    setupMoonlightSunshine: async () => {
      return runBusyTask(
        {
          key: "moonlightSunshine.setup",
          label: "Setting up Moonlight and Sunshine",
          detail: "Preparing Moonlight pairing over the secure tunnel.",
          blocking: true,
        },
        async () => {
          try {
            const setup = await setupMoonlightSunshine();
            await refreshProvisioningState(set);
            return setup;
          } catch (error) {
            await refreshProvisioningState(set);
            throw error;
          }
        },
        null,
      );
    },

    submitMoonlightPin: async (pin) => {
      return runBusyTask(
        {
          key: "moonlightSunshine.pin",
          label: "Submitting PIN to Sunshine",
          detail: "Pairing Moonlight with Sunshine over the secure tunnel.",
          blocking: true,
        },
        async () => {
          try {
            const setup = await submitMoonlightPinToSunshine(pin);
            await refreshProvisioningState(set);
            return setup;
          } catch (error) {
            await refreshProvisioningState(set);
            throw error;
          }
        },
        null,
      );
    },

    retrySetupStage: async (stage) => {
      return runBusyTask(
        {
          key: "postWireguard.retry",
          label: "Retrying setup step",
          detail: "Repeating only the failed post-WireGuard step.",
          blocking: true,
        },
        async () => {
          try {
            const setup = await retrySetupStage(stage);
            await refreshProvisioningState(set);
            return setup;
          } catch (error) {
            await refreshProvisioningState(set);
            throw error;
          }
        },
        null,
      );
    },

    startSleepPrevention: async () => {
      set({ busy: true, error: null });
      try {
        const result = await startLocalSleepPrevention();
        set({ busy: false, sleepPreventionActive: true });
        return result;
      } catch (error) {
        set({ busy: false, error: mapError(error) });
        return null;
      }
    },

    stopSleepPrevention: async () => {
      set({ busy: true, error: null });
      try {
        const result = await stopLocalSleepPrevention();
        set({ busy: false, sleepPreventionActive: false });
        return result;
      } catch (error) {
        set({ busy: false, error: mapError(error) });
        return null;
      }
    },

    loadSharedStorageSettings: async () => {
      await runBusyTask(
        {
          key: "storage.settings.load",
          label: "Loading shared storage settings",
          detail: "Fetching your Backblaze and rclone configuration.",
        },
        async () => {
          const settings = await getSharedStorageSettings();
          set({ sharedStorageSettings: settings });
        },
        undefined,
      );
    },

    saveSharedStorageSettings: async (payload) => {
      await runBusyTask(
        {
          key: "storage.settings.save",
          label: "Saving shared storage settings",
          detail: "Updating backup credentials and destination settings.",
          blocking: true,
        },
        async () => {
          const appState = await saveSharedStorageSettings(payload);
          const settings = await getSharedStorageSettings();
          set({ appState, sharedStorageSettings: settings });
        },
        undefined,
      );
    },

    testSharedStorageConfig: async () => {
      return await runBusyTask(
        {
          key: "storage.settings.test",
          label: "Testing shared storage connection",
          detail: "Checking your Backblaze bucket and remote access.",
        },
        async () => await testSharedStorageConfig(),
        null,
      );
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
      return await runInstanceTask(
        {
          key: "instance.storage.sync",
          label: "Syncing files from shared storage",
          detail:
            "Copying the selected files and folders to the remote instance.",
          blocking: true,
        },
        async () => {
          console.info("[shared-storage] sync start", {
            instanceId,
            selectedCount: selectedPaths.length,
          });
          if (selectedPaths.length === 0) {
            throw new Error("Select at least one file or folder to sync.");
          }
          const message = await syncInstanceFromSharedStorageSelected(
            instanceId,
            selectedPaths,
          );
          console.info("[shared-storage] sync complete", {
            instanceId,
            message,
          });
          return message;
        },
        null,
      );
    },

    listSyncableStorageObjects: async (instanceId) => {
      set({ error: null });
      try {
        console.info("[shared-storage] listing remote objects start", {
          instanceId,
        });
        const entries = await listInstanceSharedStorageObjects(instanceId);
        console.info("[shared-storage] listing remote objects complete", {
          instanceId,
          count: entries.length,
        });
        return entries;
      } catch (error) {
        console.error("[shared-storage] listing remote objects failed", {
          instanceId,
          error,
        });
        set({ error: mapError(error) });
        return null;
      }
    },

    saveInstanceStorageSelected: async (instanceId, selectedPaths) => {
      return await runInstanceTask(
        {
          key: "instance.storage.export",
          label: "Exporting files to shared storage",
          detail: "Saving the selected instance files back to cloud storage.",
          blocking: true,
        },
        async () =>
          await saveInstanceToSharedStorageSelected(instanceId, selectedPaths),
        null,
      );
    },

    listExportableStorageObjects: async (instanceId) => {
      set({ error: null });
      try {
        return await listInstanceExportableStorageObjects(instanceId);
      } catch (error) {
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
            lastBackupError: status.lastBackupError,
          },
          busy: false,
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
      set({
        error:
          "Scheduled backups are disabled. Save selected files manually from the shared storage interface.",
      });
      return null;
    },

    removeBackupSchedule: async () => {
      set({
        error:
          "Scheduled backups are disabled. There is no active schedule to remove.",
      });
      return null;
    },

    loadSunshineSettings: async (
      instanceId,
      sunshineUsername,
      sunshinePassword,
    ) => {
      await runInstanceTask(
        {
          key: "sunshine.settings.load",
          label: "Loading Sunshine settings",
          detail:
            "Fetching the current Sunshine configuration from the instance.",
        },
        async () => {
          const settings = await getInstanceSunshineSettings(
            instanceId,
            sunshineUsername,
            sunshinePassword,
          );
          set({ sunshineSettings: settings });
        },
        undefined,
      );
    },

    saveSunshineSettings: async (
      instanceId,
      settings,
      sunshineUsername,
      sunshinePassword,
    ) => {
      await runInstanceTask(
        {
          key: "sunshine.settings.save",
          label: "Saving Sunshine settings",
          detail:
            "Applying the updated Sunshine configuration on the instance.",
        },
        async () => {
          await updateInstanceSunshineSettings(
            instanceId,
            settings,
            sunshineUsername,
            sunshinePassword,
          );
          const refreshed = await getInstanceSunshineSettings(
            instanceId,
            sunshineUsername,
            sunshinePassword,
          );
          set({ sunshineSettings: refreshed });
        },
        undefined,
      );
    },

    resetSunshineSettings: async (
      instanceId,
      sunshineUsername,
      sunshinePassword,
    ) => {
      await runInstanceTask(
        {
          key: "sunshine.settings.reset",
          label: "Resetting Sunshine settings",
          detail:
            "Restoring the provisioned Sunshine defaults on the instance.",
        },
        async () => {
          await resetInstanceSunshineSettings(
            instanceId,
            sunshineUsername,
            sunshinePassword,
          );
          const refreshed = await getInstanceSunshineSettings(
            instanceId,
            sunshineUsername,
            sunshinePassword,
          );
          set({ sunshineSettings: refreshed });
        },
        undefined,
      );
    },

    reconnectWireguard: async (instanceId) => {
      return await runInstanceTask(
        {
          key: "instance.wireguard.reconnect",
          label: "Opening WireGuard",
          detail: "Open the WireGuard app and manage the tunnel manually.",
          blocking: true,
        },
        async () => await reconnectInstanceWireguard(instanceId),
        null,
      );
    },

    rebootInstanceServices: async (instanceId) => {
      return await runInstanceTask(
        {
          key: "instance.services.reboot",
          label: "Rebooting instance services",
          detail:
            "Restarting Sunshine, networking, and related streaming services.",
          blocking: true,
        },
        async () => await rebootInstanceServices(instanceId),
        null,
      );
    },

    pauseInstance: async (instanceId) => {
      await runInstanceTask(
        {
          key: "instance.pause",
          label: "Pausing instance",
          detail: "Suspending the rented machine until you resume it.",
          blocking: true,
        },
        async () => {
          await pauseInstance(instanceId);
        },
        undefined,
      );
    },

    destroyInstance: async (instanceId) => {
      await runInstanceTask(
        {
          key: "instance.destroy",
          label: "Destroying instance",
          detail:
            "Tearing down the rented machine and finalizing any backup steps.",
          blocking: true,
        },
        async () => {
          await destroyInstance(instanceId);
          const appState = await getAppState();
          set({ appState });
        },
        undefined,
      );
    },

    generateBundleIndex: async () => {
      await runBusyTask(
        {
          key: "restore.index.generate",
          label: "Generating restore index",
          detail: "Scanning backup metadata so bundles can be restored.",
          blocking: true,
        },
        async () => {
          await generateBundleIndex();
        },
        undefined,
      );
    },

    loadRestoreBundles: async (instanceId) => {
      await runInstanceTask(
        {
          key: "restore.index.load",
          label: "Loading restore bundles",
          detail: "Fetching indexed backup bundles for this instance.",
        },
        async () => {
          const index = await getInstanceRestoreBundles(instanceId);
          set({ bundleIndex: index });
        },
        undefined,
      );
    },

    runDryRunRestore: async (instanceId, payload) => {
      return await runInstanceTask(
        {
          key: "restore.dry_run",
          label: "Running restore dry run",
          detail:
            "Previewing which files would be restored before making changes.",
        },
        async () => await dryRunRestore(instanceId, payload),
        null,
      );
    },

    runRestoreBundle: async (instanceId, payload) => {
      return await runInstanceTask(
        {
          key: "restore.run",
          label: "Restoring backup bundle",
          detail: "Copying the selected backup data back onto the instance.",
          blocking: true,
        },
        async () => {
          const job = await restoreBundle(instanceId, payload);
          set({ restoreJob: job });
          return job;
        },
        null,
      );
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

    clearError: () => set({ error: null }),
  };
});
