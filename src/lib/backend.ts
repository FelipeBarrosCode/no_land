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
  SshCredentialsUpdate
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
