# Managed tunnel reliability

The desktop provisions remote WireGuard configuration; `noland-net-helper`
owns the privileged local adapter and embedded GotaTun engine. Fresh setup,
existing-instance setup, and reconnect use the same local lifecycle functions
in `src-tauri/src/services/wireguard.rs`.

## Shared contract

`network-contracts/src/managed_tunnel.rs` is the source of truth for runtime
discovery, status and ownership records, launch authorization, status freshness,
atomic publication, and bounded platform commands. Streaming discovery uses the
same paths and freshness policy. Unix locks live inside the short runtime
directory, not in generic `/tmp/*.lock` files.

## Failure handling

- A launch authorization binds the helper to a launch ID, config path, and
  startup deadline. The helper checks it before ownership and after adapter
  initialization. Revoking or replacing it also requests a running helper's
  shutdown. Late elevation approval cannot authorize an expired launch.
- Readiness requires an IPC exchange. Startup and first-handshake failures revoke
  authorization and attempt cleanup before releasing the lifecycle lock.
- Stop uses revocation, `stop.request`, and the generation-bound Stop RPC.
  Unresponsive owners escalate through pkexec, AppleScript, or Windows UAC.
  Cleanup elevation has its own expiring authorization so a delayed cleanup
  prompt cannot operate on a subsequent launch.
- Missing status is not proof of exit: ownership locks and scoped legacy owner
  records are checked too. New macOS/Windows records include process creation
  identity to avoid confusing a reused PID with the original owner.
- Endpoint and MTU changes are serialized against lifecycle changes. The helper
  persists the config, advances its fingerprint, and publishes status before
  acknowledging success. Persistence failure attempts rollback; failed rollback
  stops the inconsistent tunnel. Stale fingerprints cannot overwrite newer
  configuration.
- Verification is read-only on all platforms. It checks fresh status, process
  ownership, exact config, live IPC, and a recent handshake. Provisioning callers
  explicitly choose when to reconnect; health polling cannot trigger elevation.
- Instance-specific teardown does not stop another instance's known owner.
  Failed teardown preserves configuration needed for recovery.
- Windows installer hooks confirm helper exit before replacing/removing files;
  they abort rather than assume a fixed delay released Wintun.

## Validation and boundaries

Unit tests cover generation expiry/revocation, runtime isolation and legacy
discovery, atomic replacement, config revision changes, stale writers, command
deadlines, mismatched responses, and inactive-instance teardown.

Privileged adapter setup, UAC/AppleScript/pkexec cancellation, sleep/resume,
forced cleanup, and installer hooks still require native packaged-app testing on
each operating system. Cross-compiling shared Rust code is not a substitute.

The existing persistent-helper policy after normal desktop exit is unchanged.
This change does not install a system service or add automatic background
restart/elevation loops. An app-lifetime lease or supervised service remains a
separate lifecycle-policy decision.
