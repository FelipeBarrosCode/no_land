# Microphone Passthrough

Native client microphone passthrough to provisioned cloud VMs via WireGuard + RTP/Opus.

## Architecture

```
Native client mic
  -> 48 kHz mono PCM
  -> Opus encode
  -> RTP packetization
  -> UDP over WireGuard
  -> VM cloud-mic-agent
  -> RTP receive + jitter buffer
  -> Opus decode to PCM
  -> PipeWire virtual microphone
  -> apps inside VM see "Cloud Mic"
```

## Data Flow

1. **Client** captures mic, resamples to 48 kHz mono, encodes Opus
2. **Client** packetizes into RTP (dynamic PT 111, SSRC per session)
3. **Client** sends UDP to VM WireGuard IP:34778
4. **VM agent** receives RTP, validates peer IP + token
5. **VM agent** jitter buffers, decodes Opus, writes PCM to virtual mic
6. **VM apps** see "Cloud Mic" as an audio input source

## Backend Endpoints (Tauri Commands)

| Command | Args | Returns |
|---------|------|---------|
| `get_instance_mic_config` | `instanceId: number` | `InstanceMicConfig` |
| `update_instance_mic_settings` | `instanceId, payload: MicSettingsUpdate` | `InstanceMicConfig` |
| `enable_instance_mic` | `instanceId, qualityProfile?` | `MicSessionResponse` |
| `disable_instance_mic` | `instanceId` | `void` |
| `reconnect_instance_mic` | `instanceId` | `MicSessionResponse` |
| `recreate_instance_mic_device` | `instanceId` | `void` |
| `get_instance_mic_status` | `instanceId` | `InstanceMicRuntimeStatus` |

## VM Agent API

The cloud-mic-agent runs on the VM as a systemd user service.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Returns "ok" |
| `/status` | GET | Runtime status JSON |
| `/metrics` | GET | Metrics snapshot JSON |
| `/session/start` | POST | Start mic session |
| `/session/stop` | POST | Stop mic session |
| `/device/create` | POST | Create Cloud Mic device |
| `/device/recreate` | POST | Recreate Cloud Mic device |
| `/device/set-default` | POST | Set Cloud Mic as default source |

### Session Start Request

```json
{
  "sessionId": "uuid",
  "sessionToken": "base64",
  "expectedPeerIp": "10.77.0.2",
  "ssrc": 1234567890,
  "rtpPort": 34778,
  "codec": "opus",
  "sampleRate": 48000,
  "channels": 1,
  "frameMs": 20,
  "bitrateKbps": 32
}
```

## Quality Profiles

| Profile | Bitrate | Frame | Jitter Min | Jitter Target | Jitter Max |
|---------|---------|-------|------------|---------------|------------|
| Standard | 32 kbps | 20 ms | 20 ms | 30 ms | 60 ms |
| Low Latency | 48 kbps | 10 ms | 10 ms | 15 ms | 40 ms |
| High Quality | 64 kbps | 20 ms | 30 ms | 40 ms | 80 ms |

## Default Ports

- VM Agent HTTP API: `34779` (binds to WireGuard IP)
- VM Agent RTP UDP: `34778` (binds to all interfaces, filtered by peer IP)

## Security Model

- VM agent binds HTTP to WireGuard IP only (never 0.0.0.0)
- UDP RTP accepts packets only from `expectedPeerIp`
- Session tokens are 32-byte random values, base64-encoded
- No audio payload logging
- No audio storage
- Tokens are session-scoped and discarded on stop

## Provisioning Steps

1. Build the VM agent:
   ```bash
   cd vm-cloud-mic-agent
   cargo build --release
   ```

2. Copy binary to VM:
   ```bash
   scp target/release/cloud-mic-agent user@vm:/tmp/
   ```

3. Run install script on VM:
   ```bash
   sudo bash /tmp/install_cloud_mic_agent.sh user
   ```

4. Verify:
   ```bash
   systemctl --user status cloud-mic-agent.service
   curl http://<wg-ip>:34779/health
   ```

## Debugging Commands

```bash
# Check agent status
systemctl --user status cloud-mic-agent.service

# View agent logs
journalctl --user -u cloud-mic-agent.service -f

# Check PipeWire sources
pactl list sources short

# Check if Cloud Mic exists
pactl list sources | grep -A5 "Cloud Mic"

# Test RTP reception (on VM)
nc -u -l 34778 | xxd

# Monitor WireGuard
g show wg0
```

## Known Limitations (MVP)

- VM agent does not yet decode Opus or write actual PCM audio
- VM agent uses null-sink fallback instead of native PipeWire Audio/Source node
- Native client capture is not yet implemented (test sender only)
- No PLC (packet loss concealment) in jitter buffer
- No adaptive bitrate
- No echo cancellation
- No noise suppression

## Next Steps

1. Implement Opus decode in VM agent and wire to audio output
2. Implement native client capture + Opus encode
3. Create native PipeWire Audio/Source node (replace null-sink)
4. Add PLC to jitter buffer
5. Add Opus encode/decode roundtrip tests
6. Implement client test sender with synthetic sine wave
7. Add input level metering to frontend
