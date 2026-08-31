import { create } from "zustand";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  completeOnboarding,
  getAppState,
  getRentedInstances,
  getProvisioningLogs,
  searchOffers,
  selectOffer,
  setManualLocation,
  setupWireguardClient,
  reconnectLocalWireguardClientQuick,
  setupWireguardAppHandoff,
  resumeProvisioningExistingInstance,
  startPlayExistingInstance,
  startPlayFlow,
  subscribeProvisioningEvents,
  verifyWireguard,
  getSetupStatus,
  verifySunshine,
  setupMoonlightSunshine,
  retrySetupStage,
  updatePlatformCredentials,
  updateIgdbCredentials,
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
  rebootInstanceServices,
  destroyInstance,


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
  setInstanceMoonlightPipelineEnabled,
  getInstanceMoonlightPipelineStatus,
  prepareInstanceMoonlightPairing,
  completeInstanceMoonlightPairing,
  getVastWalletSummary as getVastWalletSummaryCommand,
  listStorageProviders,
  saveStaticProviderCredentials,
  testSharedStorageConnection,
  getSharedStorageProfiles,
  setActiveSharedStorageProfile,
  disconnectSharedStorageProfile,
  beginOauthAuthorization,
  completeOauthAuthorization,
  getInstanceLaunchLibrary,
  launchInstanceSoftware as launchInstanceSoftwareCommand,
  getLaunchInstanceSoftwareJob,
  getSoftwareArtwork,
} from "../lib/backend";
import { PROVISIONING_ORDER } from "../lib/constants";
import type { BlockingActionState } from "../components/ui/BlockingLoaderOverlay";
import type {
  ManualLocationInput,
  MoonlightPreferences,
  OfferCandidate,
  OnboardingPayload,
  PlatformCredentialsUpdate,
  IgdbCredentialsUpdate,
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

  InstanceMicConfig,
  InstanceMicRuntimeStatus,
  MicSessionResponse,
  MicSettingsUpdate,
  MicQualityProfile,
  MoonlightPairingSessionResponse,
  EmbeddedMoonlightInstanceStatus,
  OrchestrationState,
  PostWireGuardSetupState,
  ReachabilityResult,
  SetupStage,
  SunshineVerificationResult,
  VastWalletSummary,
  ProviderDefinition,
  ProfileReference,
  SharedStorageProfile,
  SharedStorageTestResult,
  LaunchLibraryResponse,
  LaunchSoftwareJob,
  SoftwareArtworkResult,
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
  provisioningModalDismissed: boolean;
  error: string | null;
  _eventsBound: boolean;
  vastWalletSummary: VastWalletSummary | null;
  initialize: () => Promise<void>;
  bindEvents: () => Promise<void>;
  dismissProvisioningModal: () => void;
  reopenProvisioningModal: () => void;
  runOnboarding: (payload: OnboardingPayload) => Promise<void>;
  saveManualLocation: (payload: ManualLocationInput) => Promise<void>;
  discoverOffers: (page?: number) => Promise<void>;
  nextOffersPage: () => Promise<void>;
  previousOffersPage: () => Promise<void>;
  chooseOffer: (offerId: number, storageGb: number) => Promise<boolean>;
  startPlay: () => Promise<void>;
  resumeProvisioningExisting: (instanceId: number) => Promise<string | null>;
  startPlayExisting: (instanceId: number) => Promise<string | null>;
  launchLibrary: LaunchLibraryResponse | null;
  launchLibraryLoading: boolean;
  launchSoftwareJob: LaunchSoftwareJob | null;
  launchingSoftwareAppId: string | null;
  softwareArtwork: Record<string, SoftwareArtworkResult>;
  softwareArtworkLoading: Record<string, boolean>;
  loadInstanceLaunchLibrary: (
    instanceId: number,
  ) => Promise<LaunchLibraryResponse | null>;
  launchInstanceSoftware: (
    instanceId: number,
    appId: string,
  ) => Promise<LaunchSoftwareJob | null>;
  pollLaunchSoftwareJob: (jobId: string) => Promise<LaunchSoftwareJob | null>;
  loadSoftwareArtwork: (name: string) => Promise<SoftwareArtworkResult | null>;
  clearLaunchLibrary: () => void;
  loadRentedInstances: () => Promise<void>;
  saveVastApiKey: (apiKey: string) => Promise<void>;
  refreshVastWalletSummary: () => Promise<VastWalletSummary | null>;
  savePlatformCredentials: (
    payload: PlatformCredentialsUpdate,
  ) => Promise<void>;
  saveIgdbCredentials: (payload: IgdbCredentialsUpdate) => Promise<void>;
  saveServerPreferences: (
    payload: Partial<ServerPreferencesUpdate>,
  ) => Promise<void>;
  saveMoonlightPreferences: (payload: MoonlightPreferences) => Promise<void>;
  saveSshCredentials: (payload: SshCredentialsUpdate) => Promise<void>;
  regenerateEdid: (payload: {
    mode: "auto_detect" | "mac_hardware" | "manual";
    refreshRateHz: number;
  }) => Promise<void>;
  setupLocalWireguardClient: () => Promise<void>;
  reconnectLocalWireguardClient: () => Promise<string | null>;
  setupWireguardAppHandoff: () => Promise<PostWireGuardSetupState | null>;
  verifyWireguardConnection: () => Promise<ReachabilityResult | null>;
  verifySunshine: () => Promise<SunshineVerificationResult | null>;
  setupMoonlightSunshine: () => Promise<PostWireGuardSetupState | null>;
  retrySetupStage: (
    stage: SetupStage,
  ) => Promise<PostWireGuardSetupState | null>;
  sleepPreventionActive: boolean;
  startSleepPrevention: () => Promise<string | null>;
  stopSleepPrevention: () => Promise<string | null>;
  sharedStorageSettings: SharedStorageSettingsResponse | null;
  storageProviders: ProviderDefinition[];
  sharedStorageProfiles: ProfileReference[];
  sharedStorageTestResult: SharedStorageTestResult | null;
  oauthSessionId: string | null;
  backupStatus: BackupStatusResponse | null;
  instanceBackupStatus: SharedStorageInstanceStatus | null;
  loadSharedStorageSettings: () => Promise<void>;
  saveSharedStorageSettings: (
    payload: SharedStorageSettingsUpdate,
  ) => Promise<void>;
  testSharedStorageConfig: () => Promise<string | null>;
  loadStorageProviders: () => Promise<void>;
  connectStorageProvider: (
    provider: string,
    credentials: Record<string, string>,
    bucket: string | null,
    prefix: string | null,
    displayName: string,
  ) => Promise<void>;
  testStorageConnection: (profileId: string) => Promise<void>;
  loadSharedStorageProfiles: () => Promise<void>;
  setActiveStorageProfile: (profileId: string) => Promise<void>;
  disconnectStorageProfile: (profileId: string) => Promise<void>;
  syncActiveInstanceToSharedStorage: () => Promise<void>;
  beginOauthFlow: (
    provider: string,
    displayName: string,
    clientId?: string,
    clientSecret?: string | null,
    providerFields?: Record<string, string>,
  ) => Promise<string | null>;
  completeOauthFlow: (sessionId: string) => Promise<void>;
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
  embeddedMoonlightStatus: EmbeddedMoonlightInstanceStatus | null;
  activeMoonlightPairing: MoonlightPairingSessionResponse | null;
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
  setEmbeddedMoonlightPipelineEnabled: (
    instanceId: number,
    enabled: boolean,
  ) => Promise<void>;
  loadEmbeddedMoonlightStatus: (
    instanceId: number,
  ) => Promise<EmbeddedMoonlightInstanceStatus | null>;
  prepareEmbeddedMoonlightPairing: (
    instanceId: number,
  ) => Promise<MoonlightPairingSessionResponse | null>;
  completeEmbeddedMoonlightPairing: (
    instanceId: number,
    sessionId: string,
  ) => Promise<boolean>;
  resetSunshineSettings: (
    instanceId: number,
    sunshineUsername: string,
    sunshinePassword: string,
  ) => Promise<void>;
  rebootInstanceServices: (instanceId: number) => Promise<string | null>;
  destroyInstance: (instanceId: number) => Promise<void>;

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

function applyEmbeddedMoonlightStatusToInstances(
  rentedInstances: RentedInstanceSummary[],
  embeddedMoonlightStatus: EmbeddedMoonlightInstanceStatus | null,
): RentedInstanceSummary[] {
  if (!embeddedMoonlightStatus) {
    return rentedInstances;
  }

  return rentedInstances.map((instance) =>
    instance.instanceId === embeddedMoonlightStatus.instanceId
      ? {
          ...instance,
          embeddedMoonlightPipelineEnabled: embeddedMoonlightStatus.enabled,
          embeddedMoonlightSessionState: embeddedMoonlightStatus.sessionState,
          embeddedMoonlightLastError: embeddedMoonlightStatus.lastError,
          embeddedMoonlightLastRuntimeEvent:
            embeddedMoonlightStatus.lastRuntimeEvent,
          embeddedMoonlightRuntimeConnected:
            embeddedMoonlightStatus.runtimeConnected,
          embeddedMoonlightRendererReady:
            embeddedMoonlightStatus.rendererReady,
          embeddedMoonlightVideoSessionActive:
            embeddedMoonlightStatus.videoSessionActive,
          embeddedMoonlightVideoFrameCount:
            embeddedMoonlightStatus.videoFrameCount,
          embeddedMoonlightRendererSubmittedFrameCount:
            embeddedMoonlightStatus.rendererSubmittedFrameCount,
          embeddedMoonlightRendererDroppedFrameCount:
            embeddedMoonlightStatus.rendererDroppedFrameCount,
          embeddedMoonlightAudioSampleCount:
            embeddedMoonlightStatus.audioSampleCount,
          embeddedMoonlightPaired: embeddedMoonlightStatus.paired,
        }
      : instance,
  );
}

async function enrichRentedInstancesWithEmbeddedStatus(
  rentedInstances: RentedInstanceSummary[],
): Promise<RentedInstanceSummary[]> {
  if (rentedInstances.length === 0) {
    return rentedInstances;
  }

  const statuses = await Promise.all(
    rentedInstances.map(async (instance) => {
      if (!instance.embeddedMoonlightPipelineEnabled) {
        return null;
      }
      return getInstanceMoonlightPipelineStatus(instance.instanceId).catch(() => null);
    }),
  );

  return statuses.reduce(
    (instances, status) => applyEmbeddedMoonlightStatusToInstances(instances, status),
    rentedInstances,
  );
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

const PROVISIONING_MODAL_STATES = new Set<OrchestrationState>([
  "WireGuardConfigGenerated",
  "WireGuardAppHandoffStarted",
  "WireGuardWaitingForImport",
  "WireGuardWaitingForActivation",
  "WireGuardVerifying",
  "WireGuardConnected",
  "MoonlightSunshineReadyToSetup",
  "SunshineCredentialsConfiguring",
  "SunshineVerifying",
  "MoonlightDetecting",
  "MoonlightPairingStarted",
  "MoonlightPinReceived",
  "SunshinePinSubmitting",
  "MoonlightSunshinePaired",
  "Ready",
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
  set: (
    partial: Partial<AppStore> | ((state: AppStore) => Partial<AppStore>),
  ) => void,
): Promise<void> {
  let latestPostWireguardSetup: PostWireGuardSetupState | null = null;
  let latestAppState: PersistedAppState | null = null;
  if (PROVISIONING_INTERACTIVE_STATES.has(event.state)) {
    try {
      latestPostWireguardSetup = await getSetupStatus();
    } catch {
      latestPostWireguardSetup = null;
    }
  }

  if (event.state === "Ready") {
    try {
      latestAppState = await getAppState();
    } catch {
      latestAppState = null;
    }
  }

  set((state) => {
    const nextLogs = [event, ...state.logs].slice(0, 500);
    const nextBaseState = latestAppState
      ? {
          ...latestAppState,
          ...(latestPostWireguardSetup
            ? { postWireguardSetup: latestPostWireguardSetup }
            : {}),
        }
      : state.appState
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
      ? latestPostWireguardSetup
        ? nextBaseState
        : applyPostWireguardEventState(nextBaseState, event.state)
      : nextBaseState;

    const shouldReopenProvisioningModal =
      PROVISIONING_MODAL_STATES.has(event.state) &&
      state.appState?.orchestrationState !== event.state;

    const updates: Partial<AppStore> = {
      logs: nextLogs,
      appState: nextState,
      error: event.isError ? event.message : state.error,
      ...(shouldReopenProvisioningModal
        ? { provisioningModalDismissed: false }
        : {}),
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
  WireGuardConfigGenerated: "Managed tunnel config generated",
  WireGuardAppHandoffStarted: "Starting managed tunnel",
  WireGuardWaitingForImport: "Preparing managed tunnel",
  WireGuardWaitingForActivation: "Waiting for managed tunnel activation",
  WireGuardVerifying: "Verifying secure tunnel",
  WireGuardConnected: "Secure tunnel connected",
  MoonlightSunshineReadyToSetup: "Ready to set up Moonlight and Sunshine",
  SunshineCredentialsConfiguring: "Configuring Sunshine credentials",
  SunshineVerifying: "Verifying Sunshine",
  MoonlightDetecting: "Preparing embedded streaming",
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
  let provisioningEventQueue: Promise<void> = Promise.resolve();

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
    provisioningModalDismissed: false,
    error: null,
    _eventsBound: false,
    vastWalletSummary: null,
    sharedStorageSettings: null,
    storageProviders: [],
    sharedStorageProfiles: [],
    sharedStorageTestResult: null,
    oauthSessionId: null,
    backupStatus: null,
    instanceBackupStatus: null,
    sunshineSettings: null,
    embeddedMoonlightStatus: null,
    activeMoonlightPairing: null,
    instanceActionRunning: false,
    launchLibrary: null,
    launchLibraryLoading: false,
    launchSoftwareJob: null,
    launchingSoftwareAppId: null,
    softwareArtwork: {},
    softwareArtworkLoading: {},

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
        let vastWalletSummary: VastWalletSummary | null = null;
        if (
          appState.onboardingCompleted &&
          appState.credentials.vastApiKey.trim().length > 0
        ) {
          const [instances, wallet] = await Promise.all([
            getRentedInstances(),
            getVastWalletSummaryCommand().catch(() => null),
          ]);
          rentedInstances = await enrichRentedInstancesWithEmbeddedStatus(instances);
          vastWalletSummary = wallet;
        }

        set({
          appState: {
            ...appState,
            postWireguardSetup,
          },
          logs,
          rentedInstances,
          vastWalletSummary,
          provisioningModalDismissed: false,
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
        provisioningEventQueue = provisioningEventQueue
          .then(() => applyProvisioningEventState(event, set))
          .catch(() => undefined);
      });

      set({ _eventsBound: true });
    },


    dismissProvisioningModal: () => set({ provisioningModalDismissed: true }),

    reopenProvisioningModal: () => set({ provisioningModalDismissed: false }),

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
          const rentedInstances = await enrichRentedInstancesWithEmbeddedStatus(
            await getRentedInstances(),
          );
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
      return runBusyTask(
        {
          key: "server.select",
          label: "Selecting server offer",
          detail: "Applying the selected GPU host and storage size.",
          blocking: true,
        },
        async () => {
          const appState = await selectOffer(offerId, storageGb);
          set({ appState });
          return true;
        },
        false,
      );
    },

    startPlay: async () => {
      set({ provisioningModalDismissed: false });
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

    resumeProvisioningExisting: async (instanceId) => {
      set({ provisioningModalDismissed: false });
      beginProvisioningBlock("Resuming this instance from its saved provisioning checkpoint.");
      try {
        const mode = await resumeProvisioningExistingInstance(instanceId);
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
        const restoredPostWireguardCheckpoint =
          postWireguardSetup.stage !== "pre_wireguard_existing_flow";
        if (
          restoredPostWireguardCheckpoint ||
          PROVISIONING_INTERACTIVE_STATES.has(appState.orchestrationState)
        ) {
          endProvisioningBlock();
        }
        return mode;
      } catch (error) {
        endProvisioningBlock();
        set({ error: mapError(error) });
        return null;
      }
    },

    startPlayExisting: async (instanceId) => {
      set({ provisioningModalDismissed: false });
      beginProvisioningBlock("Reconnecting to your existing gaming instance.");
      try {
        const mode = await startPlayExistingInstance(instanceId);

        const appState = await getAppState();
        const embeddedMoonlightStatus = await getInstanceMoonlightPipelineStatus(instanceId).catch(() => null);
        set({ appState, embeddedMoonlightStatus });
        if (mode === "embedded") {
          endProvisioningBlock();
          return mode;
        }
        if (PROVISIONING_INTERACTIVE_STATES.has(appState.orchestrationState)) {
          endProvisioningBlock();
        }
        return mode;
      } catch (error) {
        endProvisioningBlock();
        set({ error: mapError(error) });
        return null;
      }
    },

    loadInstanceLaunchLibrary: async (instanceId) => {
      set({
        launchLibrary: null,
        launchLibraryLoading: true,
        launchSoftwareJob: null,
        launchingSoftwareAppId: null,
        error: null,
      });
      try {
        const launchLibrary = await getInstanceLaunchLibrary(instanceId);
        set({ launchLibrary, launchLibraryLoading: false });
        return launchLibrary;
      } catch (error) {
        set({ launchLibraryLoading: false, error: mapError(error) });
        return null;
      }
    },

    launchInstanceSoftware: async (instanceId, appId) => {
      set({
        launchingSoftwareAppId: appId,
        launchSoftwareJob: null,
        error: null,
      });
      try {
        const launchSoftwareJob = await launchInstanceSoftwareCommand(
          instanceId,
          appId,
        );
        set({ launchSoftwareJob, launchingSoftwareAppId: null });
        return launchSoftwareJob;
      } catch (error) {
        set({ launchingSoftwareAppId: null, error: mapError(error) });
        return null;
      }
    },

    pollLaunchSoftwareJob: async (jobId) => {
      try {
        const launchSoftwareJob = await getLaunchInstanceSoftwareJob(jobId);
        set({ launchSoftwareJob });
        return launchSoftwareJob;
      } catch (error) {
        set({ error: mapError(error) });
        return null;
      }
    },

    loadSoftwareArtwork: async (name) => {
      const artworkName = name.trim();
      if (!artworkName) {
        return null;
      }

      const existing = get().softwareArtwork[artworkName];
      if (existing) {
        return existing;
      }
      if (get().softwareArtworkLoading[artworkName]) {
        return null;
      }

      set((state) => ({
        softwareArtworkLoading: {
          ...state.softwareArtworkLoading,
          [artworkName]: true,
        },
      }));
      try {
        const result = await getSoftwareArtwork(artworkName);
        set((state) => ({
          softwareArtwork: {
            ...state.softwareArtwork,
            [artworkName]: result,
          },
          softwareArtworkLoading: {
            ...state.softwareArtworkLoading,
            [artworkName]: false,
          },
        }));
        return result;
      } catch {
        set((state) => ({
          softwareArtworkLoading: {
            ...state.softwareArtworkLoading,
            [artworkName]: false,
          },
        }));
        return null;
      }
    },

    clearLaunchLibrary: () => {
      set({
        launchLibrary: null,
        launchLibraryLoading: false,
        launchSoftwareJob: null,
        launchingSoftwareAppId: null,
      });
    },

    saveIgdbCredentials: async (payload) => {
      set({ busy: true, error: null });
      try {
        const appState = await updateIgdbCredentials(payload);
        set({
          appState,
          busy: false,
          softwareArtwork: {},
          softwareArtworkLoading: {},
        });
      } catch (error) {
        set({ busy: false, error: mapError(error) });
      }
    },

    loadRentedInstances: async () => {
      set({ busy: true, error: null });
      try {
        const rentedInstances = await enrichRentedInstancesWithEmbeddedStatus(
          await getRentedInstances(),
        );
        set({ rentedInstances, busy: false });
      } catch (error) {
        set({ busy: false, error: mapError(error) });
      }
    },

    saveVastApiKey: async (apiKey) => {
      set({ busy: true, error: null });
      try {
        const appState = await updateVastApiKey(apiKey);
        const [rentedInstances, vastWalletSummary] = await Promise.all([
          getRentedInstances(),
          getVastWalletSummaryCommand().catch(() => null),
        ]);
        set({
          appState,
          rentedInstances: await enrichRentedInstancesWithEmbeddedStatus(rentedInstances),
          vastWalletSummary,
          busy: false,
        });
      } catch (error) {
        set({ busy: false, error: mapError(error) });
      }
    },


    refreshVastWalletSummary: async () => {
      return runBusyTask(
        {
          key: "vast.wallet.summary",
          label: "Refreshing Vast.ai wallet",
          detail: "Fetching your current Vast.ai account balance.",
          blocking: false,
        },
        async () => {
          const summary = await getVastWalletSummaryCommand();
          set({ vastWalletSummary: summary });
          return summary;
        },
        null,
      );
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
          label: "Reconnecting managed tunnel",
          detail: "Restarting the local GotaTun-backed tunnel.",
        },
        async () => {
          const result = await reconnectLocalWireguardClientQuick();
          const appState = await getAppState();
          set({ appState });
          return result;
        },
        null,
      );
    },

    setupWireguardAppHandoff: async () => {
      return runBusyTask(
        {
          key: "wireguard.appHandoff",
          label: "Starting managed tunnel",
          detail: "Applying the generated config through the local GotaTun-backed tunnel flow.",
          blocking: true,
        },
        async () => {
          set({ provisioningModalDismissed: false });
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
            set({ provisioningModalDismissed: false });
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
            set({ provisioningModalDismissed: false });
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

    loadStorageProviders: async () => {
      try {
        const providers = await listStorageProviders();
        set({ storageProviders: providers });
      } catch (error) {
        set({ error: mapError(error) });
      }
    },

    connectStorageProvider: async (
      provider,
      credentials,
      bucket,
      prefix,
      displayName,
    ) => {
      await runBusyTask(
        {
          key: "storage.connect",
          label: "Connecting storage",
          blocking: true,
        },
        async () => {
          const credentialsJson = JSON.stringify(credentials);
          const profile = await saveStaticProviderCredentials(
            provider,
            credentialsJson,
            bucket,
            prefix,
            displayName,
          );
          await get().loadSharedStorageProfiles();
          return profile;
        },
        null as unknown as SharedStorageProfile,
      );
    },

    testStorageConnection: async (profileId) => {
      await runBusyTask(
        {
          key: "storage.test",
          label: "Testing connection",
        },
        async () => {
          const result = await testSharedStorageConnection(profileId);
          set({ sharedStorageTestResult: result });
          return result;
        },
        null as unknown as SharedStorageTestResult,
      );
    },

    loadSharedStorageProfiles: async () => {
      try {
        const profiles = await getSharedStorageProfiles();
        set({ sharedStorageProfiles: profiles });
      } catch (error) {
        set({ error: mapError(error) });
      }
    },

    setActiveStorageProfile: async (profileId) => {
      await runBusyTask(
        {
          key: "storage.profile.activate",
          label: "Switching storage profile",
        },
        async () => {
          await setActiveSharedStorageProfile(profileId);
          await get().loadSharedStorageProfiles();
        },
        undefined,
      );
    },

    disconnectStorageProfile: async (profileId) => {
      await runBusyTask(
        {
          key: "storage.disconnect",
          label: "Disconnecting storage",
        },
        async () => {
          await disconnectSharedStorageProfile(profileId);
          await get().loadSharedStorageProfiles();
        },
        undefined,
      );
    },

    syncActiveInstanceToSharedStorage: async () => {
      await runBusyTask(
        {
          key: "storage.sync.active-instance",
          label: "Whole-instance export removed",
          detail:
            "Use the shared storage export flow to choose specific files or folders instead of syncing the whole filesystem.",
          blocking: true,
        },
        async () => {
          const instanceId = get().appState?.instance.instanceId;
          if (!instanceId) {
            throw new Error(
              "No active instance selected. Start or select a server first.",
            );
          }
          throw new Error(
            "Whole-instance shared-storage export has been removed. Use Export Selected Files from the dashboard/shared storage UI.",
          );
        },
        null,
      );
    },

    beginOauthFlow: async (
      provider,
      displayName,
      clientId,
      clientSecret,
      providerFields,
    ) => {
      try {
        const response = await beginOauthAuthorization(
          provider,
          displayName,
          clientId || "",
          clientSecret || null,
          JSON.stringify(providerFields || {}),
        );
        set({ oauthSessionId: response.sessionId });
        if ("__TAURI_INTERNALS__" in window) {
          await openUrl(response.authorizationUrl);
        } else {
          window.open(response.authorizationUrl, "_blank", "noopener,noreferrer");
        }
        return response.sessionId;
      } catch (error) {
        set({ error: mapError(error) });
        return null;
      }
    },

    completeOauthFlow: async (sessionId) => {
      await runBusyTask(
        {
          key: "storage.oauth.complete",
          label: "Completing authorization",
        },
        async () => {
          const result = await completeOauthAuthorization(sessionId);
          set({ oauthSessionId: null, error: null });
          await get().loadSharedStorageProfiles();
          return result;
        },
        null as never,
      );
      // If the task failed, check whether it was a transient "still in
      // progress" error (the token exchange hasn't finished yet).  In that
      // case keep the session alive so the user can click "Complete
      // Authorization" again instead of being silently bounced back to the
      // start.
      const currentError = get().error;
      if (currentError) {
        const isStillInProgress = currentError
          .toLowerCase()
          .includes("still in progress");
        if (isStillInProgress) {
          // Keep oauthSessionId so the user can retry.
        } else {
          set({ oauthSessionId: null });
        }
      }
    },

    triggerBackup: async () => {
      set({ busy: true, error: null });
      try {
        const status = await triggerInstanceBackup();

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
          label: "Restoring application state",
          detail:
            "Downloading, verifying, and applying selected app bundles on the instance.",
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
          label: "Backing up application state",
          detail: "The state agent is packing, encrypting, and committing selected apps.",
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

    setEmbeddedMoonlightPipelineEnabled: async (instanceId, enabled) => {
      await runInstanceTask(
        {
          key: "instance.moonlight.pipeline",
          label: enabled ? "Enabling embedded Moonlight" : "Disabling embedded Moonlight",
          detail: enabled
            ? "Turning on the built-in Moonlight pipeline for this instance."
            : "Turning off the built-in Moonlight pipeline for this instance.",
        },
        async () => {
          const appState = await setInstanceMoonlightPipelineEnabled(instanceId, enabled);
          const rentedInstances = await getRentedInstances();
          const embeddedMoonlightStatus = enabled
            ? await getInstanceMoonlightPipelineStatus(instanceId)
            : null;
          set({
            appState,
            rentedInstances: applyEmbeddedMoonlightStatusToInstances(
              rentedInstances,
              embeddedMoonlightStatus,
            ),
            embeddedMoonlightStatus,
          });
        },
        undefined,
      );
    },

    loadEmbeddedMoonlightStatus: async (instanceId) => {
      return await runInstanceTask(
        {
          key: "instance.moonlight.status",
          label: "Loading embedded Moonlight status",
          detail: "Checking whether this instance is ready for the built-in stream pipeline.",
        },
        async () => {
          const embeddedMoonlightStatus = await getInstanceMoonlightPipelineStatus(instanceId);
          set((state) => ({
            embeddedMoonlightStatus,
            rentedInstances: applyEmbeddedMoonlightStatusToInstances(
              state.rentedInstances,
              embeddedMoonlightStatus,
            ),
          }));
          return embeddedMoonlightStatus;
        },
        null,
      );
    },

    prepareEmbeddedMoonlightPairing: async (instanceId) => {
      return await runInstanceTask(
        {
          key: "instance.moonlight.pair.begin",
          label: "Starting embedded Moonlight pairing",
          detail: "Generating a Sunshine pairing PIN for the built-in Moonlight pipeline.",
          blocking: true,
        },
        async () => {
          const session = await prepareInstanceMoonlightPairing(instanceId);
          const appState = await getAppState();
          const rentedInstances = await getRentedInstances();
          const embeddedMoonlightStatus = await getInstanceMoonlightPipelineStatus(instanceId);
          set({
            activeMoonlightPairing: session,
            appState,
            rentedInstances,
            embeddedMoonlightStatus,
          });
          return session;
        },
        null,
      );
    },

    completeEmbeddedMoonlightPairing: async (instanceId, sessionId) => {
      return await runInstanceTask(
        {
          key: "instance.moonlight.pair.complete",
          label: "Completing embedded Moonlight pairing",
          detail: "Finalizing Sunshine pairing for the built-in Moonlight pipeline.",
          blocking: true,
        },
        async () => {
          await completeInstanceMoonlightPairing(instanceId, sessionId);
          const appState = await getAppState();
          const rentedInstances = await getRentedInstances();
          const embeddedMoonlightStatus = await getInstanceMoonlightPipelineStatus(instanceId);
          set({
            activeMoonlightPairing: null,
            appState,
            rentedInstances,
            embeddedMoonlightStatus,
          });
          return true;
        },
        false,
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
          set((state) => ({
            appState,
            embeddedMoonlightStatus:
              state.embeddedMoonlightStatus?.instanceId === instanceId
                ? null
                : state.embeddedMoonlightStatus,
            activeMoonlightPairing: null,
          }));
        },
        undefined,
      );
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
