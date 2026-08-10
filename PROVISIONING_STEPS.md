# Noland Connect - Complete Provisioning Flow

This document describes all the steps in the Noland Connect provisioning workflow for setting up a Vast.ai GPU instance with Sunshine + Moonlight for game streaming.

## Overview

The provisioning flow consists of these main stages:
0. Offer Selection & Preflight Validation
1. SSH Key Generation
2. SSH Key Upload to Vast
3. Instance Creation
4. Instance Readiness Wait
4.1 Reservation Verification & Runtime Validation
5. SSH Connectivity Check
6. NVIDIA Headless Setup (TwinView)
6.1 Post-NVIDIA Reboot Resume Checkpoint
7. Sunshine Installation & Configuration
8. Low-Latency Audio Setup
9. WireGuard Tunnel Setup
9.1 Network Queue Management + Kernel Network Tuning
9.2 Local WireGuard Client Activation & Health Gate
10. Moonlight Local Configuration
11. Pairing (PIN Entry)

---

## Stage 0: Offer Selection & Preflight Validation

**Purpose:** Select a host that can actually run VM runtime + direct UDP tunneling.

**Steps:**
1. Query Vast offers with production filters (category + geolocation + reliability)
2. Require (API-side where available):
   - `vms_enabled = true`
   - `static_ip = true`
   - `num_gpus = 1`
   - `reliability >= 0.8`
   - `direct_port_count >= 1`
3. Optional toggles from UI:
   - `verification = verified`
   - `datacenter = true`
   - `has_avx = true`
4. Persist selected offer and pricing/network metadata in app state

---

## Stage 1: SSH Key Generation

**Purpose:** Generate Ed25519 SSH keypair for authentication with the Vast.ai instance.

**Steps:**
1. Check if SSH keypair already exists at `~/.ssh/nolandConnectSSH`
2. If not exists, generate new keypair:
   ```bash
   ssh-keygen -t ed25519 -f ~/.ssh/nolandConnectSSH -N "" -C "nolandConnectSSH"
   ```
3. Verify private key exists and has correct permissions (600)

---

## Stage 2: SSH Key Upload to Vast.ai

**Purpose:** Upload the public key to Vast.ai so it's injected into new instances.

**Steps:**
1. Read the generated public key
2. Call Vast.ai API: `POST /api/v0/keys`
3. Store upload confirmation in app state

---

## Stage 3: Instance Creation

**Purpose:** Create a new rented GPU instance on Vast.ai.

**Steps:**
1. Call Vast.ai API: `POST /api/v0/asks/{offer_id}/instances` with:
   - `template_id` (or `template_hash`)
   - `storage` (GB)
2. Receive instance details including:
   - `id` - Instance ID
   - `status` - Instance status
   - `ssh_host` - SSH hostname/proxy
   - `ssh_port` - SSH port
   - `public_ipaddr` - Actual public IP
   - `ports` - Dict of exposed ports (SSH 22, WireGuard 51820/udp, etc.)

---

## Stage 4: Instance Readiness Wait

**Purpose:** Wait for the instance to boot and become ready for SSH connections.

**Steps:**
1. Poll Vast.ai API: `GET /api/v0/instances/{id}` every 60 seconds
2. Check `ssh_ready` field or `status`
3. Continue when status indicates instance is running and SSH is ready
4. Mark `InstanceReady` step as completed

---

## Stage 5: SSH Connectivity Check

**Purpose:** Verify the bundled SSH client can connect using the app-managed private key directly.

**Steps:**
1. Test SSH connection:
   ```bash
   ssh -o BatchMode=yes -o PreferredAuthentications=publickey \
       -i <key_path> -p <port> <user>@<host> "echo connected"
   ```
2. Retry up to 10 times with 15-second intervals if connection fails
3. Mark `SshConnected` step as completed

---

## Stage 6: NVIDIA Headless Setup (TwinView)

**Purpose:** Configure NVIDIA GPU for headless streaming using TwinView virtual display.

**Reference:** Based on [LizardByte Headless SSH Setup Guide](https://docs.lizardbyte.dev/projects/sunshine/v0.23.0/about/guides/linux/headless_ssh.html)

### Step 6.1: Check for Existing Display

```bash
nvidia-smi --query-gpu=name --format=csv,noheader && DISPLAY=:0 xrandr 2>/dev/null | grep -c '\+'
```

- If displays exist, skip to Stage 7
- If headless, continue with TwinView setup

### Step 6.2: Detect GPU Output

```bash
nvidia-smi --query-gpu=name --format=csv,noheader
xrandr --listproviders
```

Common outputs: `DP-0`, `HDMI-0`, `DFP-0`

### Step 6.3: Create Xorg TwinView Config

Write to `/etc/X11/xorg.conf.d/30-nvidia-virtual.conf`:

```xorg
Section "ServerLayout"
   Identifier "TwinLayout"
   Screen 0 "metaScreen" 0 0
EndSection

Section "Monitor"
   Identifier "Monitor0"
   Option "Enable" "true"
EndSection

Section "Device"
   Identifier "Card0"
   Driver "nvidia"
   VendorName "NVIDIA Corporation"
   Option "MetaModes" "1920x1080"
   Option "ConnectedMonitor" "DP-0"
   Option "ModeValidation" "NoDFPNativeResolutionCheck,NoVirtualSizeCheck,NoMaxPClkCheck,NoHorizSyncCheck,NoVertRefreshCheck,NoWidthAlignmentCheck"
EndSection

Section "Screen"
   Identifier "metaScreen"
   Device "Card0"
   Monitor "Monitor0"
   DefaultDepth 24
   Option "TwinView" "True"
   SubSection "Display"
       Modes "1920x1080"
   EndSubSection
EndSection
```

### Step 6.4: Set DRM Permissions

```bash
sudo chmod 666 /dev/dri/card0 /dev/dri/renderD128
```

### Step 6.5: Stop Display Managers

```bash
sudo systemctl stop gdm sddm lightdm
sudo systemctl mask gdm sddm lightdm
```

### Step 6.6: Kill Existing Xorg

```bash
sudo pkill -9 Xorg
sleep 2
```

### Step 6.7: Start Xorg with TwinView

```bash
sudo /usr/bin/Xorg :0 -config /etc/X11/xorg.conf.d/30-nvidia-virtual.conf \
    -nocursor -auth /root/.Xauthority -logfile /var/log/Xorg.0.log &
sleep 8
```

### Step 6.8: Verify Xorg & Setup Xauthority

```bash
pgrep -x Xorg  # Should return PID
DISPLAY=:0 xrandr --listmonitors  # Should show virtual display
xauth generate :0 . trusted
sudo cp /root/.Xauthority /run/user/<uid>/.Xauthority
sudo chown <user>:<user> /run/user/<uid>/.Xauthority
```

### Step 6.9: Create User Config Directories

```bash
mkdir -p ~/.config/pipewire/pipewire.conf.d ~/.config/wireplumber ~/.config/systemd/user
```

---

## Stage 7: Sunshine Installation & Configuration

**Purpose:** Install and configure Sunshine game streaming server.

### Step 7.1: Install Sunshine

```bash
sudo apt-get update
sudo apt-get install -y sunshine
```

### Step 7.2: Create Sunshine Configuration

Write to `/etc/sunshine/sunshine.conf`:

```
address_family = both
port = 47989
origin_pin_allowed = all
origin_web_ui_allowed = all
upnp = off
capture = nvfbc
encoder = nvenc
output_name = DP-0
nvenc_preset = p4
ping_timeout = 10000
```

### Step 7.3: Create Web UI Credentials

**IMPORTANT:** Credentials are required for pairing to work.

1. Open `https://<wireguard-ip>:47990` in your browser
2. Accept the self-signed certificate
3. Create your login credentials on the signup page
4. Then proceed to pairing

### Step 7.4: Create Systemd Service

Write to `/etc/systemd/system/sunshine.service`:

```ini
[Unit]
Description=Sunshine Game Stream Host
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
Environment=DISPLAY=:0
Environment=XAUTHORITY=/root/.Xauthority
Restart=always
RestartSec=10
ExecStartPre=/bin/bash -c 'chmod 666 /dev/dri/card0 /dev/dri/renderD128 2>/dev/null; exit 0'
ExecStart=/usr/bin/sunshine
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

### Step 7.5: Enable and Start Sunshine

```bash
sudo systemctl daemon-reload
sudo systemctl enable sunshine
sudo systemctl start sunshine
```

### Step 7.6: Verify Sunshine is Running

```bash
sudo systemctl status sunshine
sudo ss -tlnp | grep sunshine  # Should show listening on 47990, 47989, 47984, 48010
```

---

## Stage 8: Low-Latency Audio Setup

**Purpose:** Configure PipeWire/WirePlumber for low-latency audio streaming.

### Step 8.1: Copy Audio Setup Script

Copy `src-tauri/scripts/setup_low_latency_audio.sh` to remote and execute.

### Step 8.2: Script Actions

1. Check for dpkg lock (wait up to 600 seconds)
2. Install PipeWire if needed:
   ```bash
   sudo apt-get install -y pipewire pipewire-audio-client-libraries \
       wireplumber libspa-0.2-bluetooth
   ```
3. Create user directories:
   ```bash
   mkdir -p ~/.config/pipewire/pipewire.conf.d
   mkdir -p ~/.config/wireplumber/wireplumber.conf.d
   ```
4. Copy PipeWire config files
5. Restart PipeWire:
   ```bash
   systemctl --user restart pipewire pipewire-pulse wireplumber
   ```
6. Verify audio:
   ```bash
   pactl info | grep "Server Name"
   ```

---

## Stage 9: WireGuard Tunnel Setup

**Purpose:** Create encrypted tunnel from client to GPU server.

### Step 9.1: Install WireGuard on the remote GPU VM

The client never installs or invokes these tools locally. They configure the server-side kernel tunnel endpoint only.

```bash
sudo apt-get update
sudo apt-get install -y wireguard wireguard-tools
```

### Step 9.2: Generate Server Keys

```bash
cd /etc/wireguard
wg genkey | tee server_private.key | wg pubkey > server_public.key
chmod 600 server_private.key
```

### Step 9.3: Generate Client Keys

```bash
wg genkey | tee client_private.key | wg pubkey > client_public.key
```

### Step 9.4: Create Server Config

Write to `/etc/wireguard/wg0.conf`:

```ini
[Interface]
Address = 10.77.0.1/24
ListenPort = 51820
PrivateKey = <server_private_key>
PostUp = iptables -A FORWARD -i wg0 -j ACCEPT; iptables -t nat -A POSTROUTING -o eth0 -j MASQUERADE
PostDown = iptables -D FORWARD -i wg0 -j ACCEPT; iptables -t nat -D POSTROUTING -o eth0 -j MASQUERADE

[Peer]
PublicKey = <client_public_key>
AllowedIPs = 10.77.0.2/32
```

### Step 9.5: Create Client Config

Write to local file (e.g., `~/noland-wg.conf`):

```ini
[Interface]
PrivateKey = <client_private_key>
Address = 10.77.0.2/32
DNS = 1.1.1.1

[Peer]
PublicKey = <server_public_key>
Endpoint = <server_public_ip>:<wireguard_mapped_port>
AllowedIPs = 10.77.0.1/32
PersistentKeepalive = 25
```

**IMPORTANT:** 
- Use mapped external UDP port from `instances.ports["51820/udp"][0].HostPort`
- `AllowedIPs` must be `10.77.0.1/32` (only server IP, not full tunnel)
- No `DNS` field to avoid routing issues

### Step 9.6: Enable IP Forwarding

```bash
echo 1 | sudo tee /proc/sys/net/ipv4/ip_forward
sudo sysctl -w net.ipv4.ip_forward=1
```

### Step 9.7: Start WireGuard

```bash
sudo systemctl enable wg-quick@wg0
sudo systemctl start wg-quick@wg0
```

### Step 9.8: Verify WireGuard

```bash
sudo wg show  # Should show interface and peer
ip addr show wg0  # Should show 10.77.0.1
```

### Step 9.9: Endpoint Sync (critical)

Before activating the local client tunnel, refresh endpoint from Vast API:

1. Call `GET /api/v0/instances/{id}`
2. Resolve endpoint host from:
   - `public_ipaddr` (preferred)
   - fallback: `ssh_host`
3. Resolve endpoint port from `ports["51820/udp"][0].HostPort`
4. Rewrite local client config `[Peer] Endpoint = <host>:<mapped_port>`
5. If `wireguard_port == 0`, fail fast with explicit error

### Step 9.10: Local Client Activation (macOS/Linux/Windows)

**Cross-platform implementation details:**
1. Normalize `AllowedIPs` to `10.77.0.1/32` only.
2. Launch the bundled privileged `noland-net-helper`; do not invoke `wg`, `wg-quick`, WireGuard.exe, or a separately installed GotaTun binary.
3. The helper embeds GotaTun and creates/configures the TUN adapter directly. Windows uses the packaged architecture-specific Wintun DLL.
4. Read the helper-owned runtime status and validate:
   - reject full-tunnel (`0.0.0.0/0`)
   - require the expected peer and `AllowedIPs = 10.77.0.1/32`
   - require a fresh helper heartbeat and active process
   - require a recent handshake or successful reachability probe
5. Ping `10.77.0.1` as the final health gate and automatically repair stale or failed local tunnel state.

### Step 9.11: Server-side Routing/NAT apply (separate from PostUp)

After `wg0` is up, apply idempotent routing/NAT explicitly:

```bash
sudo sysctl -w net.ipv4.ip_forward=1
sudo iptables -C FORWARD -i wg0 -j ACCEPT || sudo iptables -A FORWARD -i wg0 -j ACCEPT
sudo iptables -C FORWARD -o wg0 -j ACCEPT || sudo iptables -A FORWARD -o wg0 -j ACCEPT
sudo iptables -t nat -C POSTROUTING -o <primary_nic> -j MASQUERADE || \
  sudo iptables -t nat -A POSTROUTING -o <primary_nic> -j MASQUERADE
```

---

## Stage 9.1: Network Queue Management + Kernel Tuning

**Purpose:** Keep latency stable under load and reduce network jitter on host side.

**Steps:**
1. Detect primary egress NIC via `ip route get 1.1.1.1`
2. Install persistent qdisc service (`noland-qdisc.service`) that applies `fq_codel`
3. Apply runtime tuning:
   - `net.ipv4.ip_forward=1`
   - `rp_filter=0` for `all/default/<egress>/<wg_iface>`
   - set tunnel MTU
4. Validate with:
   - `tc -s qdisc show dev wg0`
   - `tc -s qdisc show dev <egress>`

---

## Stage 4.1: Reservation/Runtime Verification

**Purpose:** Prevent provisioning against incompatible instances before remote configuration.

**Checks performed against instance payload:**
1. `image_runtype` / runtime must be VM-compatible
2. `public_ipaddr` must be present (or valid fallback)
3. SSH host/port resolution must succeed
4. WireGuard UDP mapping (`51820/udp`) must be present

If any of these fail, provisioning stops early with explicit error.

---

## Stage 10: Moonlight Local Configuration

**Purpose:** Configure Moonlight client on Mac to connect to streaming server.

### Step 10.1: Detect Moonlight Config Location

- macOS: `~/Library/Preferences moonlight-stream Moonlight Game Streaming.plist`
- Linux: `~/.config/moonlight/`
- Windows: Registry or AppData

### Step 10.2: Update Config

Add/update server entry:
- `host` = `10.77.0.1` (WireGuard server IP)
- `port` = `47984`
- `codec` = auto

---

## Stage 11: Pairing

**Purpose:** Authenticate Moonlight client with Sunshine server.

### Step 11.1: Start Pairing Flow in Moonlight

1. Open Moonlight on client
2. Click "Add PC" or "+"
3. Enter host: `10.77.0.1`
4. Moonlight displays PIN (e.g., "1234-5678")

### Step 11.2: Enter PIN in Sunshine Web UI

1. Open browser to `https://10.77.0.1:47990`
2. Login with credentials set in Step 7.3
3. Click "PIN" tab
4. Enter PIN from Moonlight
5. Click "Send" or "Pair"

**IMPORTANT:** PIN expires after ~60 seconds. Enter immediately.

### Step 11.3: Alternative - CLI Pairing

If Sunshine CLI is available:

```bash
printf '%s\n' '<pin>' | sunshine-cli pair
```

Or:

```bash
sunshine --pair-pin '<pin>'
```

### Step 11.4: Manual Skip Path

If user pairs outside app or wants to continue without in-app PIN submission:

1. User can click skip in pairing modal
2. App marks pairing context complete for checkpointing
3. Orchestration transitions to `Ready`
4. Server checkpoint records `PairingCompleted`

---

## Step Completion Markers

The app tracks completed steps to enable resumption:

| Step | Marker |
|------|--------|
| SSH Key Generated | `SshKeyReady` |
| SSH Key Uploaded | `SshKeyUploadedToVast` |
| Instance Created | `InstanceCreated` |
| Instance Ready | `InstanceReady` |
| SSH Connected | `SshConnected` |
| NVIDIA Headless Configured | `NvidiaHeadlessConfigured` |
| Sunshine Configured | `SunshineConfigured` |
| Low-Latency Audio Configured | `LowLatencyAudioConfigured` |
| WireGuard Configured | `WireguardConfigured` |
| Moonlight Configured | `MoonlightConfigured` |
| Awaiting Pair PIN | `AwaitingPairPin` |
| Post NVIDIA Reboot Completed | `PostNvidiaRebootCompleted` |
| Pairing Completed | `PairingCompleted` |

---

## Resume / Recovery Behavior

Provisioning is resumable per instance via persisted `steps` markers.

When resumed for an existing instance, completed steps are skipped and remaining steps continue in order. This avoids re-running expensive operations (e.g., GPU setup, Sunshine install) if already successful.

Key recovery rules:
1. WireGuard endpoint is refreshed from Vast before local tunnel setup
2. Missing UDP mapping fails fast
3. Pairing state can be resumed or manually skipped
4. `lastError` is persisted for post-mortem diagnostics

---

## Port Reference

| Service | Port | Protocol |
|---------|------|----------|
| SSH | 22 | TCP |
| Sunshine HTTPS | 47984 | TCP |
| Sunshine HTTP | 47989 | TCP |
| Sunshine Web UI | 47990 | TCP |
| Sunshine RTSP | 48010 | TCP |
| WireGuard | 51820 | UDP (mapped) |

---

## Troubleshooting

### SSH Connection Refused
- Instance may not be ready
- Check Vast.ai console for instance status
- Wait for `ssh_ready` or `running` status

### Sunshine Pairing Fails
1. Ensure Web UI credentials are created via the web UI at `https://<wireguard-ip>:47990`
2. Check `origin_pin_allowed = all` in config
3. Enter PIN within 60 seconds
4. Verify Moonlight can reach Sunshine: `curl -k https://10.77.0.1:47990`

### Xorg/TwinView Not Working
1. Check Xorg log: `tail /var/log/Xorg.0.log`
2. Verify GPU output name: `xrandr --listproviders`
3. Use correct output (DP-0 for datacenter GPUs, not HDMI-0)

### No Audio in Stream
1. Check PipeWire status: `systemctl --user status pipewire`
2. Verify audio devices: `pactl list sinks short`
3. Check Sunshine audio config in Web UI

### WireGuard Connection Fails
1. Verify server is listening: `sudo wg show`
2. Check firewall rules: `sudo iptables -L -n`
3. Use correct mapped port from Vast API
4. Ensure `AllowedIPs = 10.77.0.1/32` (not full tunnel)
