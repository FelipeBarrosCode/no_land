# Operations Runbook

## Local development

- frontend only: `npm run dev`
- desktop app: `npm run tauri:dev`
- production desktop bundle: `npm run tauri:build`

## CI/CD execution modes

The workflows deliberately separate untrusted pull-request checks, protected production
publishing, and the protected nightly release rehearsal.

### Pull Request CI

Workflow: `.github/workflows/ci.yml`

Trigger: `pull_request` (plus manual dispatch for maintainers). It has read-only repository
access and receives no `Secrets` environment, signing identity, cloud credential, release token,
or Azure OIDC permission.

Blocking lanes:

- `typecheck-build`: `npm ci`, TypeScript compilation, and a Vite production build.
- `rust-format`: rustfmt for all six lockfile roots.
- `rust-check`: `cargo check --locked` and Clippy for network-contracts, network-agent,
  state-agent, the desktop/net-helper workspace, mic-sidecar, and vm-cloud-mic-agent.
  Network contracts and network-agent are warning-free and use `-D warnings`. Other workspaces
  currently run Clippy with the existing-warning baseline; warnings remain visible and Clippy
  errors still fail.
- `unit-tests`: the complete test workflow described below.
- `security`: Gitleaks, dependency review, npm audit, RustSec cargo audits, Trivy,
  CodeQL, and actionlint.
- `PR CI required`: fails unless every preceding lane succeeds.

There is no JavaScript unit-test framework configured today. `npm run test:unit` is retained,
but it runs the network-contract and network-agent Rust tests and is not treated as the complete
test suite.

### Complete test surface

Reusable workflow: `.github/workflows/_tests.yml`

Production and nightly run every lane after security succeeds; PR CI runs the same tests without
performing release builds:

- `npm run test:unit` (network-contracts and network-agent).
- `cargo test --locked --workspace --all-targets` for `src-tauri`, including the desktop backend
  and `noland-net-helper`.
- all state-agent workspace unit and integration tests, including `noland-testkit` attribution,
  commit/safety, and end-to-end backup/restore tests.
- mic-sidecar and vm-cloud-mic-agent tests.
- native Moonlight C smoke and latency-policy tests through CMake/CTest.

Network provisioning, Direct/TURN/WireGuard profile, packet probe, rollback, MTU, and firewall
tests are Rust tests inside the network-agent, network-contracts, desktop, and net-helper lanes.
Tests requiring real external infrastructure remain environment-dependent and are not simulated
with production credentials in PRs.

### Production release

Workflow: `.github/workflows/release.yml`

Triggers: a trusted push to `main`, or an explicit manual run whose ref is `main`.

Execution order:

1. Frontend/script validation.
2. All blocking security gates.
3. Complete test suite.
4. Version calculation without modifying refs.
5. Signed six-platform build.
6. Per-platform package, sidecar, updater, signing, and notarization validation.
7. Final package extraction/security scan, CycloneDX SBOM, and SHA-256 generation.
8. Production-only tag and GitHub Release publication.

The publish job is unreachable unless every preceding job succeeds. It also requires all of:

- repository exactly `FelipeBarrosCode/no_land`;
- ref exactly `refs/heads/main`;
- event exactly `push` or `workflow_dispatch`;
- protected `Secrets` environment approval where configured.

Only the final `release` job has `contents: write`. Azure OIDC (`id-token: write`) is limited to
the protected build call and used only by Windows signing jobs.

### Nightly release rehearsal

Workflow: `.github/workflows/nightly.yml`

Triggers: daily at `06:23 UTC` and manual dispatch. It runs validation, full security, optional
staging DAST, the complete tests, and the same protected signed six-platform build as production.
Set repository variable `NOLAND_ZAP_STAGING_URL` to a dedicated staging HTTP target to enable the
OWASP ZAP baseline. This desktop repository has no deployable HTTP service, so ZAP reports itself
as not applicable when that variable is absent rather than scanning the Vite development server.

**Nightly binaries are never published.** The nightly workflow contains no tag, GitHub Release,
R2, store, updater-channel, or distribution job. It calls the reusable build with artifact
retention disabled; every platform validates locally and then deletes its bundle. No `.exe`,
`.msi`, `.dmg`, `.app.zip`, `.deb`, `.rpm`, or updater archive is uploaded. Only security/DAST
reports may be retained. The final job is `discard-nightly-binaries`.

### Six-platform matrix and signing

Reusable workflow: `.github/workflows/_build.yml`

- macOS Apple Silicon (`aarch64-apple-darwin`)
- macOS Intel (`x86_64-apple-darwin`, on `macos-15-intel`)
- Linux x64 (`x86_64-unknown-linux-gnu`)
- Linux ARM64 (`aarch64-unknown-linux-gnu`)
- Windows x64 (`x86_64-pc-windows-msvc`)
- Windows ARM64 (`aarch64-pc-windows-msvc`)

macOS requires Developer ID signing, Apple notarization, stapling, Gatekeeper validation, and
Tauri updater signatures. Windows installers are signed through Azure Artifact Signing using
OIDC, validated with Authenticode, and then have their Tauri updater signatures regenerated.
Linux `.deb` files pass structural checks and lintian. Every target runs bundled-sidecar/runtime
validation and generates local SHA-256 checksums.

### Security policy and reports

- Gitleaks: any detected credential, token, or private key fails.
- npm audit and dependency review: HIGH and CRITICAL fail; current MEDIUM/LOW findings are
  reported.
- RustSec: actionable Rust security advisories fail for every Cargo.lock.
- Trivy: HIGH and CRITICAL fixed findings fail; all severities are uploaded as SARIF. Repository
  secrets are intentionally left to Gitleaks to avoid duplicate scanners.
- CodeQL: JavaScript/TypeScript and native C/C++ results appear in GitHub code scanning. Rust is
  covered by RustSec, Clippy, and tests because CodeQL does not support Rust.
- actionlint: malformed expressions, unsafe workflow constructs, and workflow syntax errors fail.
- Production artifacts: extracted package filesystems are scanned, an SBOM is emitted, and the
  curated release set receives `SHA256SUMS`.

Security reports appear in the workflow run artifacts and GitHub Security/Code scanning where
SARIF is supported. Reports must never contain credentials.

`.gitleaks.toml` allows only the exact deterministic 64-hex token shown in the public
network-agent protocol example; it does not exclude the file or any credential pattern generally.

An intentional vulnerability exception must be narrow and documented in this file (advisory or
CVE, affected package, owner, justification, compensating control, and expiry/removal date) before
adding it to a scanner-specific ignore file. Broad project, directory, or severity exclusions are
not acceptable.

Current exception:

- `RUSTSEC-2023-0071` (`rsa` 0.9.x, MEDIUM), owner: desktop/Moonlight maintainers. The upstream
  crate has no fixed stable release and is used only for local Sunshine pairing key generation and
  protocol operations, not as a remotely exposed general-purpose signing service. Keys use OS CSPRNG
  generation, remain local, and pairing attempts are network/access controlled. Cargo audit still
  reports the advisory while the explicit ID is allowed. Review by **2026-12-31**, or remove sooner
  when a compatible fixed `rsa` release is available.

### Workflow credentials

- PR CI: default read-only `GITHUB_TOKEN`; no protected production secrets.
- Production/nightly macOS: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
  `KEYCHAIN_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`, optional Apple API key
  values already supported by the build scripts, and Tauri signing key/password.
- Production/nightly Windows: Azure client, tenant, and subscription IDs for OIDC Artifact
  Signing, plus Tauri signing key/password.
- Production package metadata: `CLOUDFLARE_R2_PUBLIC_BASE_URL` is read only to construct existing
  store metadata. Nightly never invokes metadata preparation or any R2 operation.
- Production release: the job-scoped write `GITHUB_TOKEN` pushes the calculated tag and creates
  the GitHub Release.

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

- Long-lived Cloudflare and per-instance control credentials use native secure storage and are
  cached only for the process lifetime; they are not persisted in `state.json`.
- Sensitive values are redacted in logs where applicable.
- SSH actions run via explicit command wrappers and timeouts.
- macOS release builds fail unless Developer ID signing and Apple notarization credentials are configured.
- Windows release installers must pass Azure Artifact Signing and Authenticode validation.
- Linux packages are validated locally; publication to a Linux package repository is not part of
  these workflows.
