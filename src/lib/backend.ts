import { listen } from "@tauri-apps/api/event";
import { invokeSafe as tauriInvokeSafe, isRunningInTauri } from "./tauri";
import type {
  ManualLocationInput,
  MoonlightPreferences,
  OfferCandidate,
  OnboardingPayload,
  PlatformCredentialsUpdate,
  PersistedAppState,
  ProvisioningEvent,
  MoonlightConfigureResult,
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
  MicQualityProfile,
  MoonlightDetectionResult,
  PostWireGuardSetupState,
  ReachabilityResult,
  SetupStage,
  SunshineVerificationResult
} from "./types";

const apiBase = (import.meta.env.VITE_NOLAND_API_BASE as string | undefined)?.replace(/\/$/, "") ?? "http://127.0.0.1:8787";

async function httpInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const request = mapCommandToHttp(command, args ?? {});
  const response = await fetch(`${apiBase}${request.path}`, {
    method: request.method,
    headers: request.body === undefined ? undefined : { "Content-Type": "application/json" },
    body: request.body === undefined ? undefined : JSON.stringify(request.body)
  });
  const isJson = response.headers.get("content-type")?.includes("application/json");
  const data = isJson ? await response.json() : null;
  if (!response.ok) {
    const message = (data && (data.message as string | undefined)) ?? `${response.status} ${response.statusText}`;
    throw new Error(message);
  }
  return data as T;
}

function mapCommandToHttp(command: string, args: Record<string, unknown>): { method: string; path: string; body?: unknown } {
  switch (command) {
    case "get_app_state": return { method: "GET", path: "/api/v1/state" };
    case "complete_onboarding": return { method: "POST", path: "/api/v1/onboarding/complete", body: { payload: args.payload } };
    case "refresh_ip_location": return { method: "POST", path: "/api/v1/location/ip/refresh" };
    case "set_manual_location": return { method: "PUT", path: "/api/v1/location/manual", body: { payload: args.payload } };
    case "set_os_location": return { method: "PUT", path: "/api/v1/location/os", body: { payload: args.payload } };
    case "search_offers": return { method: "GET", path: `/api/v1/offers?page=${args.page ?? 1}&pageSize=${args.pageSize ?? 24}` };
    case "select_offer": return { method: "PUT", path: "/api/v1/offers/selected", body: { offerId: args.offerId, storageGb: args.storageGb } };
    case "start_play_flow": return { method: "POST", path: "/api/v1/orchestration/play/start" };
    case "start_play_existing_instance": return { method: "POST", path: "/api/v1/orchestration/play/start-existing", body: { instanceId: args.instanceId } };
    case "submit_pairing_pin": return { method: "POST", path: "/api/v1/orchestration/pairing/pin", body: { pin: args.pin } };
    case "skip_pairing_and_continue": return { method: "POST", path: "/api/v1/orchestration/pairing/skip" };
    case "setup_wireguard_client": return { method: "POST", path: "/api/v1/wireguard/local/setup" };
    case "reconnect_local_wireguard_client_quick": return { method: "POST", path: "/api/v1/wireguard/local/reconnect" };
    case "setup_wireguard_app_handoff_command": return { method: "POST", path: "/api/v1/wireguard/handoff/start" };
    case "verify_wireguard": return { method: "POST", path: "/api/v1/wireguard/verify" };
    case "open_wireguard_app_command": return { method: "POST", path: "/api/v1/wireguard/app/open" };
    case "download_wireguard_config_command": return { method: "GET", path: "/api/v1/wireguard/config/download" };
    case "get_setup_status_command": return { method: "GET", path: "/api/v1/wireguard/setup-status" };
    case "verify_sunshine": return { method: "POST", path: "/api/v1/sunshine/verify" };
    case "detect_moonlight": return { method: "GET", path: "/api/v1/moonlight/detect" };
    case "setup_moonlight_sunshine_command": return { method: "POST", path: "/api/v1/moonlight-sunshine/setup" };
    case "submit_moonlight_pin_to_sunshine_command": return { method: "POST", path: "/api/v1/moonlight-sunshine/pin", body: { pin: args.pin } };
    case "retry_setup_stage_command": return { method: "POST", path: "/api/v1/orchestration/retry-stage", body: { stage: args.stage } };
    case "get_provisioning_logs": return { method: "GET", path: "/api/v1/orchestration/logs" };
    case "get_moonlight_download_url": return { method: "GET", path: "/api/v1/moonlight/download-url" };
    case "get_wireguard_download_url": return { method: "GET", path: "/api/v1/wireguard/download-url" };
    case "launch_moonlight_client": return { method: "POST", path: "/api/v1/moonlight/launch" };
    case "configure_moonlight_client": return { method: "POST", path: "/api/v1/moonlight/configure", body: {
      apply: args.apply, forceClose: args.forceClose, native: args.native, network: args.network, preferCodec: args.preferCodec, maxBitrate: args.maxBitrate, fps: args.fps, resolution: args.resolution
    } };
    case "restore_moonlight_backup": return { method: "POST", path: "/api/v1/moonlight/restore-backup", body: { backupFile: args.backupFile } };
    case "get_rented_instances": return { method: "GET", path: "/api/v1/instances/rented" };
    case "update_vast_api_key": return { method: "PUT", path: "/api/v1/settings/vast-api-key", body: { apiKey: args.apiKey } };
    case "update_platform_credentials": return { method: "PUT", path: "/api/v1/settings/platform-credentials", body: { payload: args.payload } };
    case "update_server_preferences": return { method: "PUT", path: "/api/v1/settings/server-preferences", body: { payload: args.payload } };
    case "update_moonlight_preferences": return { method: "PUT", path: "/api/v1/settings/moonlight-preferences", body: { payload: args.payload } };
    case "regenerate_edid": return { method: "POST", path: "/api/v1/settings/edid/regenerate", body: { payload: args.payload } };
    case "update_ssh_credentials": return { method: "PUT", path: "/api/v1/settings/ssh-credentials", body: { payload: args.payload } };
    case "get_shared_storage_settings": return { method: "GET", path: "/api/v1/shared-storage/settings" };
    case "save_shared_storage_settings": return { method: "PUT", path: "/api/v1/shared-storage/settings", body: { payload: args.payload } };
    case "test_shared_storage_config": return { method: "POST", path: "/api/v1/shared-storage/settings/test" };
    case "trigger_instance_backup": return { method: "POST", path: "/api/v1/shared-storage/backup/trigger" };
    case "trigger_instance_backup_for": return { method: "POST", path: `/api/v1/shared-storage/backup/trigger/${args.instanceId}` };
    case "sync_instance_from_shared_storage": return { method: "POST", path: `/api/v1/shared-storage/sync/${args.instanceId}` };
    case "list_instance_shared_storage_objects": return { method: "GET", path: `/api/v1/shared-storage/objects/${args.instanceId}` };
    case "sync_instance_from_shared_storage_selected": return { method: "POST", path: `/api/v1/shared-storage/sync/${args.instanceId}/selected`, body: { payload: args.payload } };
    case "list_instance_exportable_storage_objects": return { method: "GET", path: `/api/v1/shared-storage/exportable-objects/${args.instanceId}` };
    case "save_instance_to_shared_storage_selected": return { method: "POST", path: `/api/v1/shared-storage/save/${args.instanceId}/selected`, body: { payload: args.payload } };
    case "get_instance_backup_status": return { method: "GET", path: "/api/v1/shared-storage/backup/status" };
    case "setup_instance_backup_schedule": return { method: "POST", path: "/api/v1/shared-storage/backup/schedule/setup" };
    case "remove_instance_backup_schedule": return { method: "POST", path: "/api/v1/shared-storage/backup/schedule/remove" };
    case "get_instance_sunshine_settings": return { method: "POST", path: `/api/v1/instances/${args.instanceId}/sunshine-settings/get`, body: { sunshineUsername: args.sunshineUsername, sunshinePassword: args.sunshinePassword } };
    case "update_instance_sunshine_settings": return { method: "PUT", path: `/api/v1/instances/${args.instanceId}/sunshine-settings`, body: { settings: args.settings, sunshineUsername: args.sunshineUsername, sunshinePassword: args.sunshinePassword } };
    case "reset_instance_sunshine_settings": return { method: "POST", path: `/api/v1/instances/${args.instanceId}/sunshine-settings/reset`, body: { sunshineUsername: args.sunshineUsername, sunshinePassword: args.sunshinePassword } };
    case "reconnect_instance_wireguard": return { method: "POST", path: `/api/v1/instances/${args.instanceId}/wireguard/reconnect` };
    case "reboot_instance_services": return { method: "POST", path: `/api/v1/instances/${args.instanceId}/services/reboot` };
    case "pause_instance": return { method: "POST", path: `/api/v1/instances/${args.instanceId}/pause` };
    case "destroy_instance": return { method: "DELETE", path: `/api/v1/instances/${args.instanceId}` };
    case "generate_bundle_index": return { method: "POST", path: "/api/v1/restore/bundles/index/generate" };
    case "get_instance_restore_bundles": return { method: "GET", path: `/api/v1/restore/bundles/${args.instanceId}` };
    case "dry_run_restore": return { method: "POST", path: `/api/v1/restore/${args.instanceId}/dry-run`, body: { payload: args.payload } };
    case "restore_bundle": return { method: "POST", path: `/api/v1/restore/${args.instanceId}/run`, body: { payload: args.payload } };
    case "get_restore_job": return { method: "GET", path: `/api/v1/restore/jobs/${args.jobId}` };
    default:
      throw new Error(`No HTTP mapping for command: ${command}`);
  }
}

async function invokeSafe<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isRunningInTauri()) {
    return tauriInvokeSafe<T>(command, args);
  }
  return httpInvoke<T>(command, args);
}

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

export async function setupWireguardAppHandoff(): Promise<PostWireGuardSetupState> {
  return invokeSafe<PostWireGuardSetupState>("setup_wireguard_app_handoff_command");
}

export async function verifyWireguard(): Promise<ReachabilityResult> {
  return invokeSafe<ReachabilityResult>("verify_wireguard");
}

export async function openWireguardApp(): Promise<void> {
  await invokeSafe<void>("open_wireguard_app_command");
}

export async function downloadWireguardConfig(): Promise<string> {
  return invokeSafe<string>("download_wireguard_config_command");
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
  return invokeSafe<PostWireGuardSetupState>("setup_moonlight_sunshine_command");
}

export async function submitMoonlightPinToSunshine(pin: string): Promise<PostWireGuardSetupState> {
  return invokeSafe<PostWireGuardSetupState>("submit_moonlight_pin_to_sunshine_command", { pin });
}

export async function retrySetupStage(stage: SetupStage): Promise<PostWireGuardSetupState> {
  return invokeSafe<PostWireGuardSetupState>("retry_setup_stage_command", { stage });
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

export async function resolveMoonlightDownloadUrl(): Promise<string> {
  return invokeSafe<string>("get_moonlight_download_url");
}

export async function resolveWireguardDownloadUrl(): Promise<string> {
  return invokeSafe<string>("get_wireguard_download_url");
}

export async function launchMoonlightClient(): Promise<void> {
  await invokeSafe<void>("launch_moonlight_client");
}

export async function configureMoonlightClient(options?: {
  apply?: boolean;
  forceClose?: boolean;
  native?: boolean;
  network?: "lan" | "wifi" | "remote" | "auto";
  preferCodec?: "auto" | "h264" | "hevc" | "av1";
  maxBitrate?: number;
  fps?: number;
  resolution?: string;
}): Promise<MoonlightConfigureResult> {
  return invokeSafe<MoonlightConfigureResult>("configure_moonlight_client", {
    apply: options?.apply ?? false,
    forceClose: options?.forceClose ?? false,
    native: options?.native ?? false,
    network: options?.network ?? "auto",
    preferCodec: options?.preferCodec ?? "auto",
    maxBitrate: options?.maxBitrate ?? null,
    fps: options?.fps ?? null,
    resolution: options?.resolution ?? null,
  });
}

export async function restoreMoonlightBackup(backupFile: string): Promise<string> {
  return invokeSafe<string>("restore_moonlight_backup", { backupFile });
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

export async function regenerateEdid(payload: {
  mode: "auto_detect" | "manual";
  refreshRateHz: number;
}): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("regenerate_edid", { payload });
}

export async function updateSshCredentials(
  payload: SshCredentialsUpdate
): Promise<PersistedAppState> {
  return invokeSafe<PersistedAppState>("update_ssh_credentials", { payload });
}

export async function subscribeProvisioningEvents(
  callback: (event: ProvisioningEvent) => void
): Promise<() => void> {
  if (!isRunningInTauri()) {
    let active = true;
    let seen = new Set<string>();
    const poll = async () => {
      while (active) {
        try {
          const logs = await getProvisioningLogs();
          for (const event of logs.slice().reverse()) {
            const key = `${event.timestamp}-${event.message}-${event.state}`;
            if (seen.has(key)) {
              continue;
            }
            seen.add(key);
            callback(event);
          }
          if (seen.size > 2000) {
            seen = new Set(Array.from(seen).slice(-1000));
          }
        } catch {
          // best-effort polling
        }
        await new Promise((resolve) => setTimeout(resolve, 1200));
      }
    };
    void poll();
    return () => {
      active = false;
    };
  }

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
  instanceId: number,
  sunshineUsername: string,
  sunshinePassword: string
): Promise<SunshineSettingsResponse> {
  return invokeSafe<SunshineSettingsResponse>("get_instance_sunshine_settings", {
    instanceId,
    sunshineUsername,
    sunshinePassword
  });
}

export async function updateInstanceSunshineSettings(
  instanceId: number,
  settings: Record<string, unknown>,
  sunshineUsername: string,
  sunshinePassword: string
): Promise<void> {
  return invokeSafe<void>("update_instance_sunshine_settings", {
    instanceId,
    settings,
    sunshineUsername,
    sunshinePassword
  });
}

export async function resetInstanceSunshineSettings(
  instanceId: number,
  sunshineUsername: string,
  sunshinePassword: string
): Promise<void> {
  return invokeSafe<void>("reset_instance_sunshine_settings", {
    instanceId,
    sunshineUsername,
    sunshinePassword
  });
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
