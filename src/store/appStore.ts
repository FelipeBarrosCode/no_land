import { create } from "zustand";
import { translate, translateSource } from "../lib/i18n";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { playArcadeSuccess } from "../lib/arcadeAudio";
import { isNotificationEnabled } from "../lib/notificationPreferences";
import { notifyProvisioningUpdate } from "../lib/provisioningNotifications";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  completeOnboarding,
  getAppState,
  saveAutoShutdownSettings as saveAutoShutdownSettingsCommand,
  getRentedInstances,
  getProvisioningLogs,
  runSystemHealthCheck,
  exportDiagnosticReport,
  searchOffers,
  listAvailableOfferCountries,
  selectOffer,
  setManualLocation,
  setupWireguardClient,
  reconnectLocalWireguardClientQuick,
  setupWireguardAppHandoff,
  resumeProvisioningExistingInstance,
  startPlayExistingInstance,
  startPlayFlow,
  stopProvisioningAfterCurrentStage as stopProvisioningAfterCurrentStageCommand,
  subscribeProvisioningEvents,
  subscribeSharedStorageProgress,
  subscribeSharedStorageRestoreCompleted,
  subscribeDirectUploadProgress,
  cancelSharedStorageOperation,
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
  cancelOauthAuthorization,
  completeOauthAuthorization,
  getInstanceLaunchLibrary,
  launchInstanceSoftware as launchInstanceSoftwareCommand,
  getLaunchInstanceSoftwareJob,
  getSoftwareArtwork,
  uploadPathsToInstance,
  getCloudflareTurnSettings,
  testCloudflareTurnSettings as testCloudflareTurnSettingsCommand,
  saveCloudflareTurnSettings as saveCloudflareTurnSettingsCommand,
  clearCloudflareTurnSettings as clearCloudflareTurnSettingsCommand,
} from "../lib/backend";
import { PROVISIONING_ORDER } from "../lib/constants";
import type { BlockingActionState } from "../components/ui/BlockingLoaderOverlay";
import type {
  AutoShutdownSettings,
  ManualLocationInput,
  MoonlightPreferences,
  OfferCandidate,
  OfferCountryAvailability,
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
  BackupPerformanceMode,
  BackupStatusResponse,
  SharedStorageInstanceStatus,
  SharedStorageObjectEntry,
  SharedStorageProgressEvent,
  DirectUploadProgressEvent,
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
  SystemHealthReport,
  DiagnosticReportResponse,
  CloudflareTurnSettingsResponse,
  CloudflareTurnSettingsUpdate,
  CloudflareTurnTestResult,
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
  provisioningStopRequested: boolean;
  error: string | null;
  _eventsBound: boolean;
  vastWalletSummary: VastWalletSummary | null;
  systemHealth: SystemHealthReport | null;
  healthChecking: boolean;
  lastDiagnosticReport: DiagnosticReportResponse | null;
  cloudflareTurnSettings: CloudflareTurnSettingsResponse | null;
  cloudflareTurnTestResult: CloudflareTurnTestResult | null;
  initialize: () => Promise<void>;
  bindEvents: () => Promise<void>;
  dismissProvisioningModal: () => void;
  reopenProvisioningModal: () => void;
  stopProvisioningAfterCurrentStage: () => Promise<void>;
  runOnboarding: (payload: OnboardingPayload) => Promise<void>;
  saveManualLocation: (payload: ManualLocationInput) => Promise<void>;
  discoverOffers: (page?: number) => Promise<void>;
  nextOffersPage: () => Promise<void>;
  previousOffersPage: () => Promise<void>;
  loadAvailableOfferCountries: () => Promise<
    OfferCountryAvailability[] | null
  >;
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
  saveAutoShutdownSettings: (settings: AutoShutdownSettings) => Promise<void>;
  saveServerPreferences: (
    payload: Partial<ServerPreferencesUpdate>,
  ) => Promise<void>;
  saveMoonlightPreferences: (payload: MoonlightPreferences) => Promise<void>;
  saveSshCredentials: (payload: SshCredentialsUpdate) => Promise<void>;
  loadCloudflareTurnSettings: () => Promise<void>;
  testCloudflareTurnSettings: (
    payload: CloudflareTurnSettingsUpdate,
  ) => Promise<CloudflareTurnTestResult | null>;
  saveCloudflareTurnSettings: (
    payload: CloudflareTurnSettingsUpdate,
  ) => Promise<void>;
  clearCloudflareTurnSettings: () => Promise<void>;
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
  cancelOauthFlow: (sessionId: string) => Promise<void>;
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
    performanceMode: BackupPerformanceMode,
  ) => Promise<string | null>;
  cancelSharedStorageOperation: (instanceId: number) => Promise<void>;
  clearBackgroundStorageAction: (operationId: string) => void;
  uploadPathsToRemoteInstance: (
    instanceId: number,
    localPaths: string[],
    destination?: string,
  ) => Promise<void>;
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
  refreshSystemHealth: () => Promise<SystemHealthReport | null>;
  exportCrashReport: (reason?: string, frontendError?: string) => Promise<DiagnosticReportResponse | null>;
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

  return translate("generated.66523a826df69fac");
}

function serializeErrorForReport(error: unknown): string {
  if (error instanceof Error) {
    return `${error.name}: ${error.message}\n${error.stack ?? ""}`.trim();
  }

  if (typeof error === "string") {
    return error;
  }

  try {
    return JSON.stringify(error, null, 2);
  } catch {
    return String(error);
  }
}

interface AsyncActionOptions {
  key: string;
  label: string;
  detail?: string;
  blocking?: boolean;
  background?: boolean;
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
  const needsUserAttention =
    event.isError ||
    event.state === "WireGuardConfigGenerated" ||
    event.state === "WireGuardWaitingForActivation" ||
    event.state === "WireGuardConnected" ||
    event.state === "AwaitingPairPin";
  const provisioningFinished = event.state === "Ready" && !event.isError;
  if (needsUserAttention || provisioningFinished) {
    void notifyProvisioningUpdate(
      provisioningFinished ? "complete" : "attention",
        provisioningFinished ? translateSource("Your instance is ready to use.") : translateSource(event.message),
        event.details ? translateSource(event.details) : event.details,
    );
  }

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

    if (state.provisioningStopRequested) {
      // Keep the overlay dismissed while the backend finishes the current stage.
      // In particular, repeated WaitingForInstance events must not reopen it.
      updates.blockingAction = null;
      updates.isBlocking = false;
      if (event.state === "Idle" || event.state === "Error") {
        updates.busy = false;
        updates.provisioningStopRequested = false;
      } else {
        // The user has already stopped the UI flow. Keep surrounding controls
        // usable while the backend finishes the current safe stage.
        updates.busy = false;
      }
      return updates;
    }

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
      label: translate("generated.83fae8da085af6f1"),
      detail:
        translateSource(event.message ||
        PROVISIONING_STEP_LABELS[event.state] ||
        "Preparing your instance"),
      progress: getProvisioningProgress(event.state),
      mode: "determinate",
      stage: event.state,
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

const SHARED_STORAGE_ACTION_KEYS = new Set([
  "instance.storage.export",
  "instance.storage.sync",
]);

function formatTransferBytes(bytes: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = Math.max(0, bytes);
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  const digits = value >= 100 || unitIndex === 0 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(digits)} ${units[unitIndex]}`;
}

const notifiedRestoreOperations = new Set<string>();

async function notifyStorageCompletion(body: string) {
  if (!isNotificationEnabled("storage")) {
    return;
  }
  playArcadeSuccess();
  try {
    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (granted) {
      await sendNotification({
        title: "Noland Connect",
        body: translateSource(body),
        icon: "icons/icon.png",
        // Use the platform notification sound, independently of the optional
        // in-app arcade sound setting.
        sound: "Ping",
        silent: false,
      });
    }
  } catch (error: unknown) {
    console.warn("[shared-storage] completion notification failed", error);
  }
}

async function handleSharedStorageRestoreCompleted(
  event: import("../lib/types").SharedStorageRestoreCompletedEvent,
  get: () => AppStore,
) {
  const blockingAction = get().blockingAction;
  if (blockingAction &&
    ((blockingAction.operationId && blockingAction.operationId !== event.operationId) ||
      (blockingAction.instanceId != null && blockingAction.instanceId !== event.instanceId))
  ) {
    return;
  }
  if (notifiedRestoreOperations.has(event.operationId)) {
    return;
  }
  notifiedRestoreOperations.add(event.operationId);
  get().clearBackgroundStorageAction(event.operationId);
  await notifyStorageCompletion(`${event.displayName || "Your shared storage restore"} restore completed.`);
}

function applySharedStorageProgress(
  event: SharedStorageProgressEvent,
  set: (partial: Partial<AppStore> | ((state: AppStore) => Partial<AppStore>)) => void,
) {
  set((state) => {
    if (
      !state.blockingAction ||
      !SHARED_STORAGE_ACTION_KEYS.has(state.blockingAction.key)
    ) {
      return {};
    }
    if (
      (state.blockingAction.operationId &&
        state.blockingAction.operationId !== event.operationId) ||
      (state.blockingAction.instanceId != null &&
        state.blockingAction.instanceId !== event.instanceId)
    ) {
      return {};
    }
    const transferPhase = /upload|download/i.test(event.phase ?? event.state);
    const transferFraction = transferPhase
      ? event.completedBytes != null && event.totalBytes != null && event.totalBytes > 0
        ? event.completedBytes / event.totalBytes
        : event.completedObjects != null &&
            event.totalObjects != null &&
            event.totalObjects > 0
          ? event.completedObjects / event.totalObjects
          : null
      : null;
    const percent =
      typeof transferFraction === "number"
        ? Math.round(Math.max(0, Math.min(1, transferFraction)) * 100)
        : typeof event.fraction === "number"
          ? Math.round(event.fraction * 100)
          : null;
    const objects =
      event.completedObjects != null && event.totalObjects != null
        ? `${event.completedObjects}/${event.totalObjects} ${event.objectUnit ?? "objects"}`
        : null;
    const isDownloadPhase = /download/i.test(event.phase ?? event.state);
    const completedBytesLabel = isDownloadPhase ? "downloaded" : "ready";
    const bytes =
      event.completedBytes != null
        ? event.totalBytes != null && event.totalBytes > 0
          ? `${formatTransferBytes(event.completedBytes)}/${formatTransferBytes(event.totalBytes)} ${completedBytesLabel}`
          : `${formatTransferBytes(event.completedBytes)} ${completedBytesLabel}`
        : null;
    const transferVerb = isDownloadPhase ? "downloaded" : "uploaded";
    const transferred =
      event.transferredBytes != null
        ? `${formatTransferBytes(event.transferredBytes)} ${transferVerb}`
        : null;
    const operationUnits =
      event.completedUnits != null && event.totalUnits != null
        ? `${event.completedUnits}/${event.totalUnits}${event.unit ? ` ${event.unit}` : ""}`
        : null;
    const units = transferPhase ? (objects ?? operationUnits) : operationUnits;
    const phaseLabel = event.readyToLaunch
      ? "Ready to launch"
      : event.phase
        ? event.phase.replace(/_/g, " ")
        : event.state;
    return {
      blockingAction: {
        ...state.blockingAction,
        label: phaseLabel,
        detail:
          [event.message, units, transferPhase ? bytes : null, transferPhase ? transferred : null]
            .filter(Boolean)
            .join(" · ") ||
          state.blockingAction.detail,
        progress: percent,
        mode: percent == null ? "indeterminate" : "determinate",
        cancellable: event.cancellable,
        cancelRequested: event.cancelRequested,
        operationId: event.operationId,
        instanceId: event.instanceId,
      },
    };
  });
}

function applyDirectUploadProgress(
  event: DirectUploadProgressEvent,
  set: (partial: Partial<AppStore> | ((state: AppStore) => Partial<AppStore>)) => void,
) {
  set((state) => {
    const action = state.blockingAction;
    if (!action || action.key !== "instance.files.upload") return {};
    if (action.instanceId != null && action.instanceId !== event.instanceId) return {};
    const objects = `${event.completedObjects}/${event.totalObjects} items`;
    const bytes = `${formatTransferBytes(event.completedBytes)}/${formatTransferBytes(event.totalBytes)}`;
    return {
      blockingAction: {
        ...action,
        operationId: event.operationId,
        instanceId: event.instanceId,
        detail: [event.message, objects, bytes].filter(Boolean).join(" · "),
        progress: Math.round(event.fraction * 100),
        mode: "determinate",
        cancellable: false,
      },
    };
  });
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

  const createCrashReport = async (
    reason = "error",
    frontendError?: string,
  ): Promise<DiagnosticReportResponse | null> => {
    try {
      const report = await exportDiagnosticReport({ reason, frontendError });
      set({ lastDiagnosticReport: report });
      return report;
    } catch {
      return null;
    }
  };

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
      const message = mapError(error);
      set({ error: message });
      void createCrashReport(options.key, serializeErrorForReport(error));
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

    if (options.background || options.blocking) {
      set((state) => ({
        blockingAction: createBlockingAction(state, {
          key: options.key,
          label: options.label,
          detail: options.detail,
          progress: null,
          mode: "indeterminate",
        }),
        isBlocking: options.blocking === true,
      }));
    }

    if (options.background) {
      set({ instanceActionRunning: false, busy: false });
      void task()
        .catch((error) => {
          const message = mapError(error);
          set({ error: message });
          void createCrashReport(options.key, serializeErrorForReport(error));
        })
        .finally(() => {
          set((state) => ({
            ...(state.blockingAction?.key === options.key
              ? { blockingAction: null }
              : {}),
          }));
        });
      return fallback;
    }

    try {
      return await task();
    } catch (error) {
      const message = mapError(error);
      set({ error: message });
      void createCrashReport(options.key, serializeErrorForReport(error));
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
        label: translate("generated.83fae8da085af6f1"),
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
    provisioningStopRequested: false,
    error: null,
    _eventsBound: false,
    vastWalletSummary: null,
    systemHealth: null,
    healthChecking: false,
    lastDiagnosticReport: null,
    cloudflareTurnSettings: null,
    cloudflareTurnTestResult: null,
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
        const [appState, logs, postWireguardSetup, systemHealth, cloudflareTurnSettings] = await Promise.all([
          getAppState(),
          getProvisioningLogs(),
          getSetupStatus(),
          runSystemHealthCheck().catch(() => null),
          getCloudflareTurnSettings().catch(() => null),
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
          sharedStorageProfiles: appState.sharedStorageProfiles ?? [],
          vastWalletSummary,
          systemHealth,
          cloudflareTurnSettings,
          provisioningModalDismissed: false,
          loading: false,
        });
      } catch (error) {
        const message = mapError(error);
        set({ loading: false, error: message });
        void createCrashReport("app.initialize", serializeErrorForReport(error));
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
      await subscribeSharedStorageProgress((event) => {
        applySharedStorageProgress(event, set);
      });
      await subscribeSharedStorageRestoreCompleted((event) => {
        void handleSharedStorageRestoreCompleted(event, get);
      });
      await subscribeDirectUploadProgress((event) => {
        applyDirectUploadProgress(event, set);
      });

      set({ _eventsBound: true });
    },


    dismissProvisioningModal: () => set({ provisioningModalDismissed: true }),

    reopenProvisioningModal: () => set({ provisioningModalDismissed: false }),

    stopProvisioningAfterCurrentStage: async () => {
      if (get().provisioningStopRequested) {
        return;
      }

      // Dismiss synchronously. The backend stop request may take until the current
      // stage reaches a safe boundary, but the user should never wait on that RPC
      // for the loading modal to close.
      set({
        provisioningStopRequested: true,
        provisioningModalDismissed: true,
        busy: false,
        blockingAction: null,
        isBlocking: false,
        error: null,
      });
      try {
        await stopProvisioningAfterCurrentStageCommand();
      } catch (error) {
        set({
          provisioningStopRequested: false,
          busy: false,
          error: mapError(error),
        });
      }
    },

    runOnboarding: async (payload) => {
      await runBusyTask(
        {
          key: "onboarding.setup",
          label: translate("generated.2cdfbc3a3d292f50"),
          detail: translate("generated.b408c35971eea6b2"),
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
          label: translate("generated.3d4d51046bee2119"),
          detail: translate("generated.4c9fcb0c035aa50c"),
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

    loadAvailableOfferCountries: async () => {
      try {
        return await listAvailableOfferCountries();
      } catch (error) {
        console.error("Failed to load available offer countries", error);
        return null;
      }
    },

    chooseOffer: async (offerId, storageGb) => {
      return runBusyTask(
        {
          key: "server.select",
          label: translate("generated.5aac0f6894e3a6ca"),
          detail: translate("generated.387bf210b7ee9e4f"),
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
      set((state) => ({
        provisioningModalDismissed: false,
        provisioningStopRequested: false,
        appState: state.appState
          ? { ...state.appState, orchestrationState: "CreatingInstance" }
          : state.appState,
      }));
      beginProvisioningBlock(
        translate("generated.50e4a7cd052ffc5b"),
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
      set({
        provisioningModalDismissed: false,
        provisioningStopRequested: false,
      });
      beginProvisioningBlock(translate("generated.0cad13633ca98925"));
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
      beginProvisioningBlock(translate("generated.39caee49c0f6c0c0"));
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
          label: translate("generated.b4b7b6c350e77d3e"),
          detail: translate("generated.0334905e355667d6"),
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
          label: translate("generated.2521114569981723"),
          detail: translate("generated.9ef0d1b429d68ee2"),
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

    loadCloudflareTurnSettings: async () => {
      try {
        const cloudflareTurnSettings = await getCloudflareTurnSettings();
        set({ cloudflareTurnSettings });
      } catch (error) {
        set({ error: mapError(error) });
      }
    },

    testCloudflareTurnSettings: async (payload) => {
      set({ busy: true, error: null, cloudflareTurnTestResult: null });
      try {
        const result = await testCloudflareTurnSettingsCommand(payload);
        set({ busy: false, cloudflareTurnTestResult: result });
        return result;
      } catch (error) {
        set({ busy: false, error: mapError(error) });
        return null;
      }
    },

    saveCloudflareTurnSettings: async (payload) => {
      set({ busy: true, error: null });
      try {
        const cloudflareTurnSettings = await saveCloudflareTurnSettingsCommand(payload);
        const appState = await getAppState();
        set({ busy: false, appState, cloudflareTurnSettings, cloudflareTurnTestResult: null });
      } catch (error) {
        set({ busy: false, error: mapError(error) });
      }
    },

    clearCloudflareTurnSettings: async () => {
      set({ busy: true, error: null });
      try {
        const cloudflareTurnSettings = await clearCloudflareTurnSettingsCommand();
        const appState = await getAppState();
        set({ busy: false, appState, cloudflareTurnSettings, cloudflareTurnTestResult: null });
      } catch (error) {
        set({ busy: false, error: mapError(error) });
      }
    },

    regenerateEdid: async (payload) => {
      await runBusyTask(
        {
          key: "settings.edid.regenerate",
          label: translate("generated.7bc7f18667d6292e"),
          detail: translate("generated.954d774bb4958e32"),
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
          label: translate("generated.8098b515e447f693"),
          detail: translate("generated.e6b39e688c3860fd"),
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
          label: translate("generated.aabbaeeacad4f4ce"),
          detail: translate("generated.3e79c8197ad6505b"),
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
          label: translate("generated.90761a385f94f43b"),
          detail: translate("generated.7d2e7707685d9a47"),
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
          label: translate("generated.760776423c921b81"),
          detail: translate("generated.0763c57dbc8bef1a"),
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
          label: translate("generated.8223cc2719eed72f"),
          detail: translate("generated.df342aa9339c72df"),
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
          label: translate("generated.9aee8c9595c041c0"),
          detail: translate("generated.e41be2004d42571a"),
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
          label: translate("generated.d9de282938c0ee98"),
          detail: translate("generated.ef55d2a88ab577c8"),
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
          label: translate("generated.87ac872c323d842b"),
          detail: translate("generated.addaa0f4cf57fd2e"),
        },
        async () => {
          const settings = await getSharedStorageSettings();
          set({ sharedStorageSettings: settings });
        },
        undefined,
      );
    },

    saveAutoShutdownSettings: async (settings) => {
      await runBusyTask(
        {
          key: "settings.auto-shutdown",
          label: translate("generated.04faf7b562ab89a9"),
          detail: translate("generated.4433c51386acf4c6"),
        },
        async () => {
          const appState = await saveAutoShutdownSettingsCommand(settings);
          set({ appState });
        },
        undefined,
      );
    },

    saveSharedStorageSettings: async (payload) => {
      await runBusyTask(
        {
          key: "storage.settings.save",
          label: translate("generated.b935b0e63f135682"),
          detail: translate("generated.c57f7dddbf6b4327"),
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
          label: translate("generated.eb27b7e8ded23881"),
          detail: translate("generated.2ccad01060281a32"),
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
          label: translate("generated.2c7ddc59b61759fd"),
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
          label: translate("generated.04dfed9c1dcdcb4a"),
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
          label: translate("generated.60b763646d4c3fd7"),
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
          label: translate("generated.f1ac1eaaca0ce497"),
        },
        async () => {
          await disconnectSharedStorageProfile(profileId);
          await get().loadSharedStorageProfiles();
          set({ sharedStorageTestResult: null });
        },
        undefined,
      );
    },

    syncActiveInstanceToSharedStorage: async () => {
      await runBusyTask(
        {
          key: "storage.sync.active-instance",
          label: translate("generated.42e34f252c7666b0"),
          detail:
            translate("generated.33116ae646f2cd4e"),
          blocking: true,
        },
        async () => {
          const instanceId = get().appState?.instance.instanceId;
          if (!instanceId) {
            throw new Error(
              translate("generated.2a5f6344635d6380"),
            );
          }
          throw new Error(
            translate("generated.7083e842fe19eac2"),
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

    cancelOauthFlow: async (sessionId) => {
      try {
        await cancelOauthAuthorization(sessionId);
        set({ oauthSessionId: null, error: null });
      } catch (error) {
        set({ error: mapError(error) });
      }
    },

    completeOauthFlow: async (sessionId) => {
      await runBusyTask(
        {
          key: "storage.oauth.complete",
          label: translate("generated.3e9fecfcc6f84c62"),
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
          label: translate("generated.38928c388adf6720"),
          detail:
            translate("generated.ddf87f6b45c36b44"),
          blocking: false,
          background: true,
        },
        async () => {
          console.info("[shared-storage] sync start", {
            instanceId,
            selectedCount: selectedPaths.length,
          });
          if (selectedPaths.length === 0) {
            throw new Error(translate("generated.54cc33a5d2f591d4"));
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

    saveInstanceStorageSelected: async (
      instanceId,
      selectedPaths,
      performanceMode,
    ) => {
      return await runInstanceTask(
        {
          key: "instance.storage.export",
          label: translate("generated.6e9fc8a84aaaa28a"),
          detail: translate("generated.744d5005ed49ebf5"),
          blocking: false,
          background: true,
        },
        async () => {
          const result = await saveInstanceToSharedStorageSelected(
            instanceId,
            selectedPaths,
            performanceMode,
          );
          await notifyStorageCompletion(translate("generated.57893731ab390a3d"));
          return result;
        },
        null,
      );
    },

    cancelSharedStorageOperation: async (instanceId) => {
      try {
        await cancelSharedStorageOperation(instanceId);
        set((state) => ({
          blockingAction: state.blockingAction
            ? {
                ...state.blockingAction,
                cancelRequested: true,
                cancellable: false,
                detail: translate("generated.acf1e4361a83466b"),
              }
            : null,
        }));
      } catch (error) {
        set({ error: mapError(error) });
      }
    },

    clearBackgroundStorageAction: (operationId) => {
      set((state) => {
        const action = state.blockingAction;
        return action?.operationId === operationId && SHARED_STORAGE_ACTION_KEYS.has(action.key)
          ? { blockingAction: null }
          : {};
      });
    },

    uploadPathsToRemoteInstance: async (instanceId, localPaths, destination) => {
      await runInstanceTask(
        {
          key: "instance.files.upload",
          label: translate("generated.e91ca92cc5ad0131"),
          detail: translate("generated.f49ff416f3bf8c04"),
          blocking: false,
          background: true,
        },
        async () => {
          const result = await uploadPathsToInstance(instanceId, localPaths, destination);
          await notifyStorageCompletion(
            `${result.uploadedObjects} items uploaded to ${result.destination}.`,
          );
        },
        undefined,
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
        error: translate("storage.scheduled.disabled.manual"),
      });
      return null;
    },

    removeBackupSchedule: async () => {
      set({
        error: translate("storage.scheduled.disabled.none"),
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
          label: translate("generated.945fb00f0760ef39"),
          detail:
            translate("generated.3443a1150ce732cc"),
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
          label: translate("generated.f53602b17f93e906"),
          detail:
            translate("generated.c1ae07f1a3806161"),
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
          label: translate("generated.24922a092534f322"),
          detail:
            translate("generated.fa24897734cbdaf5"),
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
          label: translate(enabled ? "moonlight.enabling" : "moonlight.disabling"),
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
          label: translate("generated.0358066e035e3c63"),
          detail: translate("generated.70ede56625b44c42"),
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
          label: translate("generated.15bedff6d793f86a"),
          detail: translate("generated.c1a13260aa639112"),
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
          label: translate("generated.4f3885d0851d75b3"),
          detail: translate("generated.6a8d371fa5d4d888"),
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
          label: translate("generated.f87a66adc3a6aab5"),
          detail:
            translate("generated.3fd8445568ced854"),
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
          label: translate("generated.0c9df74dc6eeab50"),
          detail:
            translate("generated.9bb868b4f3f40791"),
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

    refreshSystemHealth: async () => {
      set({ healthChecking: true });
      try {
        const report = await runSystemHealthCheck();
        set({ systemHealth: report, healthChecking: false });
        return report;
      } catch (error) {
        const message = mapError(error);
        set({ healthChecking: false, error: message });
        void createCrashReport("system.health", serializeErrorForReport(error));
        return null;
      }
    },

    exportCrashReport: async (reason, frontendError) => {
      return createCrashReport(reason ?? "manual", frontendError ?? get().error ?? undefined);
    },

    clearError: () => set({ error: null }),
  };
});
