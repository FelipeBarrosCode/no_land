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
  | "ConfiguringMoonlight"
  | "AwaitingPairPin"
  | "Pairing"
  | "Ready"
  | "Error";

export interface CredentialsState {
  appUsername: string;
  appPassword: string;
  vastApiKey: string;
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
}

export interface MoonlightState {
  configured: boolean;
  hostAddress: string;
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
  wireguardConfigured: boolean;
  moonlightConfigured: boolean;
  awaitingPairPin: boolean;
  pairingCompleted: boolean;
}

export interface ProvisionedServerState {
  instanceId: number;
  offerId: number | null;
  sshHost: string;
  sshPort: number;
  status: string;
  sshCommand: string;
  wireguardServerIp: string;
  wireguardClientIp: string;
  wireguardServerPublicKey: string;
  wireguardClientPublicKey: string;
  wireguardConfigPath: string;
  moonlightHostAddress: string;
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
}

export interface RentedInstanceSummary {
  instanceId: number;
  label: string;
  status: string;
  gpuName: string;
  sshHost: string;
  sshPort: number;
  publicIp: string;
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

export interface PersistedAppState {
  version: number;
  onboardingCompleted: boolean;
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
  provisionedServers: ProvisionedServerState[];
  orchestrationState: OrchestrationState;
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
// Bundle Index + Restore types
// ============================================================

export interface BundleHost {
  username: string;
  home: string;
  os: string;
}

export interface FolderBundle {
  id: string;
  label: string;
  source: string;
  target: string;
  kind: string;
  defaultSelected: boolean;
}

export interface AppBundle {
  id: string;
  name: string;
  type: string;
  confidence: number;
  signals: string[];
  folderBundles: FolderBundle[];
}

export interface BundleIndex {
  schemaVersion: number;
  generatedAt: string;
  instanceId: number;
  snapshotId: string;
  host: BundleHost;
  bundles: AppBundle[];
}

export interface RestoreRequest {
  bundleId: string;
  folderBundleIds: string[];
  mode: string;
}

export interface RestoreDryRunItem {
  folderBundleId: string;
  label: string;
  source: string;
  target: string;
  kind: string;
  action: string;
}

export interface RestoreDryRunResult {
  wouldRestore: RestoreDryRunItem[];
  totalFilesEstimate: number;
}

export interface RestoreJobItem {
  folderBundleId: string;
  label: string;
  source: string;
  target: string;
  kind: string;
  status: string;
  error: string | null;
}

export interface RestoreJob {
  jobId: string;
  instanceId: number;
  bundleId: string;
  mode: string;
  status: string;
  startedAt: string;
  finishedAt: string | null;
  items: RestoreJobItem[];
  error: string | null;
}

// ============================================================
// Microphone Passthrough types
// ============================================================

export type MicQualityProfile = "standard" | "lowLatency" | "highQuality";

export interface InstanceMicConfig {
  instanceId: number;
  enabled: boolean;
  transport: string;
  codec: string;
  sampleRate: number;
  channels: number;
  vmWireguardIp: string;
  rtpPort: number;
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
  | "error";

export interface InstanceMicRuntimeStatus {
  enabled: boolean;
  state: MicState;
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
  error: string | null;
}

export interface MicSessionResponse {
  sessionId: string;
  sessionToken: string;
  ssrc: number;
  vmWireguardIp: string;
  rtpPort: number;
  sampleRate: number;
  channels: number;
  frameMs: number;
  bitrateKbps: number;
}

export interface MicSettingsUpdate {
  qualityProfile?: MicQualityProfile;
}
