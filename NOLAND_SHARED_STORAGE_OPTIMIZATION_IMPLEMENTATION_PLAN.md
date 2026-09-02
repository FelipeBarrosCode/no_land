# Noland Shared Storage Optimization Implementation Plan
## Integrating Performance Optimizations into the Existing fanotify → ObserverHub → AppSession → Shared Storage Architecture

**Project:** Noland / no_land  
**Purpose:** implementation plan for optimizing the current Shared Storage design without weakening correctness  
**Primary target:** Linux disposable gaming instances  
**Observer backend:** fanotify v1  
**Existing ingestion seam:** `ObserverHub::inject_fs()`  
**Application correlation:** `PID/cgroup/process history → AppSession`  
**Storage model:** immutable/versioned content-addressed application state  
**Cloud provider:** Google Drive first, rclone-backed transport initially  
**Primary implementation language:** Rust  
**Status:** implementation-ready optimization plan

---

# 0. Objective

The current architecture is already directionally correct:

```text
Linux filesystem/process activity
    ↓
fanotify / process observer
    ↓
noland-fs-observer
    ↓
ObserverHub::inject_fs()
    ↓
PID / process / cgroup → AppSession
    ↓
USES vs MUTATES
    ↓
path attribution + classification
    ↓
persistent state / dependencies / base image / cache
    ↓
reconciliation
    ↓
snapshot
    ↓
hash / chunk / pack
    ↓
encrypted Shared Storage commit
    ↓
restore
```

The optimization work must **not** replace this architecture.

The goal is to make it substantially faster by changing the system from:

```text
broad scan
→ broad hashing
→ many tiny remote calls
→ serial object handling
→ full restore before launch
```

into:

```text
mutation-driven delta
→ persistent local index
→ bounded incremental reconcile
→ streaming hash/chunk
→ packed immutable objects
→ bulk/parallel transfer
→ resumable commit
→ critical-state-first restore
```

The single largest architectural optimization is:

> **Use the filesystem observer as the source of backup scope acceleration, not merely as a correctness/attribution mechanism.**

Every trustworthy mutation flowing through `ObserverHub::inject_fs()` should update a durable per-app mutation journal and incremental file index.

This lets a normal backup start from:

```text
small set of known changed paths
```

instead of:

```text
walk all known roots every time
```

---

# 1. Non-Negotiable Correctness Rules

Optimization must not violate these guarantees:

1. Do not lose user state.
2. Do not treat a read as ownership.
3. Do not back up unchanged base-image files.
4. Do not use `rclone sync` for immutable backup history.
5. Do not publish a manifest before all referenced immutable content exists.
6. `COMMITTED` remains the final visibility marker.
7. Restore remains staged and verified.
8. Save/database files are not merged blindly.
9. Observation gaps trigger reconciliation.
10. `FAN_Q_OVERFLOW` means the mutation journal may be incomplete.
11. Automatic instance deletion remains gated by the seal protocol.
12. Long-lived provider credentials remain off the disposable VM.
13. The main state agent does not gain `CAP_SYS_ADMIN` merely for observation.
14. The privileged fanotify helper performs observation only.
15. Content-addressed objects remain immutable.
16. Backup/restore optimizations must be resumable and idempotent.

---

# 2. Current Cost Model

The current logical pipeline is:

```text
DISCOVERING
RECONCILING
SNAPSHOTTING
HASHING
PACKING
UPLOADING
COMMITTING
CHECKPOINTING
```

Restore is roughly:

```text
FETCH MANIFEST
RESOLVE DEPENDENCIES
DOWNLOAD
VERIFY
MATERIALIZE
SNAPSHOT TARGET
APPLY
VALIDATE
```

Likely cost centers are:

## Backup

- broad reconciliation even when only a few files changed;
- repeated filesystem traversal;
- hashing unchanged files;
- `read_to_end()` style chunk preparation;
- per-object remote existence checks;
- repeated `ensure_root()` work;
- many tiny metadata writes;
- too many rclone process invocations;
- low transfer concurrency;
- caller waiting synchronously for a long operation;
- little reuse of prior chunk/index knowledge.

## Restore

- downloading all content before launch;
- downloading small objects individually;
- no local CAS reuse;
- no priority tiers;
- rewriting files that already match;
- redundant verification passes;
- restart means repeated work;
- limited parallel fetch/materialization.

---

# 3. Target Optimized Architecture

```text
                         ┌────────────────────────────┐
                         │ Process / AppSession Graph │
                         └──────────────┬─────────────┘
                                        │
Linux fanotify events                    │
        │                                │
        ▼                                ▼
noland-fs-observer                 SessionCorrelator
        │                                │
        └──────────────┬─────────────────┘
                       ▼
              ObserverHub::inject_fs()
                       │
                       ▼
              Classification Engine
                       │
          ┌────────────┼─────────────┐
          │            │             │
          ▼            ▼             ▼
   path association  dependency   mutation journal
      graph           graph          + dirty roots
                                          │
                                          ▼
                              Persistent Incremental Index
                                          │
                                          ▼
                              Incremental Backup Planner
                                          │
                              ┌───────────┴───────────┐
                              ▼                       ▼
                     unchanged fast-skip      changed candidates
                                                      │
                                                      ▼
                                              snapshot/staging
                                                      │
                                                      ▼
                                      streaming hash + FastCDC
                                                      │
                                                      ▼
                                       local CAS / small-file pack
                                                      │
                                                      ▼
                                         Remote Sync Planner
                                                      │
                                   ┌──────────────────┼──────────────┐
                                   ▼                  ▼              ▼
                             local CAS hit      remote-known      new objects
                                   │                  │              │
                                   └──────────────────┴──────┬───────┘
                                                             ▼
                                                parallel immutable upload
                                                             │
                                                             ▼
                                                compact encrypted manifest
                                                             │
                                                             ▼
                                                      COMMITTED
                                                             │
                                                             ▼
                                                   catalog/latest pointer
```

Restore target:

```text
manifest
  ↓
local CAS hit test
  ↓
critical tier
  ↓
parallel fetch
  ↓
stream verify
  ↓
staging
  ↓
ready-to-launch
  ↓
background lower-priority hydration
```

---

# 4. Optimization Principle: Do Less Work First

The preferred order of optimization is:

```text
1. know exactly what changed
2. skip unchanged paths
3. avoid remote round trips
4. combine tiny objects
5. overlap CPU/disk/network
6. resume completed work
7. restore only what is required before launch
8. specialize the backend only after generic path is efficient
```

Do **not** start by replacing rclone.

If the system still:

- rescans too much;
- hashes too much;
- creates too many objects;
- performs too many remote control-plane calls;

a custom transport alone will not solve the largest delays.

---

# 5. Upgrade A — Performance Instrumentation

Implement instrumentation before deeper structural optimization.

## 5.1 Per-operation timing

Persist:

```text
operation_id
operation_type

total_duration_ms

discovery_duration_ms
reconciliation_duration_ms
snapshot_duration_ms
planning_duration_ms
hashing_duration_ms
chunking_duration_ms
packing_duration_ms
upload_duration_ms
download_duration_ms
manifest_duration_ms
commit_duration_ms
checkpoint_duration_ms
restore_materialize_duration_ms
restore_apply_duration_ms
validation_duration_ms
```

## 5.2 Work counters

```text
num_candidate_paths
num_dirty_paths
num_dirty_roots
num_files_scanned
num_files_skipped_fast_identity
num_files_rehashed
num_files_uploaded
num_files_downloaded
num_files_reused_local

bytes_scanned
bytes_hashed
bytes_chunked
bytes_packed
bytes_uploaded
bytes_downloaded
bytes_reused_local

num_chunks_created
num_chunks_reused
num_small_files_packed

num_rclone_invocations
num_remote_stat_calls
num_remote_list_calls
num_remote_mkdir_calls
num_remote_upload_calls
num_remote_download_calls
num_manifest_writes

num_local_cas_hits
num_remote_index_hits
num_remote_unknown_objects
```

## 5.3 Observer-driven metrics

```text
fs_mutation_events
fs_read_events
fs_events_deduped
fs_events_dropped
fanotify_overflows
dirty_apps
dirty_paths
dirty_roots
apps_requiring_reconciliation
```

## 5.4 Expose metrics

Make them visible through:

```text
structured logs
GetOperationStatus
dev diagnostics UI
optional metrics endpoint
```

The optimization agent must compare before/after measurements.

---

# 6. Upgrade B — Background Operation Engine

Backup, restore, checkpoint, and seal should all use a common long-running operation engine.

## 6.1 API

```text
StartBackup
StartRestore
StartCheckpoint
StartSeal
```

should return immediately:

```json
{
  "operation_id": "...",
  "stage": "QUEUED"
}
```

Then use:

```text
GetOperationStatus
CancelOperation
RetryOperation
ListRecentOperations
```

## 6.2 OperationManager

```rust
pub struct OperationManager {
    queue: OperationQueue,
    running: DashMap<OperationId, OperationHandle>,
    db: Arc<StateDb>,
}
```

Conceptual jobs:

```rust
pub enum OperationKind {
    Backup,
    Restore,
    Checkpoint,
    Seal,
}
```

## 6.3 Persist stage state

Every stage transition is written to SQLite.

On restart:

```text
safe-to-resume operation → resume
unsafe partial apply → rollback/fail safely
immutable transfer stage → continue
```

This makes later resumability possible.

---

# 7. Upgrade C — Observer-Driven Mutation Journal

This is the most important integration with the current fanotify design.

The current observer pipeline should become:

```text
fanotify
  ↓
ObserverHub::inject_fs()
  ↓
AppSession correlation
  ↓
classification
  ↓
mutation journal update
  ↓
dirty subtree update
  ↓
backup planner
```

## 7.1 Journal only mutations

Reads do not make a path dirty.

Mutation-like evidence includes:

```text
CloseWrite
Create
Truncate
Rename
Delete
```

For fanotify v1, `FAN_CLOSE_WRITE` is the main live mutation event.

Rename/delete correctness still comes from reconciliation.

## 7.2 New tables

Add something equivalent to:

```sql
CREATE TABLE app_mutation_journal (
    app_id TEXT NOT NULL,
    path_id INTEGER,
    canonical_path TEXT NOT NULL,
    parent_path TEXT,
    mutation_kind TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    event_count INTEGER NOT NULL DEFAULT 1,
    requires_reconcile INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(app_id, canonical_path)
);

CREATE INDEX idx_mutation_app
ON app_mutation_journal(app_id, last_seen_at);

CREATE TABLE dirty_roots (
    app_id TEXT NOT NULL,
    canonical_root TEXT NOT NULL,
    reason TEXT NOT NULL,
    first_dirty_at INTEGER NOT NULL,
    last_dirty_at INTEGER NOT NULL,
    PRIMARY KEY(app_id, canonical_root)
);
```

If equivalent tables already exist, extend them rather than duplicate.

## 7.3 Journal rules

For a strong persistent mutation:

```text
mark app dirty
upsert changed path
upsert nearest known state root
```

For cache/temp:

```text
do not mark durable state dirty
```

For unknown mutation:

```text
mark path dirty candidate
mark root requires bounded reconciliation
```

For observer overflow:

```text
mark all affected active app roots requires_reconcile
```

## 7.4 Clear only after commit

Do not clear the journal after hashing.

Clear entries only after:

```text
bundle COMMITTED
+
catalog commit succeeds
```

If operation fails, dirty evidence remains.

---

# 8. Upgrade D — Persistent Incremental File Index

Create a durable local file-state index.

This lives in the ephemeral VM DB during the VM lifecycle and is exported in compact checkpoints.

Minimum fields:

```text
app_id
path
logical_root
relative_path
inode
device
size
mtime_ns
mode
file_type
quick_identity
content_id
file_hash
last_reconciled_generation
last_backup_generation
classification
semantic_role
association_confidence
dirty_hint
reconcile_hint
```

## 8.1 Fast identity

For a previously tracked file:

```text
same inode/device when meaningful
same size
same mtime_ns
same path
no mutation journal entry
```

allows:

```text
skip full hashing
reuse previous content_id
```

Do not rely on mtime/size after a known observer gap.

## 8.2 Trust levels

```text
TRUSTED_UNCHANGED
DIRTY
MAYBE_DIRTY
UNKNOWN
```

### Trusted unchanged

Requirements:

- no relevant observer gap;
- no mutation event;
- metadata identity still matches;
- parent/root not marked uncertain.

### Dirty

Known mutation.

### Maybe dirty

Root affected by overflow/restart/untracked rename.

### Unknown

New/unindexed path.

---

# 9. Upgrade E — Incremental Reconciliation

Replace:

```text
walk every app root every backup
```

with:

```text
start from journal
→ inspect dirty paths
→ inspect dirty parent directories
→ widen only if evidence requires it
```

## 9.1 Reconcile scope levels

```text
PATH
DIRECTORY
ROOT
FULL_APP
```

Use the narrowest valid level.

## 9.2 Widen conditions

Widen when:

```text
fanotify overflow
observer restart gap
directory structure changed unexpectedly
installer/update occurred
root metadata changed
path disappeared
unknown rename/delete suspected
classification confidence regressed
```

## 9.3 Backup behavior

Normal app state:

```text
few mutations
→ path/directories only
```

Uncertain app state:

```text
overflow/update
→ root/full-app reconcile
```

Correctness remains the backstop.

---

# 10. Upgrade F — Backup Planner

Add a pure, testable planner.

Input:

```text
AppSession/app_id
mutation journal
incremental file index
classification graph
previous bundle manifest
local CAS index
remote pack/object index
backup mode
```

Output:

```rust
pub struct BackupPlan {
    pub unchanged_files: Vec<FileRef>,
    pub rehash_files: Vec<FileRef>,
    pub deleted_paths: Vec<LogicalPath>,
    pub reconstructable_metadata: Vec<DependencyRef>,
    pub persistent_files: Vec<FileRef>,
    pub optional_app_content: Vec<FileRef>,
    pub required_shared_state: Vec<FileRef>,
    pub small_file_candidates: Vec<FileRef>,
}
```

The planner must be deterministic.

Do not let transfer code decide classification.

---

# 11. Upgrade G — Stream Hashing and FastCDC

The current design already uses:

```text
BLAKE3
FastCDC
1 MiB min
4 MiB average
8 MiB max
```

Optimize implementation so large files are never loaded with a full `read_to_end()`.

## 11.1 Streaming pipeline

```text
file reader
   ↓
FastCDC rolling buffer
   ↓
BLAKE3 chunk hash
   ↓
local CAS lookup
   ↓
chunk queue
   ↓
pack builder
   ↓
upload queue
```

CPU, disk, and network should overlap.

## 11.2 Bounded memory

Use limits:

```text
read window
chunk buffer
max inflight chunks
max inflight bytes
```

Example configuration:

```text
max_inflight_hash_bytes = 256 MiB
max_inflight_pack_bytes = 1 GiB
```

Tune later using metrics.

## 11.3 Do not rechunk trusted unchanged files

Reuse prior file/chunk mapping.

---

# 12. Upgrade H — Small-File Packing

The existing large immutable pack architecture should explicitly optimize tiny state files.

Tiny files often include:

```text
config
metadata
controller mappings
small saves
settings
profiles
launcher state
mod metadata
```

Uploading each separately is inefficient.

## 12.1 Threshold

Start with:

```text
<= 256 KiB
```

as the default small-file threshold.

Make configurable.

## 12.2 Bucket strategy

Pack by:

```text
app_id
+
logical directory bucket
+
backup generation
```

Do not create one global small-file pack.

Example:

```text
smallpacks/<app>/<generation>/<bucket>.pack
```

## 12.3 Manifest mapping

Store:

```text
logical path
pack_id
offset
length
BLAKE3
metadata
```

## 12.4 Update behavior

A changed tiny file should create a new immutable small-file pack for the affected bucket/generation.

Do not mutate a prior pack.

---

# 13. Upgrade I — Remote Sync Planner

Before any network operation, compute:

```text
required immutable content
→ already local
→ already known remote
→ unknown remote
→ upload set
```

## 13.1 Eliminate per-object `stat`

Do not do:

```text
for object:
  stat remote
  if missing:
    upload
```

Instead prefer:

```text
one prefix/index listing
+
local comparison
```

or:

```text
known remote pack index
```

## 13.2 Remote-known set

Maintain a compact local cache:

```text
pack_id
remote_key
last_confirmed_at
provider_generation/cache_epoch
```

Refresh:

```text
on provider reconnect
on explicit cache invalidation
when expected object is missing
periodically at low frequency
```

## 13.3 Optimistic immutable upload

When appropriate:

```text
attempt create/copy
already exists → success
```

but use provider-specific behavior only after confirming it is safe.

---

# 14. Upgrade J — Cache `ensure_root()`

Once:

```text
Noland Shared Storage
```

and required subdirectories are verified, avoid recreating/checking them every operation.

Cache:

```text
provider account identity
root folder id
storage schema version
verified_at
```

Invalidate on:

```text
account change
root deleted
provider reconnect
schema migration
explicit diagnostic refresh
```

---

# 15. Upgrade K — Reduce rclone Invocation Count

Short-term: keep rclone.

Optimize how it is called.

Do not spawn one command for every chunk/object.

Preferred:

```text
build local upload batch/staging set
→ one/few bulk rclone copy operations
```

or use rclone's RPC/remote-control interface if the current deployment can safely keep one rclone process alive.

Goal metric:

```text
num_rclone_invocations
```

should drop dramatically.

Preserve:

```text
copy semantics
```

not:

```text
sync semantics
```

---

# 16. Upgrade L — Parallel Pipeline

Use separate bounded worker pools.

```text
changed files
   ↓
stat/scan workers
   ↓
hash/chunk workers
   ↓
pack workers
   ↓
upload workers
```

## 16.1 Config

```rust
pub struct TransferTuning {
    pub stat_workers: usize,
    pub hash_workers: usize,
    pub pack_workers: usize,
    pub upload_workers: usize,
    pub download_workers: usize,
    pub materialize_workers: usize,
    pub max_inflight_bytes: u64,
}
```

## 16.2 Initial conservative defaults

Example only:

```text
stat_workers = 8
hash_workers = min(4, logical_cpu_count / 2)
pack_workers = 2
upload_workers = 4
download_workers = 4
materialize_workers = 4
```

Tune using actual machines/provider rate limits.

## 16.3 Gameplay-aware scheduling

When game session is active:

```text
lower hash/pack CPU priority
limit disk parallelism
limit upload bandwidth
```

When app exits or seal begins:

```text
allow more aggressive throughput
```

---

# 17. Upgrade M — Adaptive Concurrency

Static concurrency is not enough.

Track:

```text
average upload latency
HTTP/provider errors
rate limit errors
CPU saturation
disk queue
gameplay-active state
network throughput
```

Adjust workers within bounds.

Example:

```text
healthy + underutilized → +1 worker
rate limited → halve workers
disk saturated → reduce hash/materialize workers
gameplay active → cap bandwidth/workers
```

Do not let optimization cause stutter.

---

# 18. Upgrade N — Compact Manifest Layout

The current manifest remains the durable logical description.

Optimize its physical representation.

## 18.1 Keep semantic schema

Do not lose fields such as:

```text
logical path
classification
semantic role
hash
chunk refs
permissions
dependencies
restore tier
provenance
```

## 18.2 Store compactly

Remote manifest can be:

```text
binary or compact structured format
+
zstd compression
+
encryption
```

Options:

```text
CBOR
MessagePack
postcard
compact protobuf
```

JSON may remain available for diagnostics/export.

## 18.3 Path dictionary

For large manifests:

```text
logical root table
directory prefix table
chunk table
file table
```

avoid repeating huge strings.

---

# 19. Upgrade O — Metadata Batching

Commit path should approach:

```text
1. upload required immutable packs
2. upload one encrypted compact bundle manifest
3. upload COMMITTED
4. upload one catalog commit
5. update LATEST
```

Do not create dozens/hundreds of small metadata objects for a single generation unless required for fault isolation.

---

# 20. Upgrade P — Operation Resume Journal

Existing `sync_journal` should become the central durable transfer-resume record.

Persist:

```text
operation_id
pack_id/object_id
direction
local_path
remote_key
state
attempt_count
last_error
completed_at
```

States:

```text
PLANNED
BUILDING
READY
UPLOADING
UPLOADED
DOWNLOADING
DOWNLOADED
VERIFIED
FAILED_RETRYABLE
FAILED_FINAL
```

## 20.1 Backup retry

If a process/VM operation restarts:

```text
skip packs already confirmed uploaded
reuse existing local completed packs
rebuild only missing local output
resume manifest commit
```

## 20.2 Restore retry

Reuse:

```text
downloaded packs
verified packs
materialized staging files
```

Do not restart from zero.

---

# 21. Upgrade Q — Local CAS Cache

Maintain local reusable immutable content.

Path example:

```text
/var/lib/noland/state/cache/cas/
```

Store:

```text
pack files
pack indexes
verified content
recent manifests
```

## 21.1 Cache use during backup

If content/chunk already exists locally:

```text
do not regenerate bytes
```

## 21.2 Cache use during restore

```text
manifest
→ determine needed pack IDs
→ local hit
→ remote fetch only misses
```

## 21.3 Eviction

Use byte-based LRU.

Pin:

```text
active app latest generation
latest restore generation
currently running operation
```

Because VMs are disposable, local cache is an optimization, not durable truth.

---

# 22. Upgrade R — Remote CAS Index Cache

The client/agent may keep a compact remote-known index.

Example:

```text
provider_id
root_id
pack_id
remote_key
remote_size
confirmed_at
```

Use it to avoid repeated listings.

If an expected remote object cannot be fetched:

```text
invalidate entry
refresh relevant prefix
retry
```

Never treat cached presence as proof forever.

---

# 23. Upgrade S — Restore Priority Tiers

Change restore objective from:

```text
time to complete restore
```

to:

```text
time to playable
```

## Tier 0 — prerequisites

Examples:

```text
required runtime metadata
required user-provided BIOS/firmware
required launcher/profile mapping
```

## Tier 1 — must-have state

Examples:

```text
save files
worlds
memory cards
critical config
profile DB
mods required by save
controller mapping if game depends on it
```

## Tier 2 — needed soon

Examples:

```text
secondary configs
useful game-local data
optional mod metadata
small rebuild-expensive caches
```

## Tier 3 — background/optional

Examples:

```text
screenshots
logs
history
thumbnails
rebuildable caches
```

When Tier 0 + Tier 1 are applied and validated:

```text
READY_TO_LAUNCH
```

Continue Tier 2/3 asynchronously.

---

# 24. Add Restore Tier to Classification/Manifest

Add a field conceptually equivalent to:

```rust
pub enum RestorePriority {
    Prerequisite,
    Critical,
    Soon,
    Background,
}
```

It is separate from:

```text
PersistenceClass
SemanticRole
DependencyRequirement
```

Examples:

```text
save.dat
  PersistentState
  UserState
  Critical

bios.bin
  RequiredDependency
  Prerequisite

shader-cache.bin
  Ephemeral / optional
  Background

screenshot.png
  PersistentState
  Background
```

---

# 25. Upgrade T — Parallel Restore Download

From manifest:

```text
Tier 0 required packs
Tier 1 required packs
```

should be fetched in parallel first.

Then:

```text
Tier 2
Tier 3
```

Use a priority queue:

```rust
BinaryHeap<RestoreFetchTask>
```

or equivalent.

Prioritize:

```text
higher restore tier
+
smaller files when they unlock launch quickly
+
dependencies with multiple downstream references
```

---

# 26. Upgrade U — Streaming Verification

Do not:

```text
download pack
write full pack
read full pack again
hash
```

when avoidable.

Instead:

```text
network stream
→ ciphertext/auth verification
→ decrypt
→ BLAKE3/chunk validation
→ local cache/staging
```

One data pass where practical.

Do not remove content verification.

Remove redundant verification passes.

---

# 27. Upgrade V — Skip Identical Restore Targets

Before writing an existing target:

Use:

```text
incremental index / target metadata
```

and if necessary hash.

If target already matches expected content:

```text
skip write
```

This is especially useful when restoring over a partially restored/reused VM.

---

# 28. Upgrade W — Restore Background Hydration

Once critical state is ready:

```text
app launch may proceed
```

while background content continues.

The UI must show:

```text
Ready to launch
Background restore: 63%
```

Do not claim:

```text
Restore complete
```

until every selected component is finished.

---

# 29. Upgrade X — Predictive Prefetch

This is advanced and should come after tiered restore works.

Learn from prior runs:

```text
which files are opened in first 10s
which files are opened in first 60s
which packs contain those files
```

Persist an app startup profile.

On restore:

```text
manifest
→ critical files
→ startup profile pack prefetch
→ launch
→ remaining background
```

This is particularly useful for:

```text
emulators
large modded games
large prefixes
game libraries
```

---

# 30. Upgrade Y — Fast Backup Modes

Expose backup policy modes.

## Fast Checkpoint

```text
strong PersistentState
required SharedState
critical config
no optional app content
minimal incremental reconciliation
```

Best for frequent background checkpointing.

## Balanced

Default.

```text
PersistentState
important SharedState
common useful config
normal reconciliation rules
```

## Full Capture

```text
all relevant persistent state
optional app content if portability requested
captured immutable app base where required
wider reconciliation
```

Fast checkpoint should be very cheap when the observer journal is healthy.

---

# 31. Upgrade Z — Classification as Performance Policy

The existing classifier should directly control optimization.

Map to:

```text
MUST_BACKUP
SHOULD_BACKUP
OPTIONAL
IGNORE
```

Example:

```text
PersistentState + Critical
  → MUST_BACKUP

PersistentState + Soon
  → SHOULD_BACKUP

ReconstructableApp + AppContent
  → OPTIONAL in personal-state mode

Ephemeral
  → IGNORE

BaseImage
  → IGNORE payload, keep dependency metadata
```

This prevents wasting hash/pack/network time on irrelevant data.

---

# 32. Remote Layout After Optimization

Keep the existing immutable commit model.

Recommended physical layout:

```text
Noland Shared Storage/
├── catalog/
│   ├── LATEST
│   └── commits/
│       └── <catalog_commit_id>.enc
│
├── bundles/
│   └── <app_id>/
│       └── <bundle_id>/
│           ├── manifest.enc.zst
│           ├── manifest.hash
│           └── COMMITTED
│
├── packs/
│   └── <prefix>/
│       └── <pack_id>.pack
│
├── smallpacks/
│   └── <app_id>/
│       └── <generation>/
│           └── <bucket>.pack
│
├── checkpoints/
│
└── instances/
```

The old `objects/` direct-object namespace may remain for compatibility but new data should prefer packed storage.

---

# 33. Optimized Backup Flow

```text
StartBackup()
   ↓
return operation_id
   ↓
load mutation journal
   ↓
load incremental index
   ↓
determine reconciliation scope
   ↓
incremental reconcile only where needed
   ↓
BackupPlanner
   ↓
skip trusted unchanged files
   ↓
snapshot changed/relevant files
   ↓
stream BLAKE3 + FastCDC
   ↓
local CAS reuse
   ↓
small-file pack + normal pack builder
   ↓
RemoteSyncPlanner
   ↓
reuse remote-known set
   ↓
parallel bulk immutable upload
   ↓
compact encrypted manifest
   ↓
COMMITTED
   ↓
catalog commit
   ↓
LATEST
   ↓
checkpoint learned state/index
   ↓
clear committed mutation journal entries
```

---

# 34. Optimized Seal Flow

Seal should benefit heavily from continuous observer/index work.

Before deletion:

```text
SEAL_REQUESTED
   ↓
inspect dirty apps
   ↓
for each dirty app:
    use journal
    perform bounded reconcile
    fast checkpoint/balanced backup
   ↓
final learned-state/index checkpoint
   ↓
seal record
   ↓
COMMITTED
   ↓
SEALED
```

A well-maintained journal means seal should not begin with a broad filesystem rediscovery.

---

# 35. Optimized Restore Flow

```text
StartRestore()
   ↓
return operation_id
   ↓
fetch/verify manifest
   ↓
resolve logical roots
   ↓
build restore plan
   ↓
local CAS hit map
   ↓
queue missing Tier 0 / Tier 1 packs
   ↓
parallel download
   ↓
stream verify/decrypt
   ↓
materialize critical staging
   ↓
create rollback point
   ↓
apply critical state
   ↓
validate
   ↓
READY_TO_LAUNCH
   ↓
launch allowed
   ↓
continue Tier 2 / Tier 3 background hydration
   ↓
FULLY_RESTORED
```

---

# 36. Google Drive / rclone Optimization

## Short-term

Keep rclone but optimize:

```text
few process launches
large immutable packs
small-file packs
bulk copy
root caching
minimal metadata writes
parallel transfers
```

## Medium-term

Introduce provider capability metadata:

```rust
pub struct ProviderCapabilities {
    pub supports_atomic_move: bool,
    pub supports_conditional_create: bool,
    pub supports_bulk_list: bool,
    pub supports_server_side_copy: bool,
    pub preferred_object_size: Option<u64>,
    pub recommended_parallelism: usize,
}
```

Then tune Google Drive differently from future providers.

## Long-term

Only replace rclone if metrics show:

```text
rclone/provider abstraction is still a material bottleneck
```

after the rest of the pipeline is optimized.

---

# 37. Backend-Specific Google Drive Considerations

Optimize for:

```text
fewer files
larger packs
fewer folder operations
fewer metadata writes
fewer list/stat API calls
bounded parallelism
```

Google Drive should not see thousands of tiny CAS files when one immutable pack can contain them.

---

# 38. Changes to the fanotify Observer Design

The Linux observer itself should remain simple.

Do **not** add chunking/storage logic into `noland-fs-observer`.

Its responsibility remains:

```text
kernel event
→ normalized event
```

The main optimization integration occurs downstream of:

```text
ObserverHub::inject_fs()
```

## Required additions downstream

After successful session/classification:

```text
if persistent mutation:
    mutation_journal.upsert(...)
    dirty_roots.upsert(...)
    incremental_index.mark_dirty(...)
```

On overflow:

```text
incremental_index.invalidate_trust(...)
dirty_roots.mark_requires_reconcile(...)
```

On process/app exit:

```text
schedule cheap reconciliation if dirty
```

This preserves privilege separation.

---

# 39. Observer Read Events Must Not Slow Backup

External reads remain dependency discovery only.

Read events should:

```text
update dependency graph
update usage evidence
possibly update startup/prefetch profile
```

They should **not**:

```text
mark app dirty
trigger hashing
trigger backup
trigger full reconciliation
```

This is important for performance.

---

# 40. AppSession Integration

Every mutation journal entry must be tied to:

```text
app_id
session_id
correlation provenance
```

Example:

```rust
pub struct MutationRecord {
    pub app_id: AppId,
    pub session_id: SessionId,
    pub path: PathBuf,
    pub operation: MutationKind,
    pub correlation_method: CorrelationMethod,
    pub association_confidence: f32,
    pub first_seen_at: Timestamp,
    pub last_seen_at: Timestamp,
}
```

If PID/session correlation is weak:

```text
do not immediately classify as trusted dirty state
mark candidate + reconcile
```

---

# 41. Installer Transactions Integration

Installer/update transactions should invalidate wider index regions.

Example:

```text
Steam app update
```

should cause:

```text
install root → reconcile required
app binary/base metadata → re-evaluate
user save roots → not automatically rescanned fully
```

This avoids unnecessary user-state walks while correctly handling changed application content.

---

# 42. Snapshot Optimization

Do not snapshot large irrelevant roots.

Input to the snapshot coordinator should be the BackupPlan.

For Btrfs, whole-subvolume snapshot creation may still be cheap, but:

```text
hashing/materialization
```

must still target only relevant planned files.

For ext4 safe-copy fallback:

```text
copy only planned paths/roots
```

not entire home state.

---

# 43. Chunking Optimization for Large Mutable Files

FastCDC already helps when bytes shift.

For frequently modified large files:

```text
memory cards
world region files
large save archives
VM-like images
```

retain previous chunk map.

When reprocessing:

```text
stream file
→ FastCDC
→ compare chunk hashes
→ upload only new chunks
```

Do not switch to fixed chunks unless measurement proves superior for a specific file class.

---

# 44. Compression Strategy

Do not compress already-compressed media aggressively.

Classify likely data:

```text
text/config/JSON/DB → compress useful
zip/pak/video/ROM compressed format → often low gain
```

Pack builder may use heuristics:

```text
compression = zstd
compression = none
```

per pack/bucket.

Measure CPU vs upload savings.

---

# 45. Resource Scheduling During Gaming

Noland is a cloud-gaming product, so storage speed cannot come at the cost of stream quality.

When active game session detected:

```text
nice hash/pack workers lower
ionice background
upload bandwidth cap
fewer disk workers
fewer hash workers
```

After exit:

```text
increase workers
remove upload cap up to configured maximum
```

Seal can use high throughput because gameplay is ending.

---

# 46. Retry Granularity

Retry the smallest failed unit.

Examples:

```text
one pack upload failed
→ retry pack

manifest upload failed
→ retry manifest

COMMITTED write failed
→ retry marker

LATEST pointer failed
→ retry pointer

local disk full
→ fail operation immediately

reconciliation failed
→ fail before expensive transfer
```

Do not restart a 50 GB backup because one final metadata write failed.

---

# 47. Rate Limit Handling

Provider errors should feed adaptive transfer control.

On:

```text
429
rate limit
quota throttle
repeated transient 5xx
```

perform:

```text
exponential backoff
reduce worker count
preserve completed object state
```

Do not fan out retries from every worker simultaneously.

Use a shared provider backoff gate.

---

# 48. New/Extended Tables

Add or extend equivalent schema:

```sql
CREATE TABLE operation_metrics (
    operation_id TEXT PRIMARY KEY,
    metrics_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE app_mutation_journal (
    app_id TEXT NOT NULL,
    canonical_path TEXT NOT NULL,
    mutation_kind TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    event_count INTEGER NOT NULL DEFAULT 1,
    requires_reconcile INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(app_id, canonical_path)
);

CREATE TABLE dirty_roots (
    app_id TEXT NOT NULL,
    canonical_root TEXT NOT NULL,
    reason TEXT NOT NULL,
    first_dirty_at INTEGER NOT NULL,
    last_dirty_at INTEGER NOT NULL,
    requires_reconcile INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(app_id, canonical_root)
);

CREATE TABLE file_state_index (
    app_id TEXT NOT NULL,
    canonical_path TEXT NOT NULL,
    logical_root TEXT,
    relative_path TEXT,
    device INTEGER,
    inode INTEGER,
    size INTEGER,
    mtime_ns INTEGER,
    file_type TEXT,
    content_id TEXT,
    file_hash TEXT,
    trust_state TEXT NOT NULL,
    last_reconciled_generation TEXT,
    last_backup_generation TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(app_id, canonical_path)
);

CREATE TABLE local_cas (
    content_id TEXT PRIMARY KEY,
    local_path TEXT NOT NULL,
    size INTEGER NOT NULL,
    last_accessed_at INTEGER NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE remote_content_index (
    provider_id TEXT NOT NULL,
    content_id TEXT NOT NULL,
    remote_key TEXT NOT NULL,
    size INTEGER,
    last_confirmed_at INTEGER NOT NULL,
    PRIMARY KEY(provider_id, content_id)
);
```

Exact schema should fit the current DB.

---

# 49. RPC Changes

Existing RPC should evolve to:

```text
StartBackup(app_id, mode)
GetOperationStatus(operation_id)
CancelOperation(operation_id)
RetryOperation(operation_id)

StartRestore(bundle_id, mode)
StartSeal(instance_id)

GetPerformanceDiagnostics(operation_id)
GetObserverHealth()
GetDirtyState(app_id)
GetIncrementalIndexStats(app_id)
```

Status should include:

```json
{
  "operation_id": "...",
  "kind": "backup",
  "stage": "UPLOADING",
  "progress": {
    "files_done": 120,
    "files_total": 150,
    "bytes_done": 104857600,
    "bytes_total": 157286400
  },
  "metrics": {
    "local_cas_hits": 42,
    "remote_known_hits": 31,
    "new_packs": 3
  }
}
```

---

# 50. UI Changes

## Backup

Show:

```text
Preparing changed state
3 files changed
82 files reused
Packing
Uploading 24 MB
Committed
```

Do not show a fake per-file progress bar when work is pack-based.

## Restore

Show:

```text
Downloading critical state
Applying saves
Ready to launch
Restoring optional data in background
```

## Diagnostics

Development UI:

```text
reconcile: 120 ms
hash: 280 ms
pack: 130 ms
upload: 850 ms
remote calls: 4
rclone invocations: 1
CAS reuse: 92%
```

This makes optimization measurable.

---

# 51. Implementation Order

The agent should implement in this order.

## Phase 1 — Measure and decouple callers

1. Add operation metrics.
2. Add async/background `OperationManager`.
3. Persist operation state.
4. Add cancel/retry/status RPC.

**Exit condition:** backup/restore no longer depend on one long blocking RPC.

---

## Phase 2 — Observer-driven incremental scope

5. Add mutation journal.
6. Add dirty-root tracking.
7. Wire `ObserverHub::inject_fs()` classified mutations into journal.
8. Wire fanotify overflow/restart gaps into trust invalidation.
9. Add persistent file-state index.
10. Implement narrow reconciliation scopes.

**Exit condition:** repeat backup with one changed save does not walk/hash the entire app state.

---

## Phase 3 — Planner and fast skipping

11. Add deterministic `BackupPlanner`.
12. Reuse trusted unchanged `content_id`.
13. Skip hashing unchanged files.
14. Add remote sync planner.
15. Cache `ensure_root()`.

**Exit condition:** planner produces explicit changed/reused/upload sets before transfer.

---

## Phase 4 — Data path

16. Replace `read_to_end()` chunking with streaming.
17. Add bounded hash/chunk worker pool.
18. Add local CAS reuse.
19. Add small-file pack layer.
20. Compress manifest and appropriate packs.
21. Add parallel immutable upload.

**Exit condition:** CPU/disk/network overlap and memory is bounded.

---

## Phase 5 — Remote-call reduction

22. Remove per-object `stat`.
23. Add batch/prefix remote-known comparison.
24. Reduce rclone process count.
25. Batch metadata.
26. Preserve tiny final commit sequence.

**Exit condition:** remote control-plane calls per backup are near O(number of packs), not O(number of files/chunks).

---

## Phase 6 — Resumability

27. Extend sync journal.
28. Resume uploaded packs.
29. Resume downloaded packs.
30. Resume materialization.
31. Add shared provider backoff/rate-limit controller.

**Exit condition:** interrupted transfer continues instead of restarting.

---

## Phase 7 — Restore speed

32. Add restore priorities to manifest/classifier.
33. Build restore planner.
34. Fetch Tier 0/1 first.
35. Add local CAS lookup.
36. Parallel download.
37. Stream verify/decrypt.
38. Skip identical targets.
39. Mark `READY_TO_LAUNCH`.
40. Continue optional hydration in background.

**Exit condition:** time-to-playable is significantly shorter than total restore time.

---

## Phase 8 — Adaptive and backend-specific tuning

41. Adaptive worker counts.
42. Gameplay-aware throttling.
43. Google Drive-specific concurrency/object-size tuning.
44. Startup-profile/predictive prefetch.
45. Evaluate whether rclone is still a bottleneck.

**Exit condition:** provider and machine resources are used efficiently without gaming regressions.

---

# 52. Priority by Expected Payoff

## Highest payoff

1. Persistent mutation journal.
2. Persistent incremental file index.
3. Narrow reconciliation.
4. Skip hashing unchanged files.
5. Remove per-object remote stat.
6. Parallel immutable transfer.
7. Small-file packing.
8. Streaming chunking.
9. Tiered restore.
10. Async operation manager.

## Medium payoff

11. Local CAS cache.
12. Remote content index cache.
13. Reduced rclone process count.
14. Compact metadata/manifest.
15. Resumability.
16. Adaptive worker tuning.

## Advanced

17. Lazy/background hydration.
18. Startup predictive prefetch.
19. Backend-specific transport.
20. Cross-generation pack compaction/GC optimization.

---

# 53. Correctness Tests

## Incremental backup

1. Initial backup.
2. Change one save.
3. Backup again.
4. Assert:
   - only save root/path reconciled;
   - unchanged files not rehashed;
   - only new chunks/packs uploaded.

## Observer overflow

1. Force source overflow.
2. Change file.
3. Backup.
4. Assert broad-enough reconciliation runs before commit.

## App update

1. Update Steam app.
2. Save unchanged.
3. Assert install root re-evaluated.
4. Assert save root not fully rescanned without reason.

## Cache write

1. Game writes cache.
2. Assert no durable backup dirty state.

## Save write

1. Game writes save.
2. Assert mutation journal updated.
3. Assert dirty root updated.

---

# 54. Performance Tests

Create baseline measurements for:

```text
100 tiny files
10,000 tiny files
1 GB single file
10 GB app state
100 GB app install + 10 MB save delta
high-latency Drive connection
fast local NVMe
active game streaming
```

Measure:

```text
backup time
restore time
time to first upload
time to playable
CPU
RAM
disk utilization
remote API calls
rclone invocations
bytes uploaded
dedupe ratio
files scanned
files rehashed
```

---

# 55. Release Performance Targets

Initial targets:

## Repeat backup with no state changes

```text
no remote payload upload
minimal metadata/checkpoint only
no broad reconciliation
no full hashing
```

## Repeat backup with one small save changed

```text
scan/reconcile only affected area
rehash changed file only
upload one/few new packed chunks
```

## Large install + tiny state delta

Example:

```text
100 GB reconstructable game
20 MB changed save/mod state
```

Expected transfer should be near the changed state size, not app size.

## Restore

`READY_TO_LAUNCH` should occur once critical state is present, even if optional/background hydration remains.

---

# 56. Do Not Do These Optimizations

Do not:

- disable reconciliation entirely;
- trust mtime/size after observer loss;
- drop mutation events silently;
- make fanotify helper write DB/CAS;
- upload reads just because they were observed;
- make remote index cache the durable truth;
- use destructive sync to reduce calls;
- mutate packfiles;
- repack all historical tiny files when one changes;
- merge divergent saves automatically;
- skip cryptographic verification;
- start with a custom Google Drive protocol before measuring;
- increase concurrency without rate-limit/backpressure control;
- run aggressive hashing/upload while gameplay is latency-sensitive;
- delete journal entries before cloud commit success.

---

# 57. Final Optimized Design

The complete optimized architecture should become:

```text
fanotify
   ↓
noland-fs-observer
   ↓
ObserverHub::inject_fs()
   ↓
PID/cgroup → AppSession
   ↓
USES vs MUTATES
   ↓
classification
   │
   ├── dependency graph
   │
   └── mutation journal
            ↓
     persistent file index
            ↓
     incremental reconcile
            ↓
       BackupPlanner
            ↓
 trusted unchanged ──────────────┐
            │                    │ reuse prior content ids
            ▼                    │
 changed files                   │
            ↓                    │
 snapshot/staging                │
            ↓                    │
 streaming BLAKE3 + FastCDC      │
            ↓                    │
 local CAS ◄─────────────────────┘
            ↓
 small-file / normal pack builder
            ↓
 RemoteSyncPlanner
            ↓
 remote-known-set comparison
            ↓
 parallel immutable bulk upload
            ↓
 compact encrypted manifest
            ↓
 COMMITTED
            ↓
 catalog commit
            ↓
 learned-state/index checkpoint
```

Restore:

```text
catalog
   ↓
manifest
   ↓
restore planner
   ↓
local CAS hits
   ↓
Tier 0 prerequisites
   ↓
Tier 1 critical state
   ↓
parallel fetch + streaming verify
   ↓
staging + rollback point
   ↓
apply
   ↓
READY_TO_LAUNCH
   ↓
Tier 2 / Tier 3 background hydration
```

---

# 58. Core Optimization Rule

Whenever deciding between two implementations, prefer the one that follows:

```text
observe continuously
→ persist the knowledge
→ calculate the smallest safe delta
→ reuse prior work
→ perform expensive work once
→ batch remote metadata
→ transfer immutable content concurrently
→ make every step resumable
→ restore only what is necessary before launch
```

The current Noland tracking system already provides the foundation.

The optimization work should turn that tracking knowledge into **less filesystem work, less hashing, fewer remote calls, fewer objects, more concurrency, and much shorter time-to-playable** without reducing the safety guarantees of Shared Storage.


---

# Appendix A — Source Design Relationship

This optimization plan is a synthesis of the existing Noland Linux filesystem observer design and the deep Shared Storage upload/download optimization notes. It intentionally preserves the existing fanotify → `ObserverHub::inject_fs()` → `AppSession` → classification/reconciliation architecture, while integrating the performance work around incremental indexing, mutation journals, parallel transfer, packing, resumability, and tiered restore.
