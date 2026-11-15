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

## Capture, transfer, and incremental indexing

New chunks share sealed, lease-owned capture containers (32 MiB maximum)
instead of creating and flushing one cache file per chunk. Hashing and pack
workers reuse buffers; packs retain their existing encryption, compression format,
and Zstd level 1. Containers are immutable and durable before authenticated retry
checkpoints reference them; legacy flat-file capture checkpoints remain readable.

Uploads use a count/byte-bounded rolling queue independently of the compression
worker count, with one shared transfer budget and one operation-scoped remote pack
inventory. Compact packs are reserved for small saves/secrets; installation assets
use normal packs. Restore prioritizes file/chunk locality and keeps destination
handles bounded. Completed restores seed verified destination identities and,
when the entire bundle was restored, its repository/key-bound parent baseline.
Concurrent filesystem changes retain their dirty state.

Reconciliation collapses equivalent overlapping roots, prunes excluded trees,
and batches classification and database work. Namespace-local runtime paths are
translated using the originating process's mount information; uncertain mappings
require reconciliation. Assets inside private runtime homes are application
content, rather than all being treated as personal state.

New client backups send only the selected applications and use complete capture
with automatic resource budgeting. The performance selector and shader/cache
toggle are removed. Application-owned caches are included; Steam capture also
discovers the selected app's shadercache, compatdata, userdata and known state
roots. Other games and shared launcher/runtime infrastructure remain outside that
app's scope. `capture_scope_version: 1` prevents an incomplete legacy parent from
being reused as an unchanged complete backup. Historical performance/cache fields
remain readable for resumable operations. Fresh requests normalize removed legacy
options to complete/automatic/include; replay retains the original identity.

Periodic learned-state checkpoints are change-triggered, compressed, and atomically
published. Retention applies only to newly owned, unreferenced periodic artifacts;
explicit seal/backup checkpoints and legacy files are preserved. Client monitoring
reconnects to the same operation after transport failures and distinguishes a lost
connection from an authoritative failed or cancelled operation.
Schema version 4 persists a semantic generation so unchanged periodic ticks skip
loading the learned-state rows. Observation timestamps do not advance this generation.

`StartBackup`, `StartRestore`, and `StartSeal` accept a `client_operation_id` UUID. Repeating
that ID with the same repository, key, and selection returns its existing status,
including terminal status, without running the operation again. Clients retain
the ID before submission, so an interrupted initial response is also resumable.
`ListRecentOperations` and `GetOperationStatus` accept `projection: "matching"`
for identity/status-only reattachment without per-operation transfer aggregates.
Status calls also accept `projection: "progress"` for correlated lifecycle state,
progress and pack counters without serializing full operation results or retry
documents on every poll. Omitting the projection retains the complete response.
Seal admission is not seal completion: lifecycle callers require terminal successful
completion and must honor cancellation even if the cancellation RPC is interrupted.

Agent/client API version 22 includes these capabilities, full cloud deletion and
single-current-bundle convergence. End-to-end measurement
guidance and the pre-change cloud baseline are in
[`docs/shared-storage-io-validation.md`](../docs/shared-storage-io-validation.md).

Seal uses the transfer lane because it performs backups itself; read-only control
requests remain responsive without allowing simultaneous catalog writers in one
agent. All-app backups persist their target list and child operation IDs before
starting work. Retries reuse completed children and immutable captured revisions,
and leave later local mutations dirty for a subsequent backup.

Schema v5 adds atomically maintained credential-free operation projections.
Progress reads do not fetch large result JSON; legacy matching rows upgrade lazily
outside the database lock. Remote chunk hints use indexed lookups and a bounded
4,096-entry operation cache; index ingestion compares batches of 512 entries
instead of materializing unrelated repository history. An empty remote index
does not incur per-chunk SQL queries. No-op backups reuse the authenticated parent
manifest, and network accounting includes parent loading and failed attempts.

V3 capture publishes authenticated durable segment receipts before file EOF.
One operation-wide scheduler feeds shared pack workers and independent upload
slots; neither a large file nor a slow upload creates a batch barrier. Atomic
snapshots and per-file CoW are preferred. Without CoW, one descriptor read feeds
full-file hashing, FastCDC and durable capture together. Recovery replays a local
prefix and verifies the frozen source generation before capturing its suffix.
Pack plans and sealed ciphertext remain fixed across retries. Only complete,
verified files enter the final manifest. V1/V2 checkpoints remain readable.

Preparation uses at most four readers and eight packers, sharing a CPU/memory/IO
budget; foreground uploads start with at most eight provider/resource-limited
slots. Live games and unknown sessions retain backup contention protection;
proven Steam launcher processes do not trigger it. No runtime CPU/GPU quotas are
applied. Pack-size benchmark overrides are `NOLAND_STREAM_PACK_MIB=128|256|512`;
the default target remains 512 MiB, with an initial pack of up to 64 MiB and a
one-second partial-pack flush. Memory queues and retry disk retention are distinct:
capture and ciphertext remain on disk until the sealed operation checkpoint is
durable. Disk admission and incremental write reservations cover this retention.
Retries recheck current preparation budgets, and transient CoW views remain in the
operation-owned capture tree for crash cleanup. Final pipeline diagnostics are
retained in completed operation details after the live progress record is cleared.

Agent deployment checks source content fingerprints as well as API/prebuilt
compatibility. Compatible active work is reattached rather than restarted to
refresh a build. Android embedded sources are cached by archive hash, not API
version, and extracted into an isolated directory before atomic publication.

## Full deletion and one current bundle (API 22)

Cloud deletion now removes the actual encrypted packs, all confirmed bundle
objects and their empty bundle directories, not just metadata. The preview scans
authenticated **committed bundle objects**, including commits absent from a stale
catalog, and computes the transitive pack-sharing dependency closure. Every
affected backup is listed. The UI requires an additional acknowledgement when
dependent backups are included. A changed inventory aborts before destructive
effects; consent is never widened after confirmation. Drive uses exact-object
`deletefile --drive-use-trash=false`, not trash, `sync`, or a repository purge.
Backblaze B2 and OneDrive use their provider-scoped hard-delete options. Preview
and execution preflight those flags without remote IO; an older rclone must be
updated before tombstones or payload removal can begin.

A durable encrypted deletion plan precedes all tombstones/unlinks. Partial or
cancelled deletion is not reported as complete: select its pending-deletion entry
to preview and resume the same frozen plan after a restart. Small authenticated
receipts remain outside bundle directories to prevent stale catalogs and cached
chunk hints from resurrecting deleted data. Provider retention/versioning policies
can still affect when provider-side quota or old versions disappear.

Complete backups converge to one current bundle per app. The new manifest, packs
and catalog must be durable before older ancestor generations are retired.
Only their unreferenced packs are reclaimed automatically; packs used by the
current bundle or another app belong to those surviving bundles. No local game
files are removed. Failed replacement keeps the last good bundle; a newer cloud
head or divergent history fails closed instead of silently discarding another
PC's saves. The restore selector exposes only the current complete head.
Retained generations needing explicit resolution are exposed separately as
delete-only entries, never as alternate restore choices.
Final seal validation/publication also holds repository admission and rechecks
each referenced committed bundle. A backup deleted between child transfers
cannot authorize automatic instance destruction through a stale clean-app entry.

All PCs using the repository must run updated agents. Managed backup, restore and
maintenance now use authenticated, repository-wide admission claims. Claims do not
expire while workers may still be alive. Cancelled/crashed claims are recovered
only by their owner after local execution has stopped; foreign abandoned claims
require verified-owner recovery, not a guessed TTL. An unfinished destructive plan
blocks other managed transfers until that exact plan is resumed. External/legacy
clients that ignore this protocol must not access the repository during deletion.
Each execution has a distinct claim even when retrying the same operation. An
agent-root OS lock proves prior local execution has stopped before recovering its
claims. Admission rechecks pending work after winning, closing the finish/failure
race between two PCs. Rclone uses structured JSON inventory and rejects truncated,
ambiguous or duplicate object listings; known catalog commits cross-check inventory
completeness. The protocol requires complete read-after-write object visibility:
stale caching layers and eventually consistent mirrors are not supported for
destructive maintenance. Local tests do not certify a cloud backend's consistency.

The API 21 local-deletion safety contract below remains unchanged.

## Explicit storage deletion (introduced in API 21)

`PrepareDeletion` accepts `target: "remote_pc" | "cloud"` and explicit
`selected_paths` (`/apps/<app-id>` or `/catalog/<app-id>/<bundle-uuid>`).
It returns a ten-minute preview with an opaque `planId`, names, item count,
logical bytes, exclusions and sample paths. `StartDeletion` requires that
`plan_id`, the same target and `confirmed: true`; cloud calls also require a
fresh ephemeral session and the same repository/vault binding. The plan ID is
the operation ID, so a lost reply can be replayed without deleting new files.
Operation status, progress and cancellation use the existing monitor RPCs.

Local deletion checks the full frozen set before mutation, then rechecks each
leaf's parent, identity and shared ownership. It is limited to exclusively owned
application files beneath the configured home. It excludes Noland/system state,
credentials, `.nup` packages and pre-sync copies. Wildcards, unknown ownership,
active selected apps and overlapping storage operations are rejected or excluded.
No recursive path purge is used: a leaf is moved into a private same-filesystem
quarantine and checked again before unlinking. A raced/crashed quarantine is
retained beside its source for recovery; it is never swept automatically.
Empty directories and launcher registrations may remain.

API 21's metadata-only cloud deletion is superseded by the full-deletion protocol
above. Small deletion receipts still prevent stale catalog resurrection. Manual
deletion remains separate from upload/download; successful complete backups also
retire superseded cloud generations under the requested one-bundle policy.

## Constraints that are not negotiable

- Backup uses `rclone copy` / `copyto`. Never `rclone sync`.
- A bundle is invisible until `COMMITTED` is written last.
- Reads prove use, not ownership.
- Restore never writes unverified data into live app paths.
- Automatic instance deletion waits for the seal protocol.
- Explicit manual instance destruction skips Shared Storage backup/seal even when
  storage is enabled. Its confirmation warns that unsynced data will be lost;
  users must back up first if needed. Pause and automatic-stop sealing are unchanged.
