export type LocationSource = "os" | "ip" | "manual";

export type OrchestrationState =
  | "Idle"
  | "Onboarding"
  | "SelectingServer"
  | "ServerSelected"
  | "GeneratingSshKey"
  | "UploadingSshKeyToVast"
  | "CreatingInstance"
  | "WaitingForInstance"
  | "VerifyingReservation"
  | "ConnectingSsh"
  | "ConfiguringRemote"
  | "ConfiguringWireGuard"
  | "ConfiguringSunshine"
  | "ConfiguringNvidiaHeadless"
  | "WireGuardConfigGenerated"
  | "WireGuardAppHandoffStarted"
  | "WireGuardWaitingForImport"
  | "WireGuardWaitingForActivation"
  | "WireGuardVerifying"
  | "WireGuardConnected"
  | "MoonlightSunshineReadyToSetup"
  | "SunshineCredentialsConfiguring"
  | "SunshineVerifying"
  | "MoonlightDetecting"
  | "MoonlightPairingStarted"
  | "MoonlightPinReceived"
  | "SunshinePinSubmitting"
  | "MoonlightSunshinePaired"
  | "ConfiguringMoonlight"
  | "AwaitingPairPin"
  | "Pairing"
  | "Ready"
  | "Error";

export type SetupStage =
  | "pre_wireguard_existing_flow"
  | "wireguard_config_generated"
  | "wireguard_app_handoff_started"
  | "wireguard_waiting_for_import"
  | "wireguard_waiting_for_activation"
  | "wireguard_verifying"
  | "wireguard_connected"
  | "moonlight_sunshine_ready_to_setup"
  | "sunshine_credentials_configuring"
  | "sunshine_verifying"
  | "moonlight_detecting"
  | "moonlight_pairing_started"
  | "moonlight_pin_received"
  | "sunshine_pin_submitting"
  | "moonlight_sunshine_paired"
  | "setup_complete"
  | "failed";

export type WireGuardSetupMode = "embedded_gotatun";

export type WireGuardSetupStatus =
  | "not_started"
  | "config_generated"
  | "app_handoff_started"
  | "waiting_for_user_import"
  | "waiting_for_user_activation"
  | "verifying"
  | "connected"
  | "failed";

export interface SetupErrorState {
  code: string;
  message: string;
  stage: SetupStage;
  retryable: boolean;
  details: string | null;
}

export interface PostWireGuardSetupState {
  stage: SetupStage;
  wireguardSetupMode: WireGuardSetupMode;
  wireguardSetupStatus: WireGuardSetupStatus;
  currentInstanceId: number | null;
  wireguardExportPath: string;
  wireguardConfig: string;
  wireguardVerifiedHost: string;
  wireguardReachablePorts: number[];
  sunshineUsername: string;
  moonlightHost: string;
  moonlightInstalled: boolean;
  paired: boolean;
  setupComplete: boolean;
  lastError: SetupErrorState | null;
}

export type ConnectionProvider = "wireguard";

export interface CredentialsState {
  appUsername: string;
  appPassword: string;
  vastApiKey: string;
  twitchClientId: string;
  twitchClientSecret: string;
}

export interface SshState {
  keyName: string;
  privateKeyPath: string;
  publicKeyPath: string;
  uploadedToVast: boolean;
  sshUsername: string;
  sshPassword: string;
}

export interface LocationState {
  source: LocationSource;
  city: string;
  region: string;
  country: string;
  latitude: number;
  longitude: number;
}

export interface ServerPreferences {
  minReliability: number;
  storageGb: number;
  templateHash: string;
  maxHourlyPrice: number;
  minHourlyPrice: number;
  requireVerified: boolean;
  requireDatacenter: boolean;
  includeOnDemand: boolean;
  includeInterruptible: boolean;
  includeReserved: boolean;
  requireStaticIp: boolean;
  requireAvx: boolean;
  minGpuCount: number;
  minGpuRamGb: number;
  minCpuCores: number;
  minInetDownMbps: number;
  minInetUpMbps: number;
  geolocationCountryCode: string;
}

export interface OfferCountryAvailability {
  code: string;
  offerCount: number;
}

export interface OfferCandidate {
  id: number;
  hostId: number | null;
  hostLabel: string;
  locationLabel: string;
  city: string;
  region: string;
  country: string;
  latitude: number;
  longitude: number;
  reliability: number;
  gpuName: string;
  gpuRamMb: number;
  gpuCount: number;
  cpuName: string;
  cpuCores: number;
  internetDownMbps: number;
  internetUpMbps: number;
  hourlyPrice: number;
  computeHourlyPrice: number;
  storageHourlyPrice: number;
  availableStorageGb: number;
  estimatedDistanceKm: number;
  score: number;
  timeRemainingHours: number;
  isVerified: boolean;
  isDatacenter: boolean;
  offerType: string;
  hasStaticIp: boolean;
  hasAvx: boolean;
}

export interface InstanceState {
  instanceId: number | null;
  offerId: number | null;
  status: string;
  sshHost: string;
  sshPort: number;
  sshUser: string;
  sshCommand: string;
  hourlyPrice: number;
  computeHourlyPrice: number;
  storageHourlyPrice: number;
}

export interface WireGuardState {
  serverIp: string;
  clientIp: string;
  serverPublicKey: string;
  clientPublicKey: string;
  configPath: string;
}

export interface SunshineState {
  configured: boolean;
  headlessEdidBase64: string;
  edidMode: "auto_detect" | "mac_hardware" | "manual";
  edidRefreshRateHz: number;
  edidSourceLabel: string;
}

export interface MoonlightState {
  configured: boolean;
  hostAddress: string;
  sessionState: string;
  lastError: string | null;
}

export interface ProvisionedServerSteps {
  sshKeyReady: boolean;
  sshKeyUploadedToVast: boolean;
  instanceCreated: boolean;
  instanceReady: boolean;
  sshConnected: boolean;
  nvidiaHeadlessConfigured: boolean;
  sunshineConfigured: boolean;
  lowLatencyAudioConfigured: boolean;
  micReceiverInstalled: boolean;
  wireguardConfigured: boolean;
  moonlightConfigured: boolean;
  awaitingPairPin: boolean;
  pairingCompleted: boolean;
}

export interface DisplayModeSpec {
  width: number;
  height: number;
  refreshMillihz: number;
}

export interface DisplayProfile {
  preferredMode: DisplayModeSpec;
  advertisedModes: DisplayModeSpec[];
  sourceLabel: string;
}

export interface RemoteDisplayState {
  desiredProfileHash: string;
  installedProfileHash: string;
  advertisedModes: DisplayModeSpec[];
  selectedMode: DisplayModeSpec | null;
  activeMode: DisplayModeSpec | null;
  outputName: string | null;
  appliedAt: string | null;
  lastApplyError: string | null;
}

export interface InstanceDisplayStatus {
  desiredProfile: DisplayProfile;
  desiredProfileHash: string;
  installedProfileHash: string;
  outputName: string | null;
  activeMode: DisplayModeSpec | null;
  selectedMode: DisplayModeSpec | null;
  xorgActive: boolean;
  sunshineActive: boolean;
  profileUpdateRequired: boolean;
}

export interface ApplyDisplayModeResult {
  status: InstanceDisplayStatus;
  xorgRestarted: boolean;
}

export interface ProvisionedServerState {
  instanceId: number;
  offerId: number | null;
  sshHost: string;
  sshPort: number;
  status: string;
  sshCommand: string;
  hourlyPrice: number;
  computeHourlyPrice: number;
  storageHourlyPrice: number;
  wireguardServerIp: string;
  wireguardClientIp: string;
  wireguardServerPublicKey: string;
  wireguardClientPublicKey: string;
  wireguardConfigPath: string;
  moonlightHostAddress: string;
  connectionProvider: ConnectionProvider;
  embeddedMoonlightPipelineEnabled: boolean;
  embeddedMoonlightHostId: string;
  embeddedMoonlightPaired: boolean;
  micDeviceId: string;
  micDeviceName: string;
  micQualityProfile: MicQualityProfile;
  micForwardingEnabled: boolean;
  micAutoConnect: boolean;
  display: RemoteDisplayState;
  lastState: OrchestrationState;
  lastError: string | null;
  steps: ProvisionedServerSteps;
}

export interface MoonlightPreferences {
  bitrate: number;
  fps: number;
  refreshRateMode: string;
  width: number;
  height: number;
  displayOutput: string | null;
  aspectRatio: string | null;
  hostaudio: number;
  showperfoverlay: number;
  keepawake: number;
  framepacing: number;
  vsync: number;
  hdr: number;
  videocfg: number;
  videodec: number;
  yuv444: number;
  gameopts: number;
  gamepadmouse: number;
  detectnetblocking: number;
  showInputDebugHud: number;
}

export type MoonlightPacingMode =
  | "off"
  | "automatic"
  | "software"
  | "hardwareMultiple";

export type MoonlightFrameBufferMode =
  | "off"
  | "oneFrame"
  | "twoFrames"
  | "threeFrames";

export type MoonlightRemoteStreamMode =
  | "auto"
  | "forceRemote"
  | "forceLocal";

export interface NolandLatencyConfig {
  telemetryEnabled: boolean;
  adaptiveLateFrameDropEnabled: boolean;
  adaptivePacketSizeEnabled: boolean;
  decoderBackpressurePolicyEnabled: boolean;
  pacingMode: MoonlightPacingMode;
  frameBufferMode: MoonlightFrameBufferMode;
  autoReconnectOnUnexpectedTermination: boolean;
  remoteStreamMode: MoonlightRemoteStreamMode;
  remotePacketSize: number;
  lateFrameToleranceUs: number;
  vsyncEnabled: boolean;
}

export interface MoonlightHostLatencyPreferencesResponse {
  hostId: string;
  effective: {
    latency: NolandLatencyConfig;
  };
  overrides: {
    latency?: Partial<NolandLatencyConfig> | null;
  } | null;
}

export interface RentedInstanceSummary {
  instanceId: number;
  label: string;
  status: string;
  gpuName: string;
  sshHost: string;
  sshPort: number;
  publicIp: string;
  embeddedMoonlightPipelineEnabled: boolean;
  embeddedMoonlightSessionState?: string | null;
  embeddedMoonlightLastError?: string | null;
  embeddedMoonlightLastRuntimeEvent?: string | null;
  embeddedMoonlightRuntimeConnected?: boolean | null;
  embeddedMoonlightRendererReady?: boolean | null;
  embeddedMoonlightVideoSessionActive?: boolean | null;
  embeddedMoonlightVideoFrameCount?: number | null;
  embeddedMoonlightRendererSubmittedFrameCount?: number | null;
  embeddedMoonlightRendererDroppedFrameCount?: number | null;
  embeddedMoonlightAudioSampleCount?: number | null;
  embeddedMoonlightPaired?: boolean | null;
}

export interface LaunchLibraryItem {
  appId: string;
  displayName: string;
  aliases: string[];
  installed: boolean;
  inSharedStorage: boolean;
  latestBundleId: string | null;
  sourceLabels: string[];
  launchable: boolean;
  launchMethod: string;
  restoreRequired: boolean;
  artworkKey: string;
}

export interface LaunchLibraryResponse {
  instanceId: number;
  launchPcAvailable: boolean;
  items: LaunchLibraryItem[];
}

export interface LaunchSoftwareJob {
  jobId: string;
  instanceId: number;
  appId: string;
  status: string;
  restorePerformed: boolean;
  streamStarted: boolean;
  message: string;
  error: string | null;
  startedAt: string;
  finishedAt: string | null;
}

export interface SoftwareArtworkResult {
  key: string;
  imageUrl: string | null;
  source: string;
}

export interface ServerPreferencesUpdate {
  minReliability: number;
  storageGb: number;
  templateHash: string;
  maxHourlyPrice: number;
  minHourlyPrice: number;
  requireVerified: boolean;
  requireDatacenter: boolean;
  includeOnDemand: boolean;
  includeInterruptible: boolean;
  includeReserved: boolean;
  requireStaticIp: boolean;
  requireAvx: boolean;
  minGpuCount: number;
  minGpuRamGb: number;
  minCpuCores: number;
  minInetDownMbps: number;
  minInetUpMbps: number;
  geolocationCountryCode: string;
}

export interface SshCredentialsUpdate {
  sshUsername: string;
  sshPassword: string;
}

export interface PlatformCredentialsUpdate {
  appUsername: string;
  appPassword: string;
}

export interface IgdbCredentialsUpdate {
  twitchClientId: string;
  twitchClientSecret: string;
}

export type VastBrowserBillingAction =
  | "snapshot"
  | "open-add-credit"
  | "open-auto-topup";


export interface VastWalletSummary {
  available: boolean;
  balanceUsd: number | null;
  displayAmount: string;
  source: "vast_api" | "browser_artifact" | "unavailable";
  lastUpdatedAt: string | null;
}

export interface PersistedAppState {
  version: number;
  onboardingCompleted: boolean;
  hasCompletedGuidedSetup: boolean;
  credentials: CredentialsState;
  ssh: SshState;
  location: LocationState;
  serverPreferences: ServerPreferences;
  selectedOffer: OfferCandidate | null;
  instance: InstanceState;
  wireguard: WireGuardState;
  sunshine: SunshineState;
  moonlight: MoonlightState;
  moonlightPreferences: MoonlightPreferences;
  sharedStorage: SharedStorageState;
  sharedStorageProfiles?: ProfileReference[];
  provisionedServers: ProvisionedServerState[];
  postWireguardSetup: PostWireGuardSetupState;
  orchestrationState: OrchestrationState;
  connectionProvider: ConnectionProvider;
  lastError: string | null;
}

export interface SharedStorageState {
  settings: SharedStorageSettings;
  lastBackupStartedAt: string | null;
  lastBackupFinishedAt: string | null;
  lastBackupStatus: string;
  lastBackupError: string | null;
  lastBackupTrigger: string;
}

export interface OnboardingPayload {
  appUsername: string;
  appPassword: string;
  vastApiKey: string;
}

export interface ManualLocationInput {
  city: string;
  region: string;
  country: string;
  latitude: number;
  longitude: number;
}

export interface SearchOffersRequest {
  limit: number;
}

export interface ProvisioningEvent {
  state: OrchestrationState;
  message: string;
  details?: string;
  timestamp: string;
  isError: boolean;
}

export interface FrontendError {
  code: string;
  message: string;
  details?: string;
  retryable: boolean;
}

export type HealthProbeStatus = "ok" | "warning" | "failed";

export interface HealthProbe {
  id: string;
  label: string;
  category: string;
  status: HealthProbeStatus;
  summary: string;
  details: string | null;
  fixHint: string | null;
}

export interface SystemHealthReport {
  ok: boolean;
  checkedAtUnix: number;
  os: string;
  arch: string;
  summary: string;
  probes: HealthProbe[];
}

export interface DiagnosticReportResponse {
  path: string;
  summary: string;
  reportMarkdown: string;
}

export interface MoonlightCodecSupport {
  h264: boolean;
  hevc: boolean;
  av1: boolean;
}

export interface MoonlightAppliedSetting {
  key: string;
  value: string;
}

export interface MoonlightConfigureResult {
  installed: boolean;
  success: boolean;
  platform: string;
  settingsLocation: string | null;
  backupPath: string | null;
  displayResolution: string;
  refreshRateHz: number;
  networkType: string;
  codecSupport: MoonlightCodecSupport;
  selectedSettings: MoonlightAppliedSetting[];
  staticDefaults: MoonlightAppliedSetting[];
  preservedSettings: string[];
  warnings: string[];
  error: string | null;
  canLaunchAnyway: boolean;
}

export interface ReachabilityResult {
  reachable: boolean;
  host: string;
  checkedPorts: number[];
  reachablePorts: number[];
  error?: string;
}


export interface MoonlightPairingSessionResponse {
  sessionId: string;
  hostId: string;
  pin: string;
  expiresInSeconds: number;
}

export interface EmbeddedMoonlightInstanceStatus {
  instanceId: number;
  enabled: boolean;
  hostId: string;
  paired: boolean;
  hostAddress: string;
  sessionState: string;
  lastError: string | null;
  runtimeConnected: boolean;
  rendererReady: boolean;
  videoSessionActive: boolean;
  videoFrameCount: number;
  rendererSubmittedFrameCount: number;
  rendererDroppedFrameCount: number;
  audioSampleCount: number;
  lastRuntimeEvent: string | null;
}

export interface SunshineVerificationResult {
  reachable: boolean;
  authenticated: boolean;
  host: string;
  port: number;
  error?: string;
}

export interface SharedStorageSettings {
  enabled: boolean;
  backblazeKeyId: string;
  bucketName: string;
  remoteName: string;
  destinationPrefix: string;
}

export interface SharedStorageSettingsResponse {
  enabled: boolean;
  backblazeKeyId: string;
  bucketName: string;
  remoteName: string;
  destinationPrefix: string;
  cryptPasswordSet: boolean;
}

export interface SharedStorageSettingsUpdate {
  enabled: boolean;
  backblazeKeyId: string;
  backblazeApplicationKey: string;
  bucketName: string;
  remoteName: string;
  destinationPrefix: string;
  cryptPassword?: string;
}

export type StorageProvider =
  | "amazon_s3"
  | "backblaze_b2"
  | "cloudflare_r2"
  | "wasabi"
  | "digital_ocean_spaces"
  | "generic_s3"
  | "google_drive"
  | "google_cloud_storage"
  | "microsoft_one_drive"
  | "dropbox"
  | "box"
  | "azure_blob"
  | "sftp"
  | "webdav";

export interface SharedStorageProfile {
  id: string;
  displayName: string;
  provider: StorageProvider;
  providerLabel: string;
  bucket: string | null;
  prefix: string | null;
  credentialVaultReference: string;
  repositoryId: string;
  status: string;
  lastVerifiedAt: number | null;
  protectedBundlesCount: number;
  totalStoredBytes: number;
}

export interface SharedStorageTestResult {
  authenticated: boolean;
  canList: boolean;
  canWrite: boolean;
  canRead: boolean;
  canDeleteTestObject: boolean;
  repositoryAccessible: boolean;
  latencyMs: number | null;
  error: string | null;
}

export interface ProviderSelectOption {
  value: string;
  label: string;
}

export type ProviderFieldType =
  | "text"
  | "password"
  | "number"
  | "toggle"
  | { options: ProviderSelectOption[] };

export interface ProviderField {
  key: string;
  label: string;
  fieldType: ProviderFieldType;
  required: boolean;
  placeholder: string | null;
  helpText: string | null;
}

export interface ProviderDefinition {
  provider: StorageProvider;
  label: string;
  category: string;
  isOauth: boolean;
  description: string;
  fields: ProviderField[];
}

export interface ProfileReference {
  id: string;
  displayName: string;
  providerLabel: string;
  provider?: StorageProvider | null;
  bucket?: string | null;
  prefix?: string | null;
  active?: boolean;
}

export type BackupPerformanceMode = "fast" | "balanced" | "full";

export interface BackupStatusResponse {
  lastBackupStartedAt: string | null;
  lastBackupFinishedAt: string | null;
  lastBackupStatus: string;
  lastBackupError: string | null;
  lastBackupTrigger?: string;
}

export interface SharedStorageInstanceStatus {
  instanceId: number;
  backupRunning: boolean;
  lastBackupStartedAt: string | null;
  lastBackupFinishedAt: string | null;
  lastBackupStatus: string;
  lastBackupError: string | null;
}

export interface SharedStorageObjectEntry {
  path: string;
  name: string;
  parentPath: string;
  isDir: boolean;
}

export interface SharedStorageSyncSelectionRequest {
  selectedPaths: string[];
}

export interface SharedStorageProgressEvent {
  operationId: string;
  instanceId: number;
  kind: string;
  state: string;
  phase: string | null;
  message: string | null;
  completedUnits: number | null;
  totalUnits: number | null;
  unit: string | null;
  fraction: number | null;
  readyToLaunch: boolean;
  cancelRequested: boolean;
  cancellable: boolean;
}

export interface SunshineSetting {
  key: string;
  value: unknown;
  label: string;
  description?: string;
  category: string;
  valueType: string;
  requiresRestart: boolean;
}

export interface SunshineSettingsResponse {
  settings: SunshineSetting[];
  raw: Record<string, unknown>;
}


// ============================================================
// Microphone Passthrough types
// ============================================================

export type MicQualityProfile = "standard" | "lowLatency" | "highQuality";

export interface InstanceMicConfig {
  instanceId: number;
  enabled: boolean;
  forwardingEnabled: boolean;
  autoConnect: boolean;
  transport: string;
  codec: string;
  sampleRate: number;
  channels: number;
  vmWireguardIp: string;
  rtpPort: number;
  rtcpPort: number;
  localRtcpPort: number;
  rtpPayloadType: number;
  deviceId: string;
  deviceName: string;
  qualityProfile: MicQualityProfile;
  sessionId: string | null;
  sessionToken: string | null;
  ssrc: number | null;
  lastEnabledAt: string | null;
  lastDisabledAt: string | null;
}

export type MicState =
  | "disabled"
  | "starting"
  | "connecting"
  | "streaming"
  | "no_audio_detected"
  | "wireguard_disconnected"
  | "vm_agent_unreachable"
  | "cloud_mic_missing"
  | "packet_loss_high"
  | "pipewire_unavailable"
  | "no_microphone"
  | "capture_failure"
  | "pipeline_failure"
  | "network_failure"
  | "reconnecting"
  | "degraded"
  | "error";

export interface InstanceMicRuntimeStatus {
  enabled: boolean;
  state: MicState;
  reconnectCount: number;
  vmAgentReachable: boolean;
  deviceReady: boolean;
  receivingAudio: boolean;
  transport: string;
  sampleRate: number;
  channels: number;
  bitrateKbps: number;
  frameMs: number;
  packetLossPercent: number;
  jitterMs: number;
  bufferDepthMs: number;
  lastPacketMsAgo: number | null;
  pipewireConnected: boolean;
  defaultSource: boolean;
  muted: boolean;
  sidecarHealthy: boolean;
  captureSampleRate: number;
  captureOverruns: number;
  ringFillMs: number;
  appsrcQueueMs: number;
  opusPacketsSent: number;
  bytesSent: number;
  error: string | null;
}

export interface MicSessionResponse {
  sessionId: string;
  sessionToken: string;
  ssrc: number;
  vmWireguardIp: string;
  rtpPort: number;
  rtcpPort: number;
  localRtcpPort: number;
  rtpPayloadType: number;
  sampleRate: number;
  channels: number;
  frameMs: number;
  bitrateKbps: number;
}

export interface MicSidecarMetrics {
  capturedSamples: number;
  consumedSamples: number;
  droppedStaleSamples: number;
  overruns: number;
  underruns: number;
  silenceSamples: number;
  buffersPushed: number;
  captureRestarts: number;
  captureErrors: number;
  pipelineErrors: number;
  ringDepthSamples: number;
  appsrcQueueMs: number;
  opusPacketsSent: number;
  bytesSent: number;
  currentRtpSequence: number | null;
  sampledAtUnixMs: number;
}

export interface MicSettingsUpdate {
  deviceId?: string;
  qualityProfile?: MicQualityProfile;
  forwardingEnabled?: boolean;
  autoConnect?: boolean;
}

export interface MicrophoneDevice {
  id: string;
  name: string;
  isDefault: boolean;
  sampleRates: number[];
  channels: number;
}
