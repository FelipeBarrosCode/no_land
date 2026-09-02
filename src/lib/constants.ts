export const DEFAULT_TEMPLATE_HASH = "2a62a7d5089a50a5ad89a9480f540d25";

export const VAST_REFERRAL_ID = "320920";
export const VAST_HOME_URL = `https://cloud.vast.ai/?ref_id=${VAST_REFERRAL_ID}`;
export const VAST_BILLING_URL = `https://cloud.vast.ai/billing/?ref_id=${VAST_REFERRAL_ID}`;
export const VAST_API_KEY_URL = `https://cloud.vast.ai/manage-keys/?tab=api-keys&ref_id=${VAST_REFERRAL_ID}`;
export const VAST_LOGIN_URL = VAST_HOME_URL;

export const PROVISIONING_ORDER = [
  "GeneratingSshKey",
  "UploadingSshKeyToVast",
  "CreatingInstance",
  "WaitingForInstance",
  "VerifyingReservation",
  "ConnectingSsh",
  "ConfiguringSunshine",
  "ConfiguringWireGuard",
  "ConfiguringNvidiaHeadless",
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
] as const;
