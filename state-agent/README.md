# Noland state agent

Always-on application-state tracking for disposable Linux gaming instances.

The remote instance is ephemeral. Learned application ownership, classified state, encrypted packs, and catalog commits live in the user's Shared Storage (`Noland Shared Storage` on Google Drive first).

This is not a whole-VM backup product. Tracking is always on; apps are not launched through ReproZip.

## Workspace

```
crates/
  noland-state-core        domain types and backup decision function
  noland-state-db          ephemeral SQLite WAL database
  noland-observer          process/fs events, coalescing, cgroups
  noland-discovery         .desktop, Steam, Proton, Wine, Bottles
  noland-attribution       sessions, evidence, installer transactions
  noland-classifier        persistence class + semantic role
  noland-baseline          image baseline / package ownership
  noland-snapshot          Btrfs snapshot or copy fallback
  noland-cas               BLAKE3 + FastCDC 1/4/8 MiB
  noland-pack              immutable packfiles (512 MiB / 1 GiB)
  noland-crypto            XChaCha20-Poly1305 + HKDF
  noland-storage           provider trait, rclone copy, commit protocol
  noland-restore           staged restore + rollback
  noland-rpc               Unix socket JSON-RPC
  noland-state-agent       daemon
  noland-testkit           fixtures and integration tests
```

## Runtime paths

```
/var/lib/noland/state/     SQLite, staging, packs, restore, checkpoints
/run/noland/state-agent.sock
/run/noland/storage/<operation_id>/   ephemeral provider auth only
```

The long-lived Google Drive refresh token stays on the desktop client. The VM receives an operation-scoped access token and must delete it when the operation ends.

## Build / test

```bash
cd state-agent
cargo test --workspace
```

## Constraints that are not negotiable

- Backup uses `rclone copy` / `copyto`. Never `rclone sync`.
- A bundle is invisible until `COMMITTED` is written last.
- Reads prove use, not ownership.
- Restore never writes unverified data into live app paths.
- Automatic instance deletion waits for the seal protocol.
