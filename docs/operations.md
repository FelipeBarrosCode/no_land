# Operations Runbook

## Local development

- frontend only: `npm run dev`
- desktop app: `npm run tauri:dev`
- production desktop bundle: `npm run tauri:build`

## Build and release workflows

### Shared multi-platform build

Workflow: `.github/workflows/release.yml`

Triggers:

- push to `main`
- tag push `v*`
- manual dispatch

Targets in shared matrix:

- macOS Apple Silicon (`aarch64-apple-darwin`)
- macOS Intel (`x86_64-apple-darwin`, built on `macos-15-intel`)
- Ubuntu x64 (`x86_64-unknown-linux-gnu`)
- Ubuntu ARM64 (`aarch64-unknown-linux-gnu`)
- Windows x64 (`x86_64-pc-windows-msvc`)
- Windows ARM64 (`aarch64-pc-windows-msvc`)

### Release publishing behavior

- On version tags (`v*`): publish normal release assets.
- On `main` pushes: publish/update prerelease tag `main-latest` with fresh artifacts.

## Packaging outputs

Tauri bundle outputs are uploaded from target release bundle folders and include:

- macOS: `.dmg` / `.app`
- Windows: `.msi` / `.exe`
- Linux: `.AppImage`, `.deb`, `.rpm` (where produced)

## Provisioning observability

### Event stream

- Channel: `orchestration:progress`
- Payload: `ProvisioningEvent`

### Logs

- Backend uses `tracing` logs
- UI shows timeline and recent logs through `get_provisioning_logs`
- Persistent state lives in the app data directory as `state.json`

## Recovery and troubleshooting checklist

### Sunshine pairing issues

1. Verify WireGuard tunnel connectivity (`verify_wireguard`).
2. Verify Sunshine API/auth (`verify_sunshine`).
3. Run guided setup (`setup_moonlight_sunshine_command`).
4. Submit PIN (`submit_moonlight_pin_to_sunshine_command`).

### Reboot flow issues

1. `reboot_instance_services`
2. wait for reconnect/system-ready gates
3. inspect Sunshine/audio recovery logs

### Managed tunnel control

The desktop app owns the local GotaTun tunnel lifecycle and can reconnect it from the instance controls. Do not ask users to install or operate WireGuard, `wg-quick`, or a standalone GotaTun binary.

## Security notes

- Credentials are persisted in JSON state for current phase requirements.
- Sensitive values are redacted in logs where applicable.
- SSH actions run via explicit command wrappers and timeouts.
- macOS release builds fail unless Developer ID signing and Apple notarization credentials are configured.
- Windows Authenticode signing and Linux repository/package signing are not yet configured.
