# Noland Connect (Phase 1)

Noland Connect is a Tauri 2 desktop setup assistant for Sunshine + Moonlight game streaming on Vast.ai GPU servers.

## Documentation index

Project-level docs are in `docs/`:

- `docs/README.md` (entry point)
- `docs/architecture.md`
- `docs/flows.md`
- `docs/schemas.md`
- `docs/api-reference.md`
- `docs/configuration.md`
- `docs/operations.md`

This phase implements a full vertical slice:

- onboarding with local credentials + Vast API key
- local JSON state persistence behind a storage trait
- SSH key generation and auto-upload to Vast
- offer discovery and ranking (location + reliability + cost + VRAM)
- instance creation + polling
- remote provisioning orchestration over SSH (`std::process::Command` wrappers)
- local Moonlight config backup + patching
- guided pairing workflow with PIN submission

## Stack

- Tauri 2
- Rust (`tokio`, `reqwest`, `serde`, `tracing`)
- React + TypeScript + Vite
- Tailwind CSS
- Zustand

## Shout-outs / upstream projects

Big shout-out to the upstream projects that make Noland Connect possible.

### App and developer tooling

- [Tauri](https://github.com/tauri-apps/tauri) — desktop app framework
- [React](https://github.com/facebook/react) — frontend UI layer
- [Vite](https://github.com/vitejs/vite) — frontend build/dev tooling
- [Tailwind CSS](https://github.com/tailwindlabs/tailwindcss) — styling system
- [Zustand](https://github.com/pmndrs/zustand) — client-side state management

### Streaming, networking, and media stack

- [Sunshine](https://github.com/LizardByte/Sunshine) — remote game streaming host
- [Moonlight Qt](https://github.com/moonlight-stream/moonlight-qt) — Moonlight desktop client lineage
- [moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c) — core GameStream protocol implementation used by the embedded client stack
- [WireGuard tools](https://github.com/WireGuard/wireguard-tools) — tunnel tooling reference and interoperability target
- [GotaTun](https://github.com/mullvad/gotatun) — embedded userspace tunnel engine used by `noland-net-helper`
- [GStreamer](https://github.com/GStreamer/gstreamer) — media runtime used in the native streaming stack
- [PipeWire](https://github.com/PipeWire/pipewire) — low-latency Linux audio/media runtime on provisioned hosts
- [WirePlumber](https://github.com/PipeWire/wireplumber) — PipeWire session manager used in the host audio setup

### Service integrations

- Vast.ai — core cloud provider integration used by Noland Connect. I did not find a clearly verifiable official public source repository for Vast.ai, so the repo shout-out here stays with their docs instead:
  - https://docs.vast.ai/api-reference/introduction

## Project layout

```text
src/
  app/
  components/
  features/
    onboarding/
    dashboard/
    servers/
    provisioning/
    settings/
  lib/
  store/

src-tauri/src/
  main.rs
  commands/
  models/
  errors/
  services/
    state_store.rs
    vast_api.rs
    location.rs
    offer_selector.rs
    ssh_keys.rs
    instance_manager.rs
    remote_exec.rs
    wireguard.rs
    sunshine.rs
    moonlight.rs
    nvidia_headless.rs
    orchestration.rs
```

## Setup

1. Install JS and Rust dependencies:

```bash
npm install
```

2. Run frontend-only dev server:

```bash
npm run dev
```

3. Run full desktop app (Tauri):

```bash
npm run tauri:dev
```

4. Production build:

```bash
npm run tauri:build
```

## Beta desktop releases (direct download)

This repo is configured to publish direct-download desktop binaries (no app stores).

- Shared GitHub Actions workflow: `.github/workflows/release.yml`
- Dedicated macOS Intel workflow: `.github/workflows/release-macos-intel.yml`
- Triggers automatically on pushes to `main` and tags like `v0.1.1`
- Builds on macOS, Linux, and Windows runners
- Uploads artifacts to the workflow run and attaches installers to the GitHub Release

### Manual trigger

Use **Actions -> Build Desktop Binaries -> Run workflow**.

### Tag-based release trigger

```bash
git tag v0.1.1
git push origin v0.1.1
```

### Main branch rolling prerelease

- Every push to `main` updates prerelease tag `main-latest` with fresh build artifacts.

### Downloadable artifacts produced

- macOS: `.dmg` (and app archive)
- Windows: `.msi` / `.exe`
- Linux: `.AppImage` and distro packages when supported by runner tooling

macOS release artifacts are required to use a Developer ID Application certificate and Apple notarization; the release build fails when those credentials are absent. Windows Authenticode signing and Linux repository/package signing are still pending.

## State persistence

State is saved to a single JSON file in the OS app data directory:

- file name: `state.json`
- service: `src-tauri/src/services/state_store.rs`
- abstraction: `StateStore` trait + `JsonStateStore` implementation

This keeps persistence swappable later (OS keychain / Stronghold) with minimal refactor.

## Vast.ai references

This codebase references official docs and keeps uncertain payload assumptions isolated in the API adapter layer.

- API intro: https://docs.vast.ai/api-reference/introduction
- Search offers: https://docs.vast.ai/api-reference/search/search-offers
- Create instance: https://docs.vast.ai/api-reference/instances/create-instance
- SSH docs: https://docs.vast.ai/documentation/instances/connect/ssh
- API key flow: https://cloud.vast.ai/cli/

## What is implemented now

- **Onboarding**
  - Validates username/password/api key
  - Persists credentials in local JSON (phase 1 requirement)
  - Generates `nolandConnectSSH` ed25519 key if missing
  - Uploads pubkey to Vast if not already present

- **Server discovery**
  - IP geolocation fallback command
  - manual location override
  - Vast offer search via typed backend client
  - ranking by nearest distance, then cheapest, then highest VRAM
  - selected offer persistence + storage adjustment

- **Play orchestration**
  - create instance from selected offer + template hash
  - poll every 60s until SSH-ready
  - verify instance reservation ownership in your Vast account before SSH connection
  - SSH connect test
  - run NVIDIA headless checks + Sunshine install/config + low-latency PipeWire/WirePlumber setup + WireGuard setup
  - patch Moonlight local config with backup
  - transition to pairing state and accept PIN

- **Per-server provision checkpoints**
  - backend persists per-instance step completion state (`provisionedServers`)
  - saves WireGuard + Moonlight runtime artifacts per instance to safely restore skipped steps
  - on restart/re-run, completed steps are skipped and only pending steps run
  - tracks last known state and last error per provisioned instance

- **Rented server reuse**
  - lists currently rented Vast instances in dashboard
  - can start provisioning from a rented instance directly
  - if selected offer becomes unavailable (`no_such_ask`), attempts to reuse an active rented server

- **UI**
  - modern dark dashboard shell
  - onboarding screen
  - Netflix-style rows and cards
  - server picker modal
  - provisioning timeline + logs
  - pairing modal
  - settings stub screen

## Safe defaults and central config

All main assumptions are centralized in:

- `src-tauri/src/services/app_config.rs`

Notable defaults:

- template hash: `2a62a7d5089a50a5ad89a9480f540d25`
- minimum reliability: `0.85`
- poll interval: `60s`
- Sunshine baseline config values
- WireGuard addressing defaults

## Low-latency audio setup (Ubuntu)

During remote provisioning, Noland applies an idempotent low-latency audio task for PipeWire + WirePlumber + Sunshine.

Configured files on the server (for target user, default `user`):

- `~/.config/pipewire/pipewire.conf.d/99-lowlatency.conf`
- `~/.config/pipewire/pipewire-pulse.conf.d/10-lowlatency.conf`
- `~/.config/wireplumber/wireplumber.conf.d/10-alsa-lowlatency.conf`
- `/etc/security/limits.d/audio.conf`

Additional server actions:

- ensures `rtkit` is installed
- ensures target user is in `audio` group
- restarts user services (`pipewire`, `pipewire-pulse`, `wireplumber`) via user bus env
- optionally sets CPU governor to `performance` if `cpupower` exists
- preserves existing `audio_sink` in Sunshine unless override is explicitly enabled

Verification output includes:

- `pw-top` (or metadata fallback)
- `pactl info`
- `pactl list short sinks`
- `pactl list short sources`
- target user groups
- rtprio guidance
- CPU governor info

### Fallback profiles for crackling/underruns

Set environment variable before launching app:

- `NOLAND_AUDIO_PROFILE=aggressive` (default)
- `NOLAND_AUDIO_PROFILE=fallback1` (uses `512/48000` pulse req/quantum)
- `NOLAND_AUDIO_PROFILE=fallback2` (uses `1024/48000` pulse req/quantum)

Other useful envs:

- `NOLAND_AUDIO_TARGET_USER` (default `user`)
- `NOLAND_AUDIO_FORCE_SINK_OVERRIDE=true`
- `NOLAND_AUDIO_SINK_OVERRIDE=<sink-name>`

## Important notes

- Secrets are intentionally stored in JSON in phase 1 per requirement.
- Secret values are not logged.
- Remote setup steps are idempotent-oriented and include validation gates.
- Moonlight config is backed up before editing.
- Vast API calls are logged with method/endpoint/status/latency at `info` level to help diagnose reservation and provisioning issues.

## Deferred for next phase

- stronger resume/recovery from mid-provisioning failures beyond current state tracking
- richer per-step retry controls in UI
- robust WireGuard client import automation per platform
- production-hardened Sunshine pairing command variants across distributions
