# Bounded Shared Storage restore

Desktop, Android, and iOS provision the cloud-side state agent from their own
repository. Keep the restore, pack decoder, storage stream API, and progress
handoff changes together when porting between them. No client application needs
to run to execute the source checks.

## Data path

1. Verify the committed manifest and encrypted pack index as before.
2. Preflight confined destinations and durably capture existing files for rollback.
3. Record creation/partial-file intents in checksummed, synced batches.
4. Stream ordered ciphertext using `rclone cat`, with bounded concurrent packs.
5. Authenticate and decompress each complete record, check its content hash, and
   scatter the plaintext directly into destination-side partial files.
6. Hash ordered files during assembly. Out-of-order files receive one readback
   through a bounded verification pool. Check final size/hash and permissions,
   sync file contents, then atomically rename and sync affected parent directories.
7. Commit the filesystem decision and database associations using the existing
   rollback rules. Remove only this restore's scratch/rollback workspace.

Ciphertext and plaintext records are bounded in memory. OS pipes pause producers
when the consumer is busy. Backends without streaming support use a temporary
pack, reserving the format's hard maximum per active pack and removing it on
success, failure, or cancellation. The default scratch budget is 1 GiB, so that
fallback admits one maximum-sized pack at a time. The streaming path creates no
encrypted pack cache and no decrypted chunk collection on disk.

This does **not** mean zero overhead: destination partials contain the new file
data, rollback copies of changed existing files remain until commit, and metadata
is proportional to the manifest. Keep separately preserved original directories;
this change does not authorize deleting them to make space.

## Recovery and progress

Journal v2 retains the v1 JSON checkpoint plus an incremental intent log. Older
agents refuse v2 rather than silently ignoring its recovery data. New agents can
recover v1. Complete log records are checksummed; an incomplete trailing record
was never acknowledged and is ignored. Complete-record corruption fails closed
and retains recovery artifacts. Checkpoints replace the JSON durably **before**
truncating the log. Per-file completion no longer rewrites the whole journal.

Within an operation, retries may replay authenticated records but cannot write a
chunk twice. Following a process crash, existing transaction rollback semantics
still apply; the original repository encryption key remains required.

File publication counts, assembled/published/readback bytes, and heartbeat times
are updated during reconstruction. Network byte counts stay in the transfer
journal and never overwrite the file counter. The clients display publication
bytes during APPLYING as well as isolated transfer phases. READY_TO_LAUNCH still
requires every prerequisite/critical entry; no assets or saves are silently
reclassified to report readiness early.

## Validation without app builds

- `node --test scripts/ci/restore-pipeline.test.mjs` checks integration wiring.
- `cargo fmt --manifest-path state-agent/Cargo.toml --all -- --check` parses and
  checks formatting without compiling the apps.
- `Restore engine regression tests` runs library unit tests, strict Clippy, and
  a state-agent integration check on CI. It does not build desktop/mobile apps
  or deploy/restart any remote agent.

The remote restore that is already running does not change until a separately
approved agent deployment. No speedup or completion ETA is promised without a
benchmark of the new engine on comparable storage.

## Bundle compatibility

The storage format and repository key do not change. Applications still have a
logical catalog entry backed by immutable manifest revisions and deduplicated
packs. This restore change neither rewrites bundles into one mutable archive nor
deletes historical revisions; cloud retention requires a separately settled
retention policy.
