# Frontend/Backend Command Reference

The UI uses wrappers in `src/lib/backend.ts`, which map to Tauri commands in `src-tauri/src/commands/mod.rs`.

## State and onboarding

- `get_app_state`
- `complete_onboarding`
- `refresh_ip_location`
- `set_manual_location`
- `set_os_location`

## Offer search and selection

- `search_offers`
- `select_offer`
- `get_rented_instances`

## Provisioning start and pairing

- `start_play_flow`
- `start_play_existing_instance`
- `submit_pairing_pin`
- `skip_pairing_and_continue`
- `get_provisioning_logs`

## WireGuard setup and guided handoff

- `setup_wireguard_client`
- `reconnect_local_wireguard_client_quick`

- `setup_wireguard_app_handoff_command`
- `verify_wireguard`
- `open_wireguard_app_command`
- `download_wireguard_config_command`
- `get_setup_status_command`
- `verify_sunshine`
- `detect_moonlight`
- `setup_moonlight_sunshine_command`
- `submit_moonlight_pin_to_sunshine_command`
- `retry_setup_stage_command`

## Local environment and power

- `local_environment_preflight`
- `start_local_sleep_prevention`
- `stop_local_sleep_prevention`

## Moonlight client

- `get_moonlight_download_url`
- `launch_moonlight_client`
- `configure_moonlight_client`
- `restore_moonlight_backup`

## WireGuard download URL

- `get_wireguard_download_url`

## Settings updates

- `update_vast_api_key`
- `update_platform_credentials`
- `update_server_preferences`
- `update_moonlight_preferences`
- `regenerate_edid`
- `update_ssh_credentials`

## Shared storage and backups

- `get_shared_storage_settings`
- `save_shared_storage_settings`
- `test_shared_storage_config`
- `trigger_instance_backup`
- `trigger_instance_backup_for`
- `sync_instance_from_shared_storage`
- `list_instance_shared_storage_objects`
- `sync_instance_from_shared_storage_selected`
- `list_instance_exportable_storage_objects`
- `save_instance_to_shared_storage_selected`
- `get_instance_backup_status`
- `setup_instance_backup_schedule`
- `remove_instance_backup_schedule`

## Sunshine settings for an instance

- `get_instance_sunshine_settings`
- `update_instance_sunshine_settings`
- `reset_instance_sunshine_settings`

## Instance lifecycle

- `reconnect_instance_wireguard` (manual app-open behavior)
- `reboot_instance_services`
- `pause_instance`
- `destroy_instance`

## Bundle index and restore

- `generate_bundle_index`
- `get_instance_restore_bundles`
- `dry_run_restore`
- `restore_bundle`
- `get_restore_job`

## Microphone passthrough

- `get_instance_mic_config`
- `update_instance_mic_settings`
- `enable_instance_mic`
- `disable_instance_mic`
- `reconnect_instance_mic`
- `recreate_instance_mic_device`
- `get_instance_mic_status`

## Notes

- Commands are local Tauri IPC calls, not remote HTTP endpoints.
- For every command that mutates setup state, frontend should refresh `get_app_state` or `get_setup_status` to avoid stale UI.
- Long-running steps stream progress through `orchestration:progress` events.
