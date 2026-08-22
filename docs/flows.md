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

## 11) Launch library flow

1. The instance-card Play button opens the launch library instead of starting a stream immediately.
2. The Launch PC card calls the existing `start_play_existing_instance` path unchanged.
3. `get_instance_launch_library` merges apps currently discovered by the state agent with apps stored in the shared-storage catalog.
4. Software artwork is requested lazily through `get_software_artwork`; IGDB failures fall back to a placeholder.
5. Installed software starts the embedded desktop stream and launches through Steam, a desktop entry, or a discovered executable over SSH.
6. Cloud-only software restores its latest complete-application bundle first, refreshes discovery, starts the stream, and then launches the app.
7. Launch status is returned as a job and can be refreshed with `get_launch_instance_software_job`.

## 12) Microphone passthrough flow

Service: `MicPassthroughService`

1. persist forwarding, auto-connect, device, and quality preferences per instance
2. when a Moonlight session starts, resolve its Noland instance and schedule mic startup independently
3. authorize a short-lived host endpoint over SSH, allocate RTP/RTCP ports, and restrict them to the WireGuard peer
4. enumerate and select devices through `noland-mic-sender`, then capture with CPAL on Windows/Linux or GStreamer CoreAudio on macOS into a bounded stale-dropping ring and GStreamer RTP/Opus pipeline
5. supervise the sidecar every three seconds and replace a dead, hung, or failed child without recreating the remote PipeWire source
6. receive/decode into persistent `noland_mic_sink` / `noland_mic_source` topology on the host
7. on stream close, stop sender/receiver while preserving the user's auto-connect preference and `Noland Microphone`
8. expose WireGuard, capture, queue, packet-loss, jitter, and PipeWire health independently from Moonlight/Sunshine status

## 13) Progress events and UI updates

Event schema: `ProvisioningEvent`

- emitted via `orchestration:progress`
- consumed by frontend store (`subscribeProvisioningEvents`)
- used to drive timeline, status text, and recoverable error states

## 14) Local settings and preference updates

Settings commands update persisted state directly and then refresh the frontend snapshot:

- Vast credentials and platform credentials
- server preference filters and template/storage selection defaults
- Moonlight preferences
- SSH credentials
- Sunshine EDID regeneration (`regenerate_edid`)
