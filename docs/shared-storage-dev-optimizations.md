# Shared Storage optimization branch

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
