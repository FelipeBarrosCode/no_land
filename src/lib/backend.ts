import { listen } from "@tauri-apps/api/event";
import { invokeSafe } from "./tauri";
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
  SharedStorageSettingsResponse,
  SharedStorageSettingsUpdate,
  SharedStorageProfile,
  SharedStorageTestResult,
  ProviderDefinition,
  ProfileReference,
  BackupStatusResponse,
  SharedStorageInstanceStatus,
  SharedStorageObjectEntry,
  SunshineSettingsResponse,
  BundleIndex,
  RestoreRequest,
  RestoreDryRunResult,
  RestoreJob,
  InstanceMicConfig,
  InstanceMicRuntimeStatus,
  MicSessionResponse,
  MicSettingsUpdate,
  MicQualityProfile,
  MicrophoneDevice,
  MoonlightDetectionResult,
  MoonlightPairingSessionResponse,
  EmbeddedMoonlightInstanceStatus,
  PostWireGuardSetupState,
  ReachabilityResult,
  SetupStage,
  SunshineVerificationResult,
  VastWalletSummary,
} from "./types";

export async function getAppState(): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("get_app_state");
}

export async function completeOnboarding(
  payload: OnboardingPayload,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("complete_onboarding", { payload });
}

export async function refreshIpLocation(): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("refresh_ip_location");
}

export async function setManualLocation(
  payload: ManualLocationInput,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("set_manual_location", { payload });
}

export async function setOsLocation(
  payload: ManualLocationInput,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("set_os_location", { payload });
}

export async function searchOffers(
  page = 1,
  pageSize = 24,
): Promise<OfferCandidate[]> {
  return invokeSafe<OfferCandidate[]>("search_offers", { page, pageSize });
}

export async function selectOffer(
  offerId: number,
  storageGb: number,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("select_offer", { offerId, storageGb });
}

export async function startPlayFlow(): Promise<void> {
  await invokeSafe<void>("start_play_flow");
}

export async function startPlayExistingInstance(
  instanceId: number,
): Promise<string> {
  return invokeSafe<string>("start_play_existing_instance", { instanceId });
}

export async function submitPairingPin(
  pin: string,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("submit_pairing_pin", { pin });
}

export async function skipPairingAndContinue(): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("skip_pairing_and_continue");
}

export async function setupWireguardClient(): Promise<string> {
  return invokeSafe<string>("setup_wireguard_client");
}

export async function reconnectLocalWireguardClientQuick(): Promise<string> {
  return invokeSafe<string>("reconnect_local_wireguard_client_quick");
}

export async function setupWireguardAppHandoff(): Promise<PostWireGuardSetupState> {
  return invokeSafe<PostWireGuardSetupState>(
    "setup_wireguard_app_handoff_command",
  );
}

export async function verifyWireguard(): Promise<ReachabilityResult> {
  return invokeSafe<ReachabilityResult>("verify_wireguard");
}


export async function getSetupStatus(): Promise<PostWireGuardSetupState> {
  return invokeSafe<PostWireGuardSetupState>("get_setup_status_command");
}

export async function verifySunshine(): Promise<SunshineVerificationResult> {
  return invokeSafe<SunshineVerificationResult>("verify_sunshine");
}

export async function detectMoonlight(): Promise<MoonlightDetectionResult> {
  return invokeSafe<MoonlightDetectionResult>("detect_moonlight");
}

export async function setupMoonlightSunshine(): Promise<PostWireGuardSetupState> {
  return invokeSafe<PostWireGuardSetupState>(
    "setup_moonlight_sunshine_command",
  );
}

export async function submitMoonlightPinToSunshine(
  pin: string,
): Promise<PostWireGuardSetupState> {
  return invokeSafe<PostWireGuardSetupState>(
    "submit_moonlight_pin_to_sunshine_command",
    { pin },
  );
}

export async function retrySetupStage(
  stage: SetupStage,
): Promise<PostWireGuardSetupState> {
  return invokeSafe<PostWireGuardSetupState>("retry_setup_stage_command", {
    stage,
  });
}

export async function startLocalSleepPrevention(): Promise<string> {
  return invokeSafe<string>("start_local_sleep_prevention");
}

export async function stopLocalSleepPrevention(): Promise<string> {
  return invokeSafe<string>("stop_local_sleep_prevention");
}

export async function getProvisioningLogs(): Promise<ProvisioningEvent[]> {
  return invokeSafe<ProvisioningEvent[]>("get_provisioning_logs");
}


export async function getRentedInstances(): Promise<RentedInstanceSummary[]> {
  return invokeSafe<RentedInstanceSummary[]>("get_rented_instances");
}

export async function updateVastApiKey(
  apiKey: string,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_vast_api_key", { apiKey });
}


export async function getVastWalletSummary(): Promise<VastWalletSummary> {
  return invokeSafe<VastWalletSummary>("get_vast_wallet_summary");
}


export async function updatePlatformCredentials(
  payload: PlatformCredentialsUpdate,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_platform_credentials", {
    payload,
  });
}

export async function updateServerPreferences(
  payload: ServerPreferencesUpdate,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_server_preferences", {
    payload,
  });
}

export async function updateMoonlightPreferences(
  payload: MoonlightPreferences,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_moonlight_preferences", {
    payload,
  });
}

export async function setInstanceMoonlightPipelineEnabled(
  instanceId: number,
  enabled: boolean,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("set_instance_moonlight_pipeline_enabled", {
    instanceId,
    enabled,
  });
}

export async function getInstanceMoonlightPipelineStatus(
  instanceId: number,
): Promise<EmbeddedMoonlightInstanceStatus> {
  return invokeSafe<EmbeddedMoonlightInstanceStatus>(
    "moonlight_get_instance_pipeline_status",
    { instanceId },
  );
}

export async function prepareInstanceMoonlightPairing(
  instanceId: number,
): Promise<MoonlightPairingSessionResponse> {
  return invokeSafe<MoonlightPairingSessionResponse>(
    "moonlight_prepare_instance_pairing",
    { instanceId },
  );
}

export async function completeInstanceMoonlightPairing(
  instanceId: number,
  sessionId: string,
): Promise<{ hostId: string; persisted: boolean }> {
  return invokeSafe<{ hostId: string; persisted: boolean }>(
    "moonlight_complete_instance_pairing",
    { instanceId, input: { sessionId } },
  );
}

export async function moonlightGetActiveInputMode(): Promise<
  "relative" | "absolute" | null
> {
  const response = await invokeSafe<{ mouseMode: "relative" | "absolute" | null }>(
    "moonlight_get_active_input_mode",
  );
  return response.mouseMode;
}

export async function moonlightStartInputCapture(
  mode: "relative" | "absolute",
): Promise<boolean> {
  return invokeSafe<boolean>("moonlight_start_input_capture", {
    input: { mode },
  });
}

export async function moonlightStopInputCapture(): Promise<boolean> {
  return invokeSafe<boolean>("moonlight_stop_input_capture");
}

export async function moonlightUpdateVideoGeometry(input: {
  left: number;
  top: number;
  width: number;
  height: number;
}): Promise<void> {
  await invokeSafe<void>("moonlight_update_video_geometry", {
    input: {
      left: input.left,
      top: input.top,
      width: input.width,
      height: input.height,
    },
  });
}

export async function moonlightActivateNativeMouseCapture(): Promise<boolean> {
  return invokeSafe<boolean>("moonlight_activate_native_mouse_capture");
}

export async function moonlightDeactivateNativeMouseCapture(): Promise<boolean> {
  return invokeSafe<boolean>("moonlight_deactivate_native_mouse_capture");
}

export async function moonlightDisconnectStream(): Promise<{
  state: string;
}> {
  return invokeSafe<{ state: string }>("moonlight_disconnect_stream");
}

export async function moonlightSendRelativeMouse(input: {
  deltaX: number;
  deltaY: number;
}): Promise<void> {
  await invokeSafe<void>("moonlight_send_relative_mouse", {
    input: {
      delta_x: input.deltaX,
      delta_y: input.deltaY,
    },
  });
}

export async function moonlightSendAbsoluteMouse(input: {
  x: number;
  y: number;
  referenceWidth: number;
  referenceHeight: number;
}): Promise<void> {
  await invokeSafe<void>("moonlight_send_absolute_mouse", {
    input: {
      x: input.x,
      y: input.y,
      reference_width: input.referenceWidth,
      reference_height: input.referenceHeight,
    },
  });
}

export async function moonlightSendMouseButton(input: {
  button: number;
  pressed: boolean;
}): Promise<void> {
  await invokeSafe<void>("moonlight_send_mouse_button", {
    input: {
      button: input.button,
      pressed: input.pressed,
    },
  });
}

export async function moonlightSendKeyboard(input: {
  virtualKey: number;
  pressed: boolean;
  modifiers: number;
}): Promise<void> {
  await invokeSafe<void>("moonlight_send_keyboard", {
    input: {
      virtual_key: input.virtualKey,
      pressed: input.pressed,
      modifiers: input.modifiers,
    },
  });
}

export async function moonlightGetInputDebugState(): Promise<{
  captureActive: boolean;
  captureMode: number;
  captureRequests: number;
  nativeMouseMoves: number;
  nativeMouseDowns: number;
  nativeMouseUps: number;
  nativeKeys: number;
  rustRelativeCallbacks: number;
  rustAbsoluteCallbacks: number;
  rustButtonCallbacks: number;
  rustKeyCallbacks: number;
  relativeSendAttempts: number;
  absoluteSendAttempts: number;
  buttonSendAttempts: number;
  keySendAttempts: number;
  scrollSendAttempts: number;
  sendErrors: number;
}> {
  return invokeSafe("moonlight_get_input_debug_state");
}

export async function moonlightGetSessionState(): Promise<{
  state: string;
}> {
  return invokeSafe<{ state: string }>("moonlight_get_session_state");
}

export async function regenerateEdid(payload: {
  mode: "auto_detect" | "manual";
  refreshRateHz: number;
}): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("regenerate_edid", { payload });
}

export async function updateSshCredentials(
  payload: SshCredentialsUpdate,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_ssh_credentials", { payload });
}

export async function subscribeProvisioningEvents(
  callback: (event: ProvisioningEvent) => void,
): Promise<() => void> {
  const unlisten = await listen<ProvisioningEvent>(
    "orchestration:progress",
    ({ payload }) => {
      callback(payload);
    },
  );

  return () => {
    unlisten();
  };
}

export async function getSharedStorageSettings(): Promise<SharedStorageSettingsResponse> {
  return invokeSafe<SharedStorageSettingsResponse>(
    "get_shared_storage_settings",
  );
}

export async function saveSharedStorageSettings(
  payload: SharedStorageSettingsUpdate,
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("save_shared_storage_settings", {
    payload,
  });
}

export async function testSharedStorageConfig(): Promise<string> {
  return invokeSafe<string>("test_shared_storage_config");
}

export async function listStorageProviders(): Promise<ProviderDefinition[]> {
  return invokeSafe<ProviderDefinition[]>("list_storage_providers");
}

export async function saveStaticProviderCredentials(
  provider: string,
  credentialsJson: string,
  bucket: string | null,
  prefix: string | null,
  displayName: string,
): Promise<SharedStorageProfile> {
  return invokeSafe<SharedStorageProfile>("save_static_provider_credentials", {
    provider,
    credentialsJson,
    bucket,
    prefix,
    displayName,
  });
}

export async function testSharedStorageConnection(
  profileId: string,
): Promise<SharedStorageTestResult> {
  return invokeSafe<SharedStorageTestResult>("test_shared_storage_connection", {
    profileId,
  });
}

export async function getSharedStorageProfiles(): Promise<ProfileReference[]> {
  return invokeSafe<ProfileReference[]>("get_shared_storage_profiles");
}

export async function setActiveSharedStorageProfile(
  profileId: string,
): Promise<void> {
  return invokeSafe<void>("set_active_shared_storage_profile", { profileId });
}

export async function disconnectSharedStorageProfile(
  profileId: string,
): Promise<void> {
  return invokeSafe<void>("disconnect_shared_storage_profile", { profileId });
}

export interface OAuthBeginResponse {
  sessionId: string;
  authorizationUrl: string;
  providerLabel: string;
}

export interface OAuthCompleteResponse {
  profile: SharedStorageProfile;
  accountEmail: string | null;
}

export async function beginOauthAuthorization(
  provider: string,
  displayName: string,
  clientId: string,
  clientSecret: string | null,
  providerFieldsJson?: string | null,
): Promise<OAuthBeginResponse> {
  return invokeSafe<OAuthBeginResponse>("begin_oauth_authorization", {
    provider,
    displayName,
    clientId,
    clientSecret,
    providerFieldsJson: providerFieldsJson ?? null,
  });
}

export async function completeOauthAuthorization(
  sessionId: string,
): Promise<OAuthCompleteResponse> {
  return invokeSafe<OAuthCompleteResponse>("complete_oauth_authorization", {
    sessionId,
  });
}

export async function triggerInstanceBackup(): Promise<BackupStatusResponse> {
  return invokeSafe<BackupStatusResponse>("trigger_instance_backup");
}

export async function triggerInstanceBackupFor(
  instanceId: number,
): Promise<BackupStatusResponse> {
  return invokeSafe<BackupStatusResponse>("trigger_instance_backup_for", {
    instanceId,
  });
}

export async function syncInstanceFromSharedStorage(
  instanceId: number,
): Promise<string> {
  return invokeSafe<string>("sync_instance_from_shared_storage", {
    instanceId,
  });
}

export async function listInstanceSharedStorageObjects(instanceId: number) {
  return invokeSafe<SharedStorageObjectEntry[]>(
    "list_instance_shared_storage_objects",
    { instanceId },
  );
}

export async function syncInstanceFromSharedStorageSelected(
  instanceId: number,
  selectedPaths: string[],
): Promise<string> {
  return invokeSafe<string>("sync_instance_from_shared_storage_selected", {
    instanceId,
    payload: { selectedPaths },
  });
}

export async function listInstanceExportableStorageObjects(instanceId: number) {
  return invokeSafe<SharedStorageObjectEntry[]>(
    "list_instance_exportable_storage_objects",
    { instanceId },
  );
}

export async function saveInstanceToSharedStorageSelected(
  instanceId: number,
  selectedPaths: string[],
): Promise<string> {
  return invokeSafe<string>("save_instance_to_shared_storage_selected", {
    instanceId,
    payload: { selectedPaths },
  });
}

export async function getInstanceBackupStatus(): Promise<SharedStorageInstanceStatus> {
  return invokeSafe<SharedStorageInstanceStatus>("get_instance_backup_status");
}

export async function setupInstanceBackupSchedule(): Promise<string> {
  return invokeSafe<string>("setup_instance_backup_schedule");
}

export async function removeInstanceBackupSchedule(): Promise<string> {
  return invokeSafe<string>("remove_instance_backup_schedule");
}

export async function getInstanceSunshineSettings(
  instanceId: number,
  sunshineUsername: string,
  sunshinePassword: string,
): Promise<SunshineSettingsResponse> {
  return invokeSafe<SunshineSettingsResponse>(
    "get_instance_sunshine_settings",
    {
      instanceId,
      sunshineUsername,
      sunshinePassword,
    },
  );
}

export async function updateInstanceSunshineSettings(
  instanceId: number,
  settings: Record<string, unknown>,
  sunshineUsername: string,
  sunshinePassword: string,
): Promise<void> {
  return invokeSafe<void>("update_instance_sunshine_settings", {
    instanceId,
    settings,
    sunshineUsername,
    sunshinePassword,
  });
}

export async function resetInstanceSunshineSettings(
  instanceId: number,
  sunshineUsername: string,
  sunshinePassword: string,
): Promise<void> {
  return invokeSafe<void>("reset_instance_sunshine_settings", {
    instanceId,
    sunshineUsername,
    sunshinePassword,
  });
}

export async function reconnectInstanceWireguard(
  instanceId: number,
): Promise<string> {
  return invokeSafe<string>("reconnect_instance_wireguard", { instanceId });
}

export async function rebootInstanceServices(
  instanceId: number,
): Promise<string> {
  return invokeSafe<string>("reboot_instance_services", { instanceId });
}

export async function pauseInstance(instanceId: number): Promise<void> {
  return invokeSafe<void>("pause_instance", { instanceId });
}

export async function destroyInstance(instanceId: number): Promise<void> {
  return invokeSafe<void>("destroy_instance", { instanceId });
}

export async function generateBundleIndex(): Promise<void> {
  return invokeSafe<void>("generate_bundle_index");
}

export async function getInstanceRestoreBundles(
  instanceId: number,
): Promise<BundleIndex> {
  return invokeSafe<BundleIndex>("get_instance_restore_bundles", {
    instanceId,
  });
}

export async function dryRunRestore(
  instanceId: number,
  payload: RestoreRequest,
): Promise<RestoreDryRunResult> {
  return invokeSafe<RestoreDryRunResult>("dry_run_restore", {
    instanceId,
    payload,
  });
}

export async function restoreBundle(
  instanceId: number,
  payload: RestoreRequest,
): Promise<RestoreJob> {
  return invokeSafe<RestoreJob>("restore_bundle", { instanceId, payload });
}

export async function getRestoreJob(jobId: string): Promise<RestoreJob> {
  return invokeSafe<RestoreJob>("get_restore_job", { jobId });
}

export async function getInstanceMicConfig(
  instanceId: number,
): Promise<InstanceMicConfig> {
  return invokeSafe<InstanceMicConfig>("get_instance_mic_config", {
    instanceId,
  });
}

export async function updateInstanceMicSettings(
  instanceId: number,
  payload: MicSettingsUpdate,
): Promise<InstanceMicConfig> {
  return invokeSafe<InstanceMicConfig>("update_instance_mic_settings", {
    instanceId,
    payload,
  });
}

export async function enableInstanceMic(
  instanceId: number,
  qualityProfile?: MicQualityProfile,
): Promise<MicSessionResponse> {
  return invokeSafe<MicSessionResponse>("enable_instance_mic", {
    instanceId,
    qualityProfile,
  });
}

export async function disableInstanceMic(instanceId: number): Promise<void> {
  return invokeSafe<void>("disable_instance_mic", { instanceId });
}

export async function reconnectInstanceMic(
  instanceId: number,
): Promise<MicSessionResponse> {
  return invokeSafe<MicSessionResponse>("reconnect_instance_mic", {
    instanceId,
  });
}

export async function recreateInstanceMicDevice(
  instanceId: number,
): Promise<void> {
  return invokeSafe<void>("recreate_instance_mic_device", { instanceId });
}

export async function getInstanceMicStatus(
  instanceId: number,
): Promise<InstanceMicRuntimeStatus> {
  return invokeSafe<InstanceMicRuntimeStatus>("get_instance_mic_status", {
    instanceId,
  });
}

let microphoneListCache:
  | { fetchedAt: number; devices: MicrophoneDevice[] }
  | null = null;
let microphoneListInFlight: Promise<MicrophoneDevice[]> | null = null;
const microphoneListTimeoutMs = 12_000;

function withTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  message: string,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = window.setTimeout(() => {
      reject(new Error(message));
    }, timeoutMs);

    promise.then(
      (value) => {
        window.clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        window.clearTimeout(timer);
        reject(error);
      },
    );
  });
}

export async function listMicrophones(
  options?: { forceRefresh?: boolean },
): Promise<MicrophoneDevice[]> {
  const forceRefresh = options?.forceRefresh ?? false;
  const now = Date.now();
  const cacheTtlMs = 30_000;

  if (
    !forceRefresh &&
    microphoneListCache &&
    now - microphoneListCache.fetchedAt < cacheTtlMs
  ) {
    return microphoneListCache.devices;
  }

  if (!forceRefresh && microphoneListInFlight) {
    return microphoneListInFlight;
  }

  const request = withTimeout(
    invokeSafe<MicrophoneDevice[]>("list_microphones"),
    microphoneListTimeoutMs,
    "Loading microphones timed out. Please try Refresh.",
  )
    .then((devices) => {
      microphoneListCache = { fetchedAt: Date.now(), devices };
      return devices;
    })
    .finally(() => {
      if (microphoneListInFlight === request) {
        microphoneListInFlight = null;
      }
    });

  microphoneListInFlight = request;
  return request;
}
