# Schemas and Data Contracts

This document summarizes the core schemas that define app behavior.

## 1) Persisted application state

Source: `src-tauri/src/models/app_state.rs` (`PersistedAppState`)

Top-level fields:

- `version`
- `onboardingCompleted`
- `credentials`
- `ssh`
- `location`
- `serverPreferences`
- `selectedOffer`
- `instance`
- `wireguard`
- `sunshine`
- `moonlight`
- `moonlightPreferences`
- `sharedStorage`
- `provisionedServers`
- `postWireguardSetup`
- `orchestrationState`
- `lastError`

Storage file path is managed by `JsonStateStore` and defaults to app data `state.json`.

## 2) Orchestration state machine

Enum: `OrchestrationState`

Main values:

- setup/provisioning: `GeneratingSshKey`, `UploadingSshKeyToVast`, `CreatingInstance`, `WaitingForInstance`, `ConnectingSsh`, `ConfiguringNvidiaHeadless`, `ConfiguringSunshine`, `ConfiguringWireGuard`
- post-WireGuard guided flow: `WireGuardConfigGenerated`, `WireGuardAppHandoffStarted`, `WireGuardWaitingForImport`, `WireGuardWaitingForActivation`, `WireGuardVerifying`, `WireGuardConnected`, `MoonlightSunshineReadyToSetup`, `SunshineCredentialsConfiguring`, `SunshineVerifying`, `MoonlightDetecting`, `MoonlightPairingStarted`, `MoonlightPinReceived`, `SunshinePinSubmitting`, `MoonlightSunshinePaired`
- terminal-ish: `Ready`, `Error`

## 3) Post-WireGuard setup schema

Primary struct: `PostWireGuardSetupState`

Important fields:

- stage tracking: `stage`, `wireguardSetupStatus`, `wireguardSetupMode`
- context: `currentInstanceId`, `wireguardExportPath`, `wireguardConfig`, `wireguardVerifiedHost`, `wireguardReachablePorts`
- pairing flags: `moonlightInstalled`, `paired`, `setupComplete`
- recoverable error channel: `lastError` (`SetupErrorState`)

Enums:

- `SetupStage`
- `WireGuardSetupStatus`
- `WireGuardSetupMode`

## 4) Per-instance checkpoint schema

`ProvisionedServerState` + `ProvisionedServerSteps`

Tracks whether each provisioning checkpoint has completed for a given instance:

- SSH key ready/uploaded
- instance created/ready
- SSH connected
- NVIDIA headless configured
- Sunshine configured
- low-latency audio configured
- WireGuard configured
- Moonlight configured
- awaiting pair pin
- pairing completed
- post-provision completed

## 5) Provisioning event schema

Source: `src-tauri/src/models/events.rs`

`ProvisioningEvent` fields:

- `state: OrchestrationState`
- `message: string`
- `details?: string`
- `timestamp: DateTime<Utc>`
- `isError: boolean`

This is emitted to the frontend over `orchestration:progress`.

## 6) Shared storage schemas

Key types:

- `SharedStorageState`
- `SharedStorageSettings`
- `SharedStorageSettingsUpdate`
- `BackupStatusResponse`
- `SharedStorageInstanceStatus`
- `BundleIndex`, `AppBundle`, `FolderBundle`
- `RestoreRequest`, `RestoreDryRunResult`, `RestoreJob`

These cover backup configuration, bundle discovery, dry-run planning, and restore execution status.

## 7) Microphone passthrough schemas

Key types:

- `InstanceMicConfig`
- `MicQualityProfile`
- `InstanceMicRuntimeStatus`
- `MicState`
- `MicSettingsUpdate`
- `MicSessionResponse`

Session IDs and tokens are generated at runtime and tracked in-memory by the mic service.

## 8) Frontend API type mirror

Source: `src/lib/types.ts`

The frontend mirrors backend schemas (camelCase) for command results and state hydration. Keep both files aligned when introducing schema changes.

## 9) Backward compatibility guidance

- Add new fields with serde defaults when possible.
- Avoid removing or renaming persisted fields without migration logic.
- Keep enum additions additive unless coordinated with frontend handling.
