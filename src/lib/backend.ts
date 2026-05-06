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
  MicQualityProfile
} from "./types";

export async function getAppState(): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("get_app_state");
}

export async function completeOnboarding(payload: OnboardingPayload): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("complete_onboarding", { payload });
}

export async function refreshIpLocation(): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("refresh_ip_location");
}

export async function setManualLocation(payload: ManualLocationInput): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("set_manual_location", { payload });
}

export async function setOsLocation(payload: ManualLocationInput): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("set_os_location", { payload });
}

export async function searchOffers(page = 1, pageSize = 24): Promise<OfferCandidate[]> {
  return invokeSafe<OfferCandidate[]>("search_offers", { page, pageSize });
}

export async function selectOffer(offerId: number, storageGb: number): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("select_offer", { offerId, storageGb });
}

export async function startPlayFlow(): Promise<void> {
  await invokeSafe<void>("start_play_flow");
}

export async function startPlayExistingInstance(instanceId: number): Promise<void> {
  await invokeSafe<void>("start_play_existing_instance", { instanceId });
}

export async function submitPairingPin(pin: string): Promise<PersistedAppState> {
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

export async function getProvisioningLogs(): Promise<ProvisioningEvent[]> {
  return invokeSafe<ProvisioningEvent[]>("get_provisioning_logs");
}

export async function resolveMoonlightDownloadUrl(): Promise<string> {
  return invokeSafe<string>("get_moonlight_download_url");
}

export async function getRentedInstances(): Promise<RentedInstanceSummary[]> {
  return invokeSafe<RentedInstanceSummary[]>("get_rented_instances");
}

export async function updateVastApiKey(apiKey: string): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_vast_api_key", { apiKey });
}

export async function updatePlatformCredentials(
  payload: PlatformCredentialsUpdate
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_platform_credentials", { payload });
}

export async function updateServerPreferences(
  payload: ServerPreferencesUpdate
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_server_preferences", { payload });
}

export async function updateMoonlightPreferences(
  payload: MoonlightPreferences
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_moonlight_preferences", { payload });
}

export async function updateSshCredentials(
  payload: SshCredentialsUpdate
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_ssh_credentials", { payload });
}

export async function subscribeProvisioningEvents(
  callback: (event: ProvisioningEvent) => void
): Promise<() => void> {
  const unlisten = await listen<ProvisioningEvent>("orchestration:progress", ({ payload }) => {
    callback(payload);
  });

  return () => {
    unlisten();
  };
}

export async function getSharedStorageSettings(): Promise<SharedStorageSettingsResponse> {
  return invokeSafe<SharedStorageSettingsResponse>("get_shared_storage_settings");
}

export async function saveSharedStorageSettings(
  payload: SharedStorageSettingsUpdate
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("save_shared_storage_settings", { payload });
}

export async function testSharedStorageConfig(): Promise<string> {
  return invokeSafe<string>("test_shared_storage_config");
}

export async function triggerInstanceBackup(): Promise<BackupStatusResponse> {
  return invokeSafe<BackupStatusResponse>("trigger_instance_backup");
}

export async function triggerInstanceBackupFor(instanceId: number): Promise<BackupStatusResponse> {
  return invokeSafe<BackupStatusResponse>("trigger_instance_backup_for", { instanceId });
}

export async function syncInstanceFromSharedStorage(instanceId: number): Promise<string> {
  return invokeSafe<string>("sync_instance_from_shared_storage", { instanceId });
}

export async function listInstanceSharedStorageObjects(instanceId: number) {
  return invokeSafe<SharedStorageObjectEntry[]>(
    "list_instance_shared_storage_objects",
    { instanceId }
  );
}

export async function syncInstanceFromSharedStorageSelected(
  instanceId: number,
  selectedPaths: string[]
): Promise<string> {
  return invokeSafe<string>("sync_instance_from_shared_storage_selected", {
    instanceId,
    payload: { selectedPaths }
  });
}

export async function listInstanceExportableStorageObjects(instanceId: number) {
  return invokeSafe<SharedStorageObjectEntry[]>(
    "list_instance_exportable_storage_objects",
    { instanceId }
  );
}

export async function saveInstanceToSharedStorageSelected(
  instanceId: number,
  selectedPaths: string[]
): Promise<string> {
  return invokeSafe<string>("save_instance_to_shared_storage_selected", {
    instanceId,
    payload: { selectedPaths }
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
  instanceId: number
): Promise<SunshineSettingsResponse> {
  return invokeSafe<SunshineSettingsResponse>("get_instance_sunshine_settings", { instanceId });
}

export async function updateInstanceSunshineSettings(
  instanceId: number,
  settings: Record<string, unknown>
): Promise<void> {
  return invokeSafe<void>("update_instance_sunshine_settings", { instanceId, settings });
}

export async function reconnectInstanceWireguard(instanceId: number): Promise<string> {
  return invokeSafe<string>("reconnect_instance_wireguard", { instanceId });
}

export async function rebootInstanceServices(instanceId: number): Promise<string> {
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

export async function getInstanceRestoreBundles(instanceId: number): Promise<BundleIndex> {
  return invokeSafe<BundleIndex>("get_instance_restore_bundles", { instanceId });
}

export async function dryRunRestore(
  instanceId: number,
  payload: RestoreRequest
): Promise<RestoreDryRunResult> {
  return invokeSafe<RestoreDryRunResult>("dry_run_restore", { instanceId, payload });
}

export async function restoreBundle(
  instanceId: number,
  payload: RestoreRequest
): Promise<RestoreJob> {
  return invokeSafe<RestoreJob>("restore_bundle", { instanceId, payload });
}

export async function getRestoreJob(jobId: string): Promise<RestoreJob> {
  return invokeSafe<RestoreJob>("get_restore_job", { jobId });
}

export async function getInstanceMicConfig(instanceId: number): Promise<InstanceMicConfig> {
  return invokeSafe<InstanceMicConfig>("get_instance_mic_config", { instanceId });
}

export async function updateInstanceMicSettings(
  instanceId: number,
  payload: MicSettingsUpdate
): Promise<InstanceMicConfig> {
  return invokeSafe<InstanceMicConfig>("update_instance_mic_settings", { instanceId, payload });
}

export async function enableInstanceMic(
  instanceId: number,
  qualityProfile?: MicQualityProfile
): Promise<MicSessionResponse> {
  return invokeSafe<MicSessionResponse>("enable_instance_mic", { instanceId, qualityProfile });
}

export async function disableInstanceMic(instanceId: number): Promise<void> {
  return invokeSafe<void>("disable_instance_mic", { instanceId });
}

export async function reconnectInstanceMic(instanceId: number): Promise<MicSessionResponse> {
  return invokeSafe<MicSessionResponse>("reconnect_instance_mic", { instanceId });
}

export async function recreateInstanceMicDevice(instanceId: number): Promise<void> {
  return invokeSafe<void>("recreate_instance_mic_device", { instanceId });
}

export async function getInstanceMicStatus(instanceId: number): Promise<InstanceMicRuntimeStatus> {
  return invokeSafe<InstanceMicRuntimeStatus>("get_instance_mic_status", { instanceId });
}
