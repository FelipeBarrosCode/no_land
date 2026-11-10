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
cache isolation and low-disk admission before existing saves are touched.

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
