# macOS WireGuard runtime

This directory contains the native macOS pieces for Noland's managed WireGuard tunnel.

## Targets

- `NolandTunnelBridge`
  App-side Swift bridge invoked by Rust. It owns create/update/start/stop/status for
  `NETunnelProviderManager`.
- `NolandPacketTunnel`
  Packet Tunnel Provider extension. This is where WireGuardKit must run.
- `Shared`
  Codable payloads used by the bridge and the provider.

## Build bridge

From `src-tauri/platform/macos`:

```bash
swift build -c release
```

Or use:

```bash
./build_bridge.sh
```

## Generate Xcode project

The repo includes an `xcodegen` spec for the bridge and packet tunnel targets.

```bash
brew install xcodegen
./generate_xcode_project.sh
```

This generates `NolandMacOSWireGuard.xcodeproj` from `project.yml`.

## Start over cleanly

If you need to restart the Xcode setup from scratch:

```bash
./reset_xcode_setup.sh
./generate_xcode_project.sh
open NolandMacOSWireGuard.xcodeproj
```

Then re-add the vendored `wireguard-apple` local package and reapply signing/capabilities.

The Rust app will look for the bridge binary in:

- `NOLAND_MACOS_BRIDGE_PATH`
- `src-tauri/platform/macos/.build/release/NolandTunnelBridge`
- app bundle resource locations when packaged

## Required Apple setup

1. Enable `Network Extension` capability for the main app and extension.
2. Create and assign an App Group for the main app and the packet tunnel extension.
3. Create a Packet Tunnel Provider extension target in Xcode and use the source in `NolandPacketTunnel/`.
4. Add WireGuardKit to the extension target.
5. Sign the main app, bridge, and extension with the same team.
6. Link `WireGuardKit` into the `NolandPacketTunnel` target and replace the placeholder
   implementation in `WireGuardRuntimeAdapter.swift`.

## Vendored WireGuardKit package

Use the repo-local vendored package here when adding the Swift package in Xcode:

```txt
src-tauri/platform/macos/vendor/wireguard-apple
```

Do not use a temporary clone path. The vendored copy already includes the manifest compatibility
patch needed for current Xcode/SwiftPM to parse it.

## Runtime contract

Rust sends a serialized `TunnelBridgeRequest` over stdin to the bridge.
The bridge returns `TunnelStatusPayload` JSON on stdout.

The bridge target does not need the App Groups capability, but it does need the Network Extension entitlement so it can create and control the tunnel manager.

Commands:

- `status`
- `start`
- `stop`

## Readiness rule

The Rust app should only treat macOS as ready when:

1. the bridge reports the provider running,
2. the route is active, and
3. `10.77.0.1:47990` is reachable.

## Important note

The provider source here is WireGuardKit-ready scaffolding, not a finished WireGuardKit runtime.
The remaining external work is:

1. generate/open the Xcode project,
2. assign your team and entitlements,
3. add the real `WireGuardKit` dependency to `NolandPacketTunnel`,
4. replace the placeholder adapter in `WireGuardRuntimeAdapter.swift`, and
5. package the signed bridge and extension with the Tauri app.

## Package bridge into the Tauri app bundle

After building the Tauri app and the bridge:

```bash
./package_native_assets.sh
```

This copies the compiled `NolandTunnelBridge` into the macOS app bundle resources so the Rust
driver can find it without `NOLAND_MACOS_BRIDGE_PATH`.
