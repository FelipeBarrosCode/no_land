# Noland Restore — Storage-Efficient Streaming Plan

**Goal:** restore large Steam games without requiring a full duplicate of the game in restore staging, while preserving integrity, atomicity, cancellation, rollback, and resumability.

**Primary case:** Steam game binaries plus selected saves/configuration. Proton, Steam runtimes, compatibility data, shader caches, and unrelated system/application files must not be included in the Steam game bundle unless explicitly selected as runtime state.

## 1. Current failure mode

The current restore materializes reconstructed files under:

```text
/var/lib/noland/state/restore/<restore-id>/materialized/tree/
```

and later copies them into the destination. During restore, storage can contain:

```text
downloaded encrypted packs
extracted CAS chunks
materialized staging tree
rollback snapshot
final destination files
```

This creates a large temporary multiplier. The previous large restore reached the launch milestone but failed while finishing because the final phase exhausted disk space.

## 2. Target architecture

```text
Fetch manifest and plan
        ↓
Compute required chunks and disk budget
        ↓
Download a bounded pack window
        ↓
Validate/decrypt/extract chunks
        ↓
Materialize verified files directly into destination temp files
        ↓
Atomic rename into final destination
        ↓
Release zero-reference chunks and processed packs
        ↓
Continue until complete
```

The restore must not build a complete duplicate tree before applying it.

## 3. Correctness invariants

Preserve:

- BLAKE3 verification for every restored chunk and file;
- immutable cloud manifests and pack format;
- path traversal and symlink safety checks;
- snapshot/rollback behavior for files that are overwritten;
- atomic publication: an incomplete file is never visible at its final path;
- prior committed application state when cancellation or failure occurs;
- deterministic manifest and restore-plan semantics;
- `READY_TO_LAUNCH` as an intermediate milestone and `COMPLETED` as the only final success state.

Never delete a chunk or pack that is still required by an active restore, another operation, or a resumable journal.

## 4. Phase 0 — narrow Steam bundle scope

Before changing restore storage behavior, fix Steam selection at the source.

For `steam:<appid>` backups, include only:

- the selected Steam installation root and game binaries/content;
- the app manifest needed to register the installation;
- saves and configuration paths explicitly associated with the game;
- narrowly identified game-specific user state.

Exclude by default:

- `steamapps/common/Proton*`;
- `steamapps/common/SteamLinuxRuntime*`;
- `steamapps/compatdata/<appid>`;
- other `compatdata` directories;
- shader caches, download staging, workshop caches, logs, and Steam web caches;
- unrelated Steam games and system/application roots.

Steam download staging remains ignored while active. A completed install should trigger a final scan of the game root only, not a broad filesystem reconciliation.

Add tests proving the candidate set contains game content and saves but not Proton/runtime/bloat paths.

## 5. Phase 1 — direct atomic file materialization

Refactor `noland-restore` so files are written directly to the resolved destination:

1. Resolve and validate the logical root and relative path.
2. Create the destination parent directory.
3. Create a temporary file beside the final destination, for example:

```text
<file>.noland-<restore-id>.partial
```

4. Stream CAS chunks into that file.
5. Flush, verify size and BLAKE3.
6. Apply safe mode metadata.
7. Atomically rename the temporary file to the final path.
8. Record the file as restored.

Do not use `materialized/tree` for ordinary files after this phase. Keep only a small metadata/log directory under the restore workspace.

For directories, create them lazily as required. For symlinks, validate the target against the logical root policy and publish atomically using a temporary symlink plus rename.

## 6. Phase 2 — bounded rollback

The current rollback must not duplicate the complete destination tree.

Before replacing a destination path:

- if the existing file is already identical, do nothing;
- otherwise move or copy only that specific path into a rollback directory;
- preserve metadata needed for restoration;
- use a bounded rollback budget;
- refuse or pause before exceeding the configured budget.

Rollback entries must be indexed by destination path and restore ID. On success, delete the rollback set. On failure/cancellation, restore only paths changed by this restore, then delete rollback artifacts.

If a destination is empty, no rollback copy is needed.

## 7. Phase 3 — chunk reference accounting

Before download, build a restore-local count:

```text
chunk hash → number of not-yet-published files that reference it
```

When a file is atomically published:

- decrement counts for its chunks;
- delete a CAS chunk when its count reaches zero, unless retained by another operation or cache policy;
- never delete chunks required by an unverified partial file.

The reference table should be persisted in the restore journal or a restore-local SQLite table so process restart does not make eviction unsafe.

For an initial implementation, retain chunks until the current pack window is complete, then evict only chunks proven absent from all remaining manifest entries. Add per-file accounting after correctness tests pass.

## 8. Phase 4 — rolling pack processing

Replace full-bundle pack retention with a bounded window:

- download at most `N` packs concurrently;
- process and verify them;
- extract only missing chunks;
- materialize files whose chunks are complete;
- delete successfully processed restore packs when no resume policy requires them;
- continue with the next window.

Initial defaults:

```text
Balanced: 2 pack downloads
Fast:     4 pack downloads
Hard max: 4
```

A pack may be removed only after all of its required chunks are verified and either persisted in CAS or consumed by verified files. Replacement of an invalid pack must be atomic: remove/quarantine the invalid file before downloading its replacement.

## 9. Phase 5 — disk budget and admission control

Calculate a conservative budget before starting:

```text
required free space = rollback budget
                    + active partial-file allowance
                    + bounded pack window
                    + CAS working set
                    + safety margin
```

Do not use total manifest size as temporary-space requirement once streaming restore is implemented.

Persist budget fields in operation progress:

```text
free_bytes_at_start
reserved_bytes
peak_bytes_used
free_bytes_low_watermark
```

Pause or fail clearly when free space falls below the safety threshold. Never continue downloading if the next pack cannot fit.

Recommended initial safety margin: 15–20 GB, configurable per instance.

## 10. Restore milestones and progress

Keep milestone state separate from operation state:

```text
operation state: DOWNLOADING / APPLYING / COMPLETED / FAILED
milestone:       READY_TO_LAUNCH / SOON_PREFETCH
```

`READY_TO_LAUNCH` means launch-critical files are available; it does not mean the restore is complete.

Progress must report:

- packs downloaded and processed;
- chunks extracted and reused;
- files atomically published;
- bytes currently used by restore workspace, CAS, and pack cache;
- current disk free space;
- final error, if any.

The operation may only become `COMPLETED` after all selected files are published, metadata is finalized, cleanup succeeds, and the final validation passes.

## 11. Cleanup policy

### Successful restore

Immediately or after a short retention period:

- delete restore staging metadata;
- delete processed restore packs;
- evict zero-reference CAS chunks;
- retain only configured reusable CAS data.

### Failed or cancelled restore

- stop issuing new work;
- finish or safely abandon in-flight writes;
- remove partial files;
- execute bounded rollback;
- delete restore workspace;
- remove invalid packs;
- retain valid packs only if resume is explicitly supported;
- evict restore-local CAS chunks not needed by another operation.

### Completed upload

After cloud commit is finalized, local upload packs may be deleted according to retention policy. This is independent of restore-pack cleanup.

## 12. Cancellation and failure handling

Check cancellation:

- before each pack;
- before each file;
- between chunk writes;
- before atomic publication;
- before cleanup.

A partial file must never be renamed into place. A failure in one file must prevent final success and trigger rollback/cleanup according to the restore policy.

## 13. Tests

### Unit tests

- direct file materialization verifies hash and size;
- partial files are never treated as complete;
- atomic replacement preserves old content on failure;
- symlink validation remains enforced;
- zero-reference CAS eviction does not remove future chunks;
- invalid pack replacement removes the bad file safely.

### Integration tests

- restore a large file with one pack;
- restore files sharing chunks;
- cancel during download, extraction, and materialization;
- fail with insufficient disk budget before starting;
- fail during final file and verify rollback;
- restart after a completed pack window and resume safely;
- verify `READY_TO_LAUNCH` does not report `COMPLETED`;
- verify successful restore leaves no restore workspace or pack cache.

### Storage regression test

For a representative 23 GB Steam game, assert that peak temporary usage remains within the configured budget and does not scale as:

```text
full pack set + full CAS + full staged tree + destination
```

## 14. Implementation order

```text
1. Narrow Steam finder/indexer scope
2. Add restore disk accounting and progress fields
3. Implement direct atomic file materialization
4. Replace full-tree rollback with per-path rollback
5. Add rolling pack cleanup
6. Add restore-local CAS reference accounting
7. Add admission control and low-disk pause/failure
8. Add cleanup and restart/retry tests
9. Enable automatic cleanup after successful restore
```

Do not delete CAS opportunistically until reference accounting and active-operation protection are in place.

## 15. Success criteria

A restore is considered complete only when:

- the operation state is `COMPLETED`;
- the Helldivers directory exists at the resolved Steam library root;
- all selected files pass size and BLAKE3 validation;
- saves/configuration are present according to the Steam scope policy;
- Proton and unrelated runtime files were not restored by the game bundle;
- restore staging and processed pack cache are removed;
- free disk space remains above the configured safety margin;
- a retry after interruption does not require rebuilding the entire restore workspace.
