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
- Linux: `.deb` and `.rpm` only

Linux AppImage is intentionally **not** produced or published. WebKitGTK/GIO loads host
modules at runtime while AppImage injects a bundled `usr/lib` into the loader path; mixing
the two produces GLib/GIO/libcurl symbol lookups failures on Ubuntu/Zorin LTS (for example
`g_task_set_static_name`, `g_assertion_message_cmpint`). Native packages keep the distro's
GTK/WebKit/GIO stack as one compatible set.

## Linux .deb dependency model

The rule for Linux native packages: **bundled, sanitized GStreamer runtime + system desktop
stack + explicit distro dependencies for every library left system-side.**

What ships inside the package:

- The application binary and static-linked native code (moonlight core, SDL2, Opus).
- A sanitized GStreamer closure under `/usr/lib/Noland Connect/binaries/gstreamer/<triple>/`
  (core libs, plugins, `gst-plugin-scanner`, libcrypto, ffmpeg closure). Resolved through the
  binary's inherited `DT_RPATH`.
- `noland-mic-sender`, `noland-net-helper` sidecars.
- `ssh`/`scp`/`ssh-keygen` wrappers that `exec /usr/bin/...` (no bundled OpenSSH closure).
- `state-agent` and `vm-cloud-mic-agent` sources for remote bootstrapping.

What must stay on the distro (declared in `deb.depends` in `tauri.conf.json`):

- WebKitGTK/GTK/AppIndicator/librsvg desktop stack.
- GLib family (`libglib2.0-0`: glib/gobject/gio/gmodule) for the bundled GStreamer libs.
- ALSA for SDL audio output and the mic sidecar's cpal capture (`libasound2`).
- PipeWire client lib for `libgstpipewire.so` (`libpipewire-0.3-0`).
- udev/gudev for the V4L2 plugin family (`libudev1`, `libgudev-1.0-0`).
- VA-API/VDPAU/DRM and GL/EGL/GLES for the VA-API/GL plugins and SDL render paths.
- X11/XCB/xkbcommon/Wayland/decor libraries SDL and the desktop stack dlopen at runtime.
- `openssh-client`, `xdg-utils`, `procps` for tools the app and net-helper shell out to.

The GStreamer staging step (`bootstrap-native-deps.mjs`) excludes all of the above from the
bundled closure, and `verify-bundled-sidecars.mjs` fails the build if any distro-owned
library shows up inside the packaged runtime. Keep those two lists in sync.

Minimum supported distro level is determined by WebKitGTK 4.1: Ubuntu 22.04+, Debian 12+,
Zorin 17+.

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
