export const DEFAULT_TEMPLATE_HASH = "2a62a7d5089a50a5ad89a9480f540d25";

export const VAST_API_KEY_URL = "https://cloud.vast.ai/manage-keys/";
export const MOONLIGHT_DOWNLOAD_URL = "https://github.com/moonlight-stream/moonlight-qt/releases";
export const WIREGUARD_DOWNLOAD_URL = "https://www.wireguard.com/install/";

export const MOONLIGHT_DOWNLOADS: Record<string, string> = {
  windows: MOONLIGHT_DOWNLOAD_URL,
  macos: MOONLIGHT_DOWNLOAD_URL,
  linux: MOONLIGHT_DOWNLOAD_URL,
  unknown: MOONLIGHT_DOWNLOAD_URL
};

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
  "Ready"
] as const;
