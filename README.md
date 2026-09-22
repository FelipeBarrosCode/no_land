<div align="center">

# No Land

### Turn on-demand GPU cloud machines into personal remote gaming PCs.

[![Rust](https://img.shields.io/badge/Rust-Tauri-000000?logo=rust)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app/)
[![React](https://img.shields.io/badge/React-TypeScript-20232a?logo=react)](https://react.dev/)
[![Linux](https://img.shields.io/badge/Linux-Ubuntu-FCC624?logo=linux&logoColor=black)](https://ubuntu.com/)
[![WireGuard](https://img.shields.io/badge/WireGuard-Networking-88171A?logo=wireguard)](https://www.wireguard.com/)

**GPU orchestration · automated provisioning · low-latency streaming · networking · remote storage**

[Website](https://no-land.net) · [Architecture](docs/architecture.md) · [Flows](docs/flows.md) · [Configuration](docs/configuration.md)

</div>

---

## Why No Land exists

Cloud gaming solves the cost of owning high-end hardware, but many services still restrict users to a predefined game catalog.

No Land explores a different model: rent a real GPU-powered Linux machine, provision it automatically, connect it securely, and use it as your own remote gaming PC.

The project focuses less on game catalog management and more on the systems underneath remote computing: **GPU infrastructure, Linux provisioning, networking, streaming, audio, storage, and client orchestration**.

## What it does

From one desktop app, No Land can:

1. authenticate locally and connect to a user's Vast.ai account;
2. discover and rank GPU offers by location, reliability, price, storage, and VRAM;
3. create or reuse a rented GPU instance;
4. wait for the machine to become SSH-ready;
5. configure NVIDIA headless graphics, Sunshine, audio, WireGuard, and supporting services;
6. patch the local Moonlight configuration and guide pairing;
7. persist provisioning checkpoints so completed steps can be safely skipped on retry;
8. expose a native streaming path built around Moonlight/GameStream components.

## System flow

```text
Desktop app
   │
   ▼
Vast.ai offer discovery
   │
   ▼
GPU instance creation / reuse
   │
   ▼
SSH provisioning orchestrator
   ├── NVIDIA / display
   ├── Sunshine
   ├── PipeWire / WirePlumber
   ├── WireGuard
   └── runtime validation
   │
   ▼
Moonlight pairing + native client
   │
   ▼
Low-latency remote gaming session
```

## Engineering highlights

- **Rust orchestration layer** — Tauri backend built with `tokio`, `reqwest`, `serde`, and structured tracing.
- **Cloud offer ranking** — selects machines using location, reliability, price, storage, and VRAM signals.
- **Automated SSH provisioning** — turns a generic rented GPU machine into a usable remote gaming environment.
- **Idempotent recovery** — stores per-server provisioning checkpoints and resumes only incomplete steps.
- **Secure networking** — integrates WireGuard and an embedded userspace tunnel path.
- **Low-latency Linux audio** — configures PipeWire/WirePlumber profiles for Sunshine streaming and includes fallback profiles for underruns/crackling.
- **Native streaming work** — documents frame pipelines, queues, timing domains, packet sizing, reconnect behaviour, and latency optimization experiments.
- **Cross-platform release pipeline** — GitHub Actions builds desktop binaries for macOS, Windows, and Linux.

## Stack

| Area | Technology |
| --- | --- |
| Desktop | Tauri 2, Rust |
| UI | React, TypeScript, Vite, Tailwind CSS, Zustand |
| Async / HTTP | Tokio, Reqwest |
| Cloud compute | Vast.ai GPU instances |
| Streaming | Sunshine, Moonlight, moonlight-common-c |
| Networking | WireGuard, GotaTun, UDP |
| Media | GStreamer, PipeWire, WirePlumber |
| Platform | Linux / NVIDIA |
| CI/CD | GitHub Actions |

## Architecture and documentation

Project documentation lives in `docs/`:

- [`docs/README.md`](docs/README.md) — documentation entry point
- [`docs/architecture.md`](docs/architecture.md) — high-level system architecture
- [`docs/flows.md`](docs/flows.md) — user and provisioning flows
- [`docs/schemas.md`](docs/schemas.md) — persisted/runtime data shapes
- [`docs/api-reference.md`](docs/api-reference.md) — API notes
- [`docs/configuration.md`](docs/configuration.md) — runtime configuration
- [`docs/operations.md`](docs/operations.md) — operational guidance

### Streaming implementation notes

- [`docs/moonlight-client-pipeline.md`](docs/moonlight-client-pipeline.md) — native frame pipeline, queues, timing domains, and ownership map
- [`docs/moonlight-client-optimizations.md`](docs/moonlight-client-optimizations.md) — latency feature flags, source precedents, platform limits, and validation matrix
- [`docs/moonlight-adaptive-packet-size.md`](docs/moonlight-adaptive-packet-size.md) — adaptive GameStream packet sizing, path hints, cache, scoring, and controlled reconnect

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

## Run locally

Install dependencies:

```bash
npm install
```

Frontend development:

```bash
npm run dev
```

Full desktop app:

```bash
npm run tauri:dev
```

Production build:

```bash
npm run tauri:build
```

## Desktop releases

The repository includes GitHub Actions workflows for direct-download desktop builds.

- macOS artifacts include `.dmg` / app archives;
- Windows builds produce installer artifacts;
- Linux builds produce AppImage and package formats when supported by the runner;
- pushes to `main` can update a rolling prerelease;
- version tags can publish release artifacts.

macOS release builds are designed to require Developer ID signing and notarization credentials. Windows Authenticode and Linux package signing remain separate release-hardening work.

## Provisioning state and recovery

No Land persists runtime state through a `StateStore` abstraction. Provisioned servers keep step-level completion state plus the runtime artifacts needed to resume safely.

That allows a failed or restarted provisioning run to continue from the remaining steps rather than rebuilding the machine from scratch.

## Low-latency audio

During host provisioning, No Land configures PipeWire and WirePlumber for remote streaming and validates the result with system-level tooling.

The provisioning logic can apply multiple fallback profiles when aggressive low-latency settings cause underruns on a particular machine.

## Upstream projects

No Land builds on excellent open-source work including:

- [Sunshine](https://github.com/LizardByte/Sunshine)
- [Moonlight Qt](https://github.com/moonlight-stream/moonlight-qt)
- [moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c)
- [WireGuard](https://github.com/WireGuard/wireguard-tools)
- [GotaTun](https://github.com/mullvad/gotatun)
- [GStreamer](https://github.com/GStreamer/gstreamer)
- [PipeWire](https://github.com/PipeWire/pipewire)
- [WirePlumber](https://github.com/PipeWire/wireplumber)

Vast.ai is the current cloud-provider integration used for GPU instance discovery and provisioning.

## Current focus

The project is actively evolving around:

- resilience on unstable networks;
- streaming latency and packet delivery;
- adaptive networking and reconnect behaviour;
- cross-platform tunnel integration;
- stronger provisioning recovery;
- remote application restore and shared storage workflows.

---

<div align="center">

**Cloud gaming without giving up the freedom of a real PC.**

</div>
