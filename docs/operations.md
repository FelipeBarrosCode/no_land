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
- macOS Intel (`x86_64-apple-darwin`)
- Ubuntu x64 (`x86_64-unknown-linux-gnu`)
- Ubuntu ARM64 (`aarch64-unknown-linux-gnu`)
- Windows x64 (`x86_64-pc-windows-msvc`)

### Dedicated macOS Intel build

Workflow: `.github/workflows/release-macos-intel.yml`

This workflow also builds `x86_64-apple-darwin` on `macos-13`. At the moment, Intel macOS artifacts are produced both here and in the shared matrix workflow.

Target:

- macOS Intel (`x86_64-apple-darwin`)

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

### Manual WireGuard control

Noland no longer auto-repairs the local WireGuard tunnel during health-monitor drift cases. Use the WireGuard app directly for manual reconnect/activation.

## Security notes

- Credentials are persisted in JSON state for current phase requirements.
- Sensitive values are redacted in logs where applicable.
- SSH actions run via explicit command wrappers and timeouts.
- Release workflows in this repo do not document code signing or notarization.
