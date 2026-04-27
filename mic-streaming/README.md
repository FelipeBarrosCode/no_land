# Noland Mic Streaming (GStreamer RTP/Opus)

This implementation streams your local microphone to a remote VM over UDP/RTP Opus and exposes it as a virtual microphone named `Cloud_Mic`.

It is designed for Ubuntu/Debian VMs with PipeWire (pipewire-pulse) or PulseAudio-compatible `pactl`.

## What you get

- Low-latency Opus RTP stream (`5002/udp` by default)
- Remote virtual mic pipeline:
  - sink: `cloud_mic_sink`
  - source: `Cloud_Mic`
- Idempotent setup scripts
- Start/stop/status scripts
- Optional systemd user units
- Config stored at `~/.config/noland-mic/config.env`

## Architecture

- Local sender:
  - macOS: `osxaudiosrc`
  - Linux: `pulsesrc`
- Wire format:
  - Opus RTP payload type 96
  - UDP to `VM_IP:PORT`
- Remote receiver:
  - `udpsrc -> rtpjitterbuffer -> rtpopusdepay -> opusdec -> pulsesink`

## Defaults

- Port: `5002`
- Sample rate: `48000`
- Channels: `1`
- Opus bitrate: `64000`
- Opus frame size: `2.5`
- Jitter buffer: `10ms`

## Install

### 1) Local machine

```bash
chmod +x mic-streaming/scripts/*.sh
mic-streaming/scripts/install-local.sh
```

This creates `~/.config/noland-mic/config.env` if missing.

### 2) Remote VM

Copy `mic-streaming` folder to VM (or clone repo), then:

```bash
chmod +x mic-streaming/scripts/*.sh
mic-streaming/scripts/install-remote.sh user
mic-streaming/scripts/create-virtual-mic.sh
mic-streaming/scripts/start-receiver.sh
```

## Configure

Edit local and remote config:

`~/.config/noland-mic/config.env`

Set `VM_IP` to your VM WireGuard IP (not public IP):

```bash
VM_IP=10.77.0.2
PORT=5002
SAMPLE_RATE=48000
CHANNELS=1
OPUS_BITRATE=64000
OPUS_FRAME_SIZE=2.5
JITTER_LATENCY_MS=10
VIRTUAL_MIC_SINK=cloud_mic_sink
VIRTUAL_MIC_SOURCE=Cloud_Mic
```

## Firewall

Open UDP port on VM:

```bash
sudo ufw allow 5002/udp
```

If using WireGuard and strict rules, allow UDP on the WG interface path too.

## Start / Stop / Status

### Remote VM

```bash
mic-streaming/scripts/start-receiver.sh
mic-streaming/scripts/stop-receiver.sh
mic-streaming/scripts/status.sh
```

### Local machine

```bash
mic-streaming/scripts/start-sender.sh
mic-streaming/scripts/stop-sender.sh
mic-streaming/scripts/status.sh
```

## Auto-start when WireGuard connects

Add hooks to your local WireGuard client config:

```ini
PostUp = /home/<you>/.local/share/noland-mic/scripts/start-sender.sh
PostDown = /home/<you>/.local/share/noland-mic/scripts/stop-sender.sh
```

On macOS with `wireguard-go`, point to your local script path.

## Optional systemd user services

Install scripts and service files:

```bash
mkdir -p ~/.local/share/noland-mic/scripts ~/.config/systemd/user
cp mic-streaming/scripts/*.sh ~/.local/share/noland-mic/scripts/
cp mic-streaming/systemd/noland-mic-receiver.service ~/.config/systemd/user/
chmod +x ~/.local/share/noland-mic/scripts/*.sh

systemctl --user daemon-reload
systemctl --user enable --now noland-mic-receiver.service
```

## Testing

1. Start receiver on VM
2. Start sender on local machine
3. On VM check source exists:

```bash
pactl list short sources | grep Cloud_Mic
```

4. In browser/Discord inside VM, pick microphone `Cloud_Mic`

## Troubleshooting

- **No audio level moving**
  - Verify sender is running: `mic-streaming/scripts/status.sh`
  - Check receiver log: `tail -f /tmp/noland-mic-receiver.log`
  - Confirm `VM_IP` is WG IP and reachable.

- **UDP blocked**
  - Check `ufw status`
  - Ensure `5002/udp` is allowed
  - If provider only exposes selected ports, use WireGuard tunnel IP and WG route.

- **Wrong mic selected**
  - In app, select `Cloud_Mic`
  - Confirm source exists: `pactl list short sources | grep Cloud_Mic`

- **PipeWire/Pulse not running**
  - `systemctl --user status pipewire pipewire-pulse`
  - `pactl info` must succeed

- **Audio crackling/dropouts**
  - Increase `JITTER_LATENCY_MS` from 10 to 20 or 30
  - Increase `OPUS_FRAME_SIZE` from 2.5 to 5 or 10
  - Lower `OPUS_BITRATE` on weaker links

- **Browser/Discord not seeing Cloud_Mic**
  - Restart app after creating source
  - Set default source: `pactl set-default-source Cloud_Mic`
  - For Flatpak apps, ensure Pulse socket permissions are allowed.

## Notes

- Logs are written to `/tmp` by design.
- Scripts are idempotent where possible.
- This pipeline is independent from Sunshine default sink and uses its own virtual mic path.
