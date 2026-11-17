# Shared Storage optimization branch

## Canonical installation for fully writable runtime applications

A validated fully writable native runtime workspace is the active installation,
not an additional installation to back up alongside its original import tree.
Fresh scans and capture plans select that workspace once, its private HOME and
runtime control records. The exact original source ELF is retained as a narrow
registration/discovery anchor; the original import directory's remaining
payload is not selected a second time.

This scope rule applies only after fresh validation of a matching native
workspace publication. Missing/mismatched registrations, invalid workspaces,
unsafe links and source-anchor changes still fail closed. Split writable views
continue to need the immutable original installation; AppImage package
directories are not suppressed as if they were extracted workspaces.

Historical source associations and local original files are retained. Inherited
original-payload manifest entries are omitted from the new current snapshot
without generating restore deletion instructions. Current workspace content,
including deliberate save/config/cache changes and deletions, is authoritative.
No filename-specific rule, metadata-size-limit increase, cloud deletion or
automatic upload is part of this correction.

The desktop branch starts at `dev` (`ea540d9`), Android at `7b4362b3`, and iOS at
`96d3105b`. Storage prerequisites already reviewed on `main` are carried onto
each branch before the new optimizations. The executable engine is the shared
Rust state-agent; desktop, Android, and iOS send operations to that engine.

## Implemented

| Area | Change | Correctness boundary |
| --- | --- | --- |
| Streaming connections | Separate adaptive limiter for single-connection `rclone cat`; no copyto multistream divisor | Provider error feedback still reduces concurrency and applies retry backoff |
| Manifest planning | Indexed inherited-file lookup/removal instead of per-path vector scans | Complete manifests, deletions, and retained chunk references remain explicit |
| Incremental complete captures | Reconciliation preserves verified identities for unchanged files | Dirty paths, observation gaps, restarts, mode/content changes and forced full verification invalidate reuse |
| Concurrent changes | Monotonic capture generations and version-specific mutation acknowledgement | Writes during hashing/upload and coalesced writes stay pending for the next capture |
| Capture staging | One atomic Btrfs view when available; otherwise only active hash jobs have staged copies, with Linux reflink first | Snapshot copies have independent contents; unstable captures lose fast-identity trust |
| Hash progress | Consume bounded worker results as they arrive | Failed or cancelled worker pools drain and join before returning |
| Pack layout | Launch tier, file path and file offset order | Shared chunks stay deduplicated; out-of-order restores still perform full readback verification |
| Pack/upload overlap | Bounded sealing workers feed upload batches while later packs are being sealed | Captured revision is durable before uploads; manifests become visible only after all packs succeed |
| Retry | Encrypted capture checkpoint and authenticated per-pack receipts retain bundle IDs, pack IDs and ciphertext | Changed/corrupt local artifacts, wrong keys and mismatched repositories fail closed; committed metadata is checked and reused |
| Restore overlap | Bounded authenticated-record channel feeds an owned assembly thread and bounded verification workers | Cancellation closes channels, joins workers and rolls back before returning; commit waits for progress acknowledgement |
| Memory | At most one decrypted record per stream turn, even for highly compressed input | Default queue holds eight records, each at most 8 MiB; active streams are separately bounded |
| Disk admission | Per-filesystem reservations cover destination data, rollback copies and the largest recovery temporary | Space is rechecked during writes; admission never deletes saves |
| Cache | Operation-specific CAS hardlink leases protect active and retryable inputs; released cache is trimmed | Another operation's lease prevents eviction; retry-owned leases survive interruption; manifest/chunk-index reuse is scoped to the repository and vault key |
| Visibility | Live hash, pack/upload, preflight, rollback, assembled, published and readback counters | Stage times can overlap and must not be added to estimate elapsed time |

`NOLAND_STORAGE_MIN_FREE_BYTES` controls the free-space margin (default 64 MiB).
`NOLAND_CAS_CACHE_BYTES` controls the released CAS cache target (default 1 GiB).
Active and retry-owned content may exceed that cache target. A failed capture
retains its encrypted packs/checkpoints and necessary chunk lease for retry.
Reservations are conservative, process-local admission estimates; unrelated
processes can still consume disk space, so write failures retain recovery data.

### Finalization is not payload upload

An unchanged payload can reuse every authenticated chunk and upload zero game-data
packs while still reading and publishing substantial manifest/catalog metadata.
Pack-journal bytes deliberately do not count that metadata traffic. Capture EOF,
zero active workers, and zero new packs therefore never imply backup completion.

Backup progress distinguishes manifest publication, catalog publication, guarded
history convergence, local metadata caching, file-index persistence, checkpoint
writing, and exact repository-claim release. Additive `finalization` diagnostics
retain each stage's duration, numeric provider I/O, and metadata-read accounting;
they contain no provider credentials or application payloads. Failed diagnostic
writes must not replace transfer errors or become durability barriers. Completion
still follows the existing authenticated publication and repository finalization
checks, including visible cleanup-required warnings.

History inventory overlaps at most one bundle's independent manifest/index reads,
and only when the provider permits multiple downloads. Both reads drain before a
failure returns; commit-marker verification remains afterward. Ordered prefix
lookup avoids rescanning every metadata object for each bundle. The second fresh
inventory before deletion, authentication, generation checks, deletion/absence
receipts, flushes, and publication barriers remain mandatory. These changes reduce
serialization and CPU work, not the number of required authority checks; live
before/after performance must be measured separately.

### Responsive agent browsing

Health and indexed application listing use a separate, query-only SQLite WAL
connection. They read committed rows rather than waiting on the observer or
checkpoint writer's Rust mutex. Database errors still propagate; busy reads never
become a guessed zero-operation count or empty catalog. Writer poison remains a
failure, including when it occurs while a browse read is queued.

`ListApps` and `ListBackupCandidateIds` read the existing index; they do not trigger
synchronous filesystem discovery. Existing candidate filtering is retained.
Startup/periodic discovery, explicit index refresh and operation submission keep
their discovery paths, and execution continues using the serialized writer and
fresh ownership/capture checks. These display/health reads grant no new authority.

Tracking gaps still mark applications/roots dirty, bulk-invalidate file identities
and advance capture generations transactionally. They no longer repeat that
invalidation in a separate transaction for every indexed file. Already-invalid
states need no repeated SQL UPDATE, but every new gap retains its generation
barrier. No observer events or safety checks are disabled to keep browsing fast.

### Reconciliation work and active-operation responsiveness

An index-only V7 migration adds `idx_file_state_needs_verification`, keyed by
application and containing only states whose trust is not `VERIFY_REQUIRED`.
The existing invalidation SQL, transactions, dirty evidence and capture-generation
increments are unchanged. Repeated gaps therefore retain their barriers without
scanning all already-invalid rows. Newly trusted, dirty or missing states still
enter the candidate index and must be invalidated by the next gap.

The DB-only regression uses the same UPDATE on synthetic V6/V7 databases:

| Already-invalid states | V6 SQL VM steps | V7 SQL VM steps |
| ---: | ---: | ---: |
| 4,096 | 20,492 | 17 |
| 187,282 | 936,422 | 17 |

These are algorithmic measurements, not a live-cloud speedup or a promise that
reconciliation takes a particular number of seconds. Initial index creation must
still read existing records. Migration tests retain all previous tables, rows,
receipts and generations, check transactional rollback and idempotent reopening,
and exercise V6 SQL against the upgraded database. They do not certify arbitrary
older binaries or power-loss recovery. No database is restored as a binary
rollback mechanism. Pack, manifest and learned-checkpoint formats are unchanged.

Managed operation futures execute on joined blocking workers rather than Tokio's
socket/I/O workers. A worker is admitted only after acquiring its existing
execution lane; there is at most one worker per lane. Queued cancellation
callbacks have a separate two-slot budget. The worker retains the lane and its
running lease through synchronous work, cancellation and terminal callbacks.
Aborting the asynchronous supervisor requests cancellation but cannot advertise
idle or free the lane while its worker is still draining. Durable finalization
continues to drain exact repository cleanup instead of being dropped.
Execution identities prevent a stale supervisor or lease from affecting a later
invocation that reuses the same operation ID. These tests exercise cancellation
and supervisor abort with the I/O runtime alive, not abrupt process termination,
power loss or complete runtime shutdown; the existing restart/recovery and
cleanup-required handling remain necessary.

Operation-status polling reads only its operation header, progress and PACK
totals in one committed read transaction on the query-only WAL connection.
It neither parses large result/retry JSON nor performs legacy projection
backfill. Oversized, malformed or miscorrelated progress remains an error, not
invented zero progress. Matching, submission, cancellation and commit verification
retain their existing authoritative paths.

RPC dispatch reserves a bounded display lane separately from writer-dependent
requests. Handlers acquire capacity asynchronously before starting blocking work,
so database waiters cannot occupy the socket workers or exhaust an unbounded
blocking queue. A started handler retains its capacity until it actually ends;
losing its waiting connection does not cancel accepted work. Nested retries use
the already-admitted inner dispatcher instead of reacquiring the same budget.

The collector uses one serialization gate and one joined blocking job at a time.
Background turns consume FIFO prefixes of at most 512 facts and promptly schedule
remaining facts. Failed or panicking ingestion restores the failed fact and
unprocessed tail ahead of newer arrivals; coalescing cannot cross the restored
boundary. Newer overflow is explicitly reported as loss rather than silently
discarding the failed batch. Failed turns are paced only after their worker has
joined, so a persistent database/binding error cannot create a hot retry loop.
Normal successful prefixes remain prompt, and RPC timeouts are unchanged.
Normal bounded backpressure is not an observation gap. Unresolved
filesystem facts retain their original process-generation proof and deadline
across turns, but runtime-binding and root-authority caches are rebuilt each turn.
Lost boundaries and expired/unidentified facts require durable reconciliation
fences before retirement; failures retain the obligation.

A fresh capture still consumes a full queue watermark before selecting reusable
files, and explicitly fences unresolved facts. New arrivals do not prevent
completion of that watermark, while newer loss generations remain sticky.
Acknowledging a durable loss fence does not certify PID identity. Procfs recovery
cannot lend a newer PID mapping to older queued facts: process generation,
executable and session proofs are checked, with conservative fences when
ownership remains uncertain. Already authenticated coherent prepared revisions
instead retain their frozen selection: current discovery/flush belongs after
prepared-revision validation, not before replay. A saved all-app child plan is reused without
rediscovering or replacing its child IDs; new selections and unprepared children
retain the normal fresh checks. No retry flag or checkpoint-file existence alone
authorizes this distinction.

`database_contention` diagnostics are process-local cumulative mutex-acquisition
counts and wait times. Clones share counters; they contain no paths, SQL, payloads
or credentials and do not change database rows or generations. Concurrent wait
totals are not operation wall time, and the counters grant no health, capture,
execution or repository authority.

Observation gaps still require genuine re-verification. This work does not trust
size/mtime alone, disable the observer, relax private-runtime ownership, widen
scan roots, alter application CPU/GPU limits, or remove authenticated publication,
flush, source-generation, claim-release and deletion barriers. The deleted test
instance is not reconnected or recreated; a future matched live benchmark or
cloud restore/readback remains a separately authorized acceptance step.
Native Linux/eBPF ingress throughput, including the new per-observation procfs
generation checks, still needs controlled validation. Unproved ownership is
refused or conservatively fenced, never converted into permission to reuse a
hash merely to improve a timing.
No additional Linux capabilities are granted. Validate procfs availability under
the shipped service's existing capability and LSM policy, not only a privileged
test shell.

Local regression gates (synthetic data only; no app/native-mobile build):

```sh
cargo +1.94.1 test --locked --manifest-path state-agent/Cargo.toml \
  -p noland-state-db -p noland-observer -p noland-attribution \
  -p noland-state-agent --lib -- --test-threads=1
cargo +1.94.1 fmt --manifest-path state-agent/Cargo.toml --all -- --check
```

The full engine workspace remains an additional gate for encryption, pack,
restore, repository admission, coherent retry and private-data integrity.
Socket regressions require health/progress replies before releasing a blocked
handler on one- and two-worker runtimes; they do not increase the existing
eight-second RPC timeout. Frozen-retry RPC fixtures use a child test process and
a filesystem-only fake rclone, not inherited cloud credentials or a real remote.

### Writable runtime data

The original executable's installation is not necessarily where an application
writes its current saves. Complete capture resolves an exact runtime registration
to its current private HOME and selected writable view or full workspace, plus
the exact registration/publication records. These scopes are revalidated for
every complete reconciliation, including after process exit. They are not broad,
permanent grants over the runtime directory. Shared images/extraction caches,
other applications, legacy executable placeholders, and immutable view aliases
do not become writable payload.

`/app` observations require verified mount/file identities and a matching private
HOME; attribution additionally validates the exact registration. Private file
identities are reverified even without observer events, while unchanged immutable
originals may retain their normal fast reuse. Invalid or changed bindings fail
closed; malformed registration discovery does not stop the observer/RPC service.
Discovery retains a validation-required marker, not private root authority, so
removing a registration before the first private capture cannot authorize an
original-only backup. The marker is hidden from normal root consumers and
survives learned checkpoints.

Historical directory-only index entries for strict ancestors of the validated
source installation do not constitute missing private payload. The ownership
guard accepts those structural entries only with a fresh exact runtime binding,
an indexed directory type, and no-follow verification of an actual directory.
They remain excluded from traversal and never widen the installation root.
Unbound files, symlinks, sibling private directories, missing entries and invalid
registrations still fail closed; no historical associations are deleted.

Candidate selection and frozen capture controls must share the same validated
layout generation. Streaming capture checks this after job identities freeze;
non-streaming capture checks it after file capture, before sealing retry data.
A later layout change does not retroactively invalidate replay of an already
authenticated, coherent frozen revision.

Fixtures verify restored **file contents**, not application launch. Registration
absolute paths and legacy device/inode receipts still need an explicitly guarded
restore migration on another filesystem. Automatically retiring/rebinding a
previously indexed runtime layout is intentionally unsupported. These changes
neither migrate a live workspace nor relaunch its application.

The on-disk pack and manifest formats are compatible with existing revisions.
The restore transaction journal remains v2 with v1 recovery support. SQLite
migration v3 adds capture generations without replacing existing state.

## Validation

The dedicated `restore-engine-ci.yml` workflow runs library tests, agent type
checks, formatting, source contracts and restore/pack/storage lint. Regression
cases cover authenticated/truncated records, shared/out-of-order chunks, atomic
publication, cancellation, crash recovery, rollback, 4,096 tiny files, independent
CAS leases, unchanged complete captures, one-file changes, observation gaps,
lost upload acknowledgements, interruption after the commit marker, repository/key
cache isolation and low-disk admission before existing saves are touched. Additional
failure-boundary tests cover concurrent save writes during upload, same-identity
offline writes after restart, corrupted checkpoints/receipts/ciphertext, multi-pack
retry identity, and interrupted lease cleanup without unpinning another capture.

An opt-in library workload exercises the complete capture and restore paths with
186,619 synthetic files (the Hookshot file count), mixing four 8 MiB files with
many small distinct files. It runs first/unchanged/one-save captures and first/
unchanged restores. It asserts zero rehashing on the unchanged capture, exactly
one rehash after one save changes, byte-correct restoration and no pack downloads
for an already identical destination.

The workload prints `STORAGE_SCALE` JSON with elapsed time, files/second, bytes
hashed, assembled and read back, and sampled filesystem-space growth. That disk
measurement includes other activity on the runner filesystem and is not a
process-specific peak. This is a debug library workload with reduced payload
size, not a measurement of the original 35 GB game or live cloud throughput.

Run in a sufficiently provisioned isolated checkout:

```sh
NOLAND_SYNTHETIC_FILES=186619 cargo test --locked \
  --manifest-path state-agent/Cargo.toml -p noland-state-agent --lib \
  backup::scale::synthetic_storage_workload -- --ignored --nocapture --test-threads=1
```

On the optimization branch, a push whose commit message includes `[storage-scale]`
also runs that workload in the library-only workflow. Ordinary regression runs
skip the expensive measurement. Real restore acceptance remains a native Shared
Storage Sync operation with the deployed engine and the original repository key.

## Recorded synthetic results — November 10, 2026

These are Ubuntu 24.04 hosted **debug library** runs for each repository, using
local fixture storage. The initial logical payload is 78,528,647 bytes across
186,619 files; the single-save modification is 16 bytes. This matches the game’s
file count, not its 35 GB payload or cloud-provider behavior. There is no paired
pre-optimization baseline, so these timings do not establish a speedup ratio.

| Repository / tested revision | First capture | Unchanged capture | One-save capture | Restore | Unchanged restore |
| --- | ---: | ---: | ---: | ---: | ---: |
| [Desktop `033bcf9`](https://github.com/FelipeBarrosCode/no_land/actions/runs/42907561153) | 390.8 s | 120.5 s | 205.8 s | 62.8 s | 11.7 s |
| [Android `a473fbff`](https://github.com/FelipeBarrosCode/no_land_android/actions/runs/42907738083) | 517.5 s | 170.7 s | 286.4 s | 72.4 s | 11.2 s |
| [iOS `5990c200`](https://github.com/FelipeBarrosCode/no_land_mobile_port/actions/runs/42907737712) | 695.5 s | 266.2 s | 427.3 s | 97.4 s | 19.4 s |

All three runs verified zero files/bytes hashed on the unchanged capture and exactly
one file / 16 bytes hashed after the save changed. The first restore downloaded
six packs and published all files; the unchanged restore reused all 186,619 files
and downloaded zero packs. Ordered reconstruction needed zero additional
post-assembly readback bytes in this fixture; all published files still pass
complete-file hash verification.

The sampled peak filesystem growth was 3,456,978,944 bytes on the desktop run,
3,494,731,776 bytes on Android, and 3,471,769,600 bytes on iOS. This includes fixture,
database, cache, destination and unrelated runner activity, rather than isolating
engine scratch space. Runner timings vary; this is not a platform-speed comparison.
Unchanged captures still scan/reconcile the namespace, and changed captures still
serialize complete revision metadata and a database checkpoint. The elapsed
times above include those costs even when almost no content is rehashed.

### Final regression checks

After the cache-isolation and lease-cleanup fixes, the source contracts, library
tests, agent integration checks and strict restore-library lint passed in all
three repositories:

- [Desktop `9217ab5`](https://github.com/FelipeBarrosCode/no_land/actions/runs/42911935781)
- [Android `48204597`](https://github.com/FelipeBarrosCode/no_land_android/actions/runs/42912016942)
- [iOS `a5ccecaf`](https://github.com/FelipeBarrosCode/no_land_mobile_port/actions/runs/42912345241)

The 30 optimized engine files under `state-agent/crates` match byte-for-byte
across these revisions. The synthetic timings above come from the earlier,
explicitly identified scale runs; the final runs exercise the added recovery
regressions. Review/promotion to `main` and deployed native Shared Storage Sync
acceptance remain pending.
# Runtime layout transitions and future restores

A split writable view (`portable/<runtime-id>/view`) can be migrated into a
fully writable workspace without changing the application's source identity,
runtime ID, or private HOME. Old path associations must remain as historical
evidence: deleting them, widening a root, or clearing the database is not a
reconciliation strategy.

Native migration now records a bounded `legacy-import.json` alongside the new
workspace receipt. The receipt pins its SHA-256; the proof identifies the exact
application, source, import snapshot, old receipt and old private file identities.
Only those exact historical keys can be retired from fresh scans, candidate
selection and inherited manifests. They do not produce restore tombstones.
Unexpected historical keys, changed legacy files, new legacy entries, substituted
boundaries, missing/corrupt proofs and mismatched applications still fail closed.
No source files, old private data, save files or cloud objects are deleted.

The current registration, current private HOME/workspace and pinned migration
proof remain in complete backups. A fresh same-home restore therefore enumerates
the current runtime registration without recreating the obsolete view. Missing
retired payload is permitted only when both its old view and receipt are absent;
present retired data must still match the recorded identities. The proof is not
new traversal authority and is revalidated with the current binding generation.

An older completed native migration may acquire this record only through the
approved `reconcile-records` command. It reconstructs the original import
inventory using the native validation path and requires the saved import digest
to match before recording any retirement. It cannot silently bless later writes
to the old view. Current workspace save changes are independent of that frozen
import evidence.

This addresses split-view-to-workspace transitions. It is not a general
cross-home path-rebasing mechanism or permission to rewrite restored filesystem
identities, and it does not claim live upload/restore/launch acceptance.
