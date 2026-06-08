# End-to-End Flows

## 1) Onboarding flow

1. User submits platform username/password + Vast API key.
2. Backend validates payload and persists credentials.
3. SSH keypair (`nolandConnectSSH`) is generated if missing.
4. Public key is uploaded to Vast if not present.
5. App transitions to `Idle` and user can search/select offers.

Command entrypoint: `complete_onboarding`

## 2) Offer discovery and selection

1. Backend fetches offers from Vast.
2. Offer selector ranks offers by configured scoring.
3. User selects offer and storage preference.
4. Selection is persisted and used by orchestration start.

Command entrypoints: `search_offers`, `select_offer`

## 3) Main provisioning (selected offer)

Service: `OrchestrationService::start_play_flow`

Primary stages (orchestration state timeline):

1. `GeneratingSshKey`
2. `UploadingSshKeyToVast`
3. `CreatingInstance`
4. `WaitingForInstance`
5. `VerifyingReservation`
6. `ConnectingSsh`
7. `ConfiguringNvidiaHeadless`
8. `ConfiguringSunshine`
9. `ConfiguringWireGuard`
10. transition into post-WireGuard guided flow

Checkpoint model:

- Per-instance step completion is persisted in `provisionedServers[].steps`.
- Re-runs can skip completed steps and continue from pending markers.

## 4) Existing rented instance flow

Service: `OrchestrationService::start_play_for_existing_instance`

- Rehydrates instance context from rented/provisioned records.
- Re-validates health and resumes provisioning checkpoints as needed.

## 5) Post-WireGuard guided setup flow

Service: `post_wireguard_setup.rs`

Purpose: hand off local tunnel control to the WireGuard app and complete pairing.

Stages (`SetupStage`):

1. `wireguard_config_generated`
2. `wireguard_app_handoff_started`
3. `wireguard_waiting_for_import` (macOS) or `wireguard_waiting_for_activation` (Windows/Linux)
4. `wireguard_verifying`
5. `wireguard_connected`
6. `moonlight_sunshine_ready_to_setup`
7. `sunshine_credentials_configuring`
8. `sunshine_verifying`
9. `moonlight_detecting`
10. `moonlight_pairing_started`
11. `moonlight_pin_received`
12. `sunshine_pin_submitting`
13. `moonlight_sunshine_paired`
14. `setup_complete`

Completion actions after successful PIN submit:

- pairing PIN saved in memory
- `PairingCompleted` step marked
- post-provision script executed (`post_provision.sh`)
- `PostProvisionCompleted` step marked (on success)

## 6) Local WireGuard utility flow

Commands:

- `setup_wireguard_client`
- `reconnect_local_wireguard_client_quick`

Purpose: support direct local WireGuard setup/reconnect outside the guided app handoff flow.

## 7) Post-provision installs flow

Service: `PostProvisionService::run`

- Decodes bundled shell script to `/tmp/noland-post-provision.sh`
- Executes it as root with target user argument
- Removes temp script
- Surfaces stdout/stderr in structured logs

This is used from both classic pairing completion paths and the post-WireGuard completion path.

## 8) Reboot and service recovery flow

Service: `RebootHelperService::reboot_and_reinitialize`

Sequence:

1. remote pre-reboot service prep + reboot scheduling
2. wait for SSH disconnect
3. wait for SSH reconnect
4. wait for `systemctl is-system-running` ready state
5. post-reboot audio readiness check/recovery
6. Sunshine post-reboot recovery and health verification

## 9) Instance lifecycle operations

Service: `InstanceLifecycleService`

Main actions:

- reboot services
- pause instance
- destroy instance
- Sunshine settings read/update/reset
- manual WireGuard reconnect for an instance
- backup/restore triggers and shared storage sync

## 10) Shared storage backup and restore flow

Services:

- `shared_storage/shared_storage_manager.rs`
- `shared_storage/bundle_indexer.rs`
- `shared_storage/bundle_restore.rs`

Capabilities:

- one-click or selected-path backup
- backup schedule install/remove
- restore bundle discovery
- dry-run restore planning
- restore job execution and polling

## 11) Microphone passthrough flow

Service: `MicPassthroughService`

1. check instance and WireGuard prerequisites
2. create in-memory session token/SSRC
3. notify VM agent (best effort)
4. track runtime status and reconnect/disable operations

## 12) Progress events and UI updates

Event schema: `ProvisioningEvent`

- emitted via `orchestration:progress`
- consumed by frontend store (`subscribeProvisioningEvents`)
- used to drive timeline, status text, and recoverable error states

## 13) Local settings and preference updates

Settings commands update persisted state directly and then refresh the frontend snapshot:

- Vast credentials and platform credentials
- server preference filters and template/storage selection defaults
- Moonlight preferences
- SSH credentials
- Sunshine EDID regeneration (`regenerate_edid`)
