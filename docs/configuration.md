# Configuration

## Runtime configuration source

Backend config is defined in `src-tauri/src/services/app_config.rs` and reads environment variables at startup.

## Core environment variables

### Vast and orchestration

- `NOLAND_TEMPLATE_HASH`
- `NOLAND_MIN_RELIABILITY`
- `NOLAND_OFFERS_SEARCH_LIMIT`
- `NOLAND_VAST_BASE_URL`
- `NOLAND_INSTANCE_SSH_USER`

### Audio profile

- `NOLAND_AUDIO_TARGET_USER`
- `NOLAND_AUDIO_PROFILE` (`aggressive`, `fallback1`, `fallback2`)
- `NOLAND_AUDIO_FORCE_SINK_OVERRIDE`
- `NOLAND_AUDIO_SINK_OVERRIDE`

### Sunshine

- `NOLAND_SUNSHINE_BIND_ADDRESS`
- `NOLAND_SUNSHINE_CSRF_ALLOWED_ORIGINS`

### WireGuard

- `NOLAND_WIREGUARD_CLIENT_LISTEN_PORT`
- `NOLAND_WIREGUARD_QOS_MODE`
- `NOLAND_WIREGUARD_QOS_BANDWIDTH_MBIT`
- `NOLAND_WIREGUARD_QOS_DIFFSERV`
- `NOLAND_WIREGUARD_DSCP_ENABLED`

## Important defaults

### Core runtime defaults

- state schema version: `1`
- Vast base URL: `https://console.vast.ai`
- offer search hard cap: `500`
- instance poll interval: `60s`
- instance readiness max attempts: `120`
- SSH probe attempts: `60`
- SSH probe interval: `30s`
- default SSH user: `root`

### Sunshine defaults

- encoder: `nvenc`
- cpu_affinity: `2-5`
- capture: auto-detected and persisted
- av1_mode: `1`
- hevc_mode: `0`
- minimum_fps_target: `60`
- nvenc_latency_over_power: `enabled`
- nvenc_preset: `3`
- fec_percentage: `25`
- ping_timeout: `30000`
- web UI port: `47989` (streaming API/web paths include 47990 where applicable)

### WireGuard defaults

- tunnel: `10.77.0.1/24` server, `10.77.0.2/32` client
- interface: `wg0`
- server listen port: `51820`
- client listen port: `51821`
- keepalive: `25`
- MTU: `1280`
- QoS mode: `cake`
- QoS diffserv profile: `diffserv4`
- DSCP enabled by default unless `NOLAND_WIREGUARD_DSCP_ENABLED` is explicitly set falsey

### Moonlight defaults

- bitrate: `20000`
- fps: `60`
- resolution: `1920x1080`
- refresh rate mode: `60`
- host audio / keep awake / frame pacing / vsync enabled
- HDR disabled by default

### EDID defaults

- mode: `auto_detect`
- refresh rate: `60Hz`
- fallback generation profile: `1920x1080 @ 60Hz`

## Download URL defaults

- Moonlight (all OSes): `https://github.com/moonlight-stream/moonlight-qt/releases`
- WireGuard (all OSes): `https://www.wireguard.com/install/`

## Local tool prerequisites (client side)

Checked by `local_environment_preflight`:

- `ssh`
- `ssh-keygen`
- `ssh-add`
- Windows additionally: `wireguard.exe`
- Linux additionally: `xdg-open`

Other local capabilities are used elsewhere, but the preflight command currently validates only the tools listed above.

## State schema versioning

- Config exposes `state_schema_version`.
- Current default: `1`.
- Use serde defaults and additive fields to preserve backward compatibility.
