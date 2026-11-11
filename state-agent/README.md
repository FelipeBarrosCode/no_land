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

## Restore publication pipeline

Linux agents overlap destination-side assembly with a persistent pool that verifies
and flushes the previous completed-file window. A hand-off is triggered by 128 files,
32 MiB, or a 100 ms idle timer. Admission covers both windows (queued, verifying,
and ready files): at most 256 completed files / 64 MiB, with one oversized file
admitted alone. These are on-disk partials and metadata, not retained file buffers;
the existing chunk-channel and read-back buffer limits still apply.

Publication waits for the active verification window to finish. It then renames
files and syncs each distinct parent directory once before reporting publication.
The next verification window starts after that barrier to avoid concurrent fsyncs
serializing the rename batch. Every file still receives its full-file hash/size
check and `sync_all`; write-ahead intents and rollback ordering are preserved.
All workers are joined before cancellation, failure, or transaction rollback returns.

`PipelineOptions::overlap_file_syncs` enables the overlap on Linux. Non-Linux hosts
default to staged writes with the same persistent workers: local APFS measurements
showed concurrent fsyncs slowing small writes. Actual Linux/cloud throughput still
needs measurement on the target filesystem; this is not a measured cloud speedup.

Progress schema version 2 retains the existing fields and adds `verified_bytes`
(newly reconstructed files that passed verification and sync) and `io` counters.
`readback_bytes` counts only extra disk reads; ordered files are hashed during
assembly. `io` separates write, inline-hash, verification, file-sync, rename,
directory-sync, batched file-intent journal, destination preparation, and publication
wait time in microseconds, plus sync/batch counts and peak admission. Worker timings
are cumulative across threads and overlap assembly, so their sum is not wall time.
The I/O counters are retained in completed operation details.

Targeted checks, including cancellation, late corruption, idle downloads, partial
replacement, empty/oversized files, and crash recovery:

```bash
cargo test -p noland-restore -p noland-state-agent --lib
cargo test -p noland-restore --lib pipeline::tests::large_small_file_publication_workload -- --ignored --exact --nocapture
```

The opt-in workload exercises the overlapped path with 186,664 synthetic files,
checks every published file, and prints `RESTORE_PUBLICATION` timings/counters.
Its data and storage backend are synthetic; use the reported I/O breakdown for a
later native Shared Storage restore comparison on the cloud PC.

## Constraints that are not negotiable

- Backup uses `rclone copy` / `copyto`. Never `rclone sync`.
- A bundle is invisible until `COMMITTED` is written last.
- Reads prove use, not ownership.
- Restore never writes unverified data into live app paths.
- Automatic instance deletion waits for the seal protocol.
