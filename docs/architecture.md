# Architecture

## High-level model

Noland Connect is a Tauri desktop application with:

- React/TypeScript UI (`src/`)
- Rust backend services (`src-tauri/src/`)
- A persisted JSON app state (`state.json`) managed by a state store abstraction

The UI invokes backend commands through Tauri IPC and reacts to live provisioning events.

## Runtime components

### Frontend

- `src/app/App.tsx`: app shell and route composition
- `src/store/appStore.ts`: client-side state, long-running action orchestration, event subscription
- `src/lib/backend.ts`: typed command wrappers over Tauri `invoke`
- `src/features/onboarding/*`: onboarding form and tutorial modal
- `src/features/dashboard/*`: primary server dashboard and instance actions
- `src/features/provisioning/*`: provisioning timeline and post-WireGuard modal
- `src/features/settings/*`: settings and shared storage entrypoint
- `src/features/shared-storage-manager/*`: backup, sync, export, and Sunshine settings UI
- `src/features/restore/*`: bundle restore UI
- `src/features/servers/*`: rented server picker

### Backend

- `src-tauri/src/main.rs`: bootstraps plugins, loads state, registers commands, starts background monitors
- `src-tauri/src/commands/mod.rs`: IPC command handlers
- `src-tauri/src/services/*`: domain services (Vast, orchestration, WireGuard, Sunshine, Moonlight, lifecycle, storage, mic)
- `src-tauri/src/models/*`: state and API payload schemas
- `src-tauri/src/errors/*`: app and frontend-safe error shapes

## Core backend service boundaries

- `orchestration.rs`: main create/reuse provisioning flow and checkpointing
- `post_wireguard_setup.rs`: guided post-tunnel setup and embedded-client/Sunshine pairing
- `wireguard.rs`: remote tunnel provisioning plus embedded GotaTun lifecycle and monitoring
- `sunshine.rs`: Sunshine install/config/health and credentials bootstrap
- `moonlight/*`: embedded GameStream discovery, pairing, launch, stream runtime, and input pipeline
- `native/noland-moonlight`: platform render/audio/input backends (AVFoundation/AppKit on macOS, GStreamer/SDL on Linux, Media Foundation/SDL on Windows)
- `sleep_inhibit.rs`: local system sleep prevention during active sessions
- `instance_lifecycle.rs`: actions on existing instances (pause, reboot, destroy, settings)
- `reboot_helper.rs`: reboot/reconnect/service re-init pipeline
- `post_provision.rs`: executes packaged `scripts/post_provision.sh`
- `shared_storage/*`: backup/export/restore orchestration
- `mic_passthrough.rs`: cloud mic session control and runtime status

## State model

Persistent state is stored in `state.json` and loaded into `PersistedAppState`.

- Primary schema: `src-tauri/src/models/app_state.rs`
- Frontend mirror types: `src/lib/types.ts`
- Access path: `AppContext` + `StateStore` (`JsonStateStore` default)
- Storage location: app data directory + `state.json`

## Execution model

1. Frontend invokes a command.
2. Command delegates to one or more services.
3. Services update persisted app state through `AppContext::update_state(...)`.
4. Provisioning-related services emit `ProvisioningEvent` messages.
5. Frontend store listens to `orchestration:progress` and updates UI/state.

## Background loops

`main.rs` starts two important background tasks:

- orchestration resume check (`OrchestrationService::resume_if_needed`)
- local WireGuard tunnel monitor (`maintain_persisted_local_tunnel`) every 30 seconds

## Design constraints and assumptions

- Remote VM automation targets Linux hosts.
- The installed client owns its tunnel and streaming engines; users are not expected to install WireGuard, GotaTun, or Moonlight.
- Release targets cover x86_64 and ARM64 on macOS, Linux, and Windows.
- Provisioning uses checkpoint markers so repeated runs can skip already-completed steps safely.
- Frontend state is a cache over backend state; authoritative writes happen in Rust through `AppContext::update_state(...)`.
