# Moonlight Settings Manager

## GUI To QSettings Keys

| GUI Label | QSettings Key |
| --- | --- |
| Resolution Width | `width` |
| Resolution Height | `height` |
| FPS | `fps` |
| Bitrate | `bitrate` |
| Unlock Bitrate | `unlockbitrate` |
| Auto Adjust Bitrate | `autoadjustbitrate` |
| V-Sync | `vsync` |
| Frame Pacing | `framepacing` |
| Host Audio | `hostaudio` |
| Multi Controller | `multicontroller` |
| Audio Config | `audiocfg` |
| Video Codec | `videocfg` |
| Video Decoder | `videodec` |
| HDR | `hdr` |
| YUV 4:4:4 | `yuv444` |
| Window Mode | `windowmode` |
| mDNS | `mdns` |
| Quit App After | `quitAppAfter` |
| Mouse Acceleration | `mouseacceleration` |
| Absolute Touch Mode | `abstouchmode` |
| Connection Warnings | `connwarnings` |
| Rich Presence | `richpresence` |
| Gamepad Mouse | `gamepadmouse` |
| Detect Network Blocking | `detectnetblocking` |
| Show Performance Overlay | `showperfoverlay` |
| Swap Mouse Buttons | `swapmousebuttons` |
| Mute On Focus Loss | `muteonfocusloss` |
| Background Gamepad | `backgroundgamepad` |
| Reverse Scroll | `reversescroll` |
| Swap Face Buttons | `swapfacebuttons` |
| Capture System Keys | `capturesyskeys` |
| Keep Awake | `keepawake` |
| Language | `language` |
| UI Display Mode | `uidisplaymode` |
| Default Version | `defaultver` |

## Dynamic Settings

| Key | Strategy |
| --- | --- |
| `width` | Detect display resolution, clamp to sane defaults unless `--native` is used |
| `height` | Detect display resolution, clamp to sane defaults unless `--native` is used |
| `fps` | Detect display refresh rate and normalize to stable values |
| `bitrate` | Compute from resolution, FPS, and detected network type |
| `videocfg` | Prefer HEVC only when support is detected, otherwise use safe fallback/preserve |
| `videodec` | Preserve when enum mapping is uncertain, otherwise keep safe default |
| `hdr` | Default disabled unless end-to-end HDR support is known |
| `yuv444` | Default disabled unless high-bandwidth desktop use is clearly supported |
| `windowmode` | Preserve existing value when enum mapping is uncertain |
| `uidisplaymode` | Preserve existing value when enum mapping is uncertain |
| `audiocfg` | Preserve existing enum when uncertain, otherwise keep stereo-safe behavior |

## Static Defaults

| Key | Default |
| --- | --- |
| `vsync` | `true` |
| `framepacing` | `true` |
| `autoadjustbitrate` | `false` |
| `unlockbitrate` | `false` |
| `connwarnings` | `true` |
| `detectnetblocking` | `true` |
| `showperfoverlay` | `true` |
| `keepawake` | `true` |
| `mdns` | `true` |
| `gameopts` | `false` |
| `quitAppAfter` | `false` |
| `hostaudio` | `false` |
| `muteonfocusloss` | `false` |
| `mouseacceleration` | `false` |
| `abstouchmode` | `false` |
| `swapmousebuttons` | `false` |
| `reversescroll` | `false` |
| `swapfacebuttons` | `false` |
| `gamepadmouse` | `true` |
| `backgroundgamepad` | `false` |
| `multicontroller` | `true` |
| `capturesyskeys` | `false` |
| `language` | `auto` |

## Exact Commands

### macOS

```bash
cargo run --bin moonlight_config -- --dry-run
cargo run --bin moonlight_config -- --apply
```

### Linux

```bash
cargo run --bin moonlight_config -- --dry-run
cargo run --bin moonlight_config -- --apply --network lan
```

### Windows

```powershell
cargo run --bin moonlight_config -- --dry-run
cargo run --bin moonlight_config -- --apply --force-close
```

The CLI defaults to dry-run. Re-run with `--apply` to write changes. After writing it prints:

`Done. Reopen Moonlight and verify the GUI.`
