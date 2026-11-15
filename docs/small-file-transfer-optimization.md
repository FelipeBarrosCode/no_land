# Small-file upload and download optimization

## Observed workload

Hookshot restore `b4a1cc4f-b023-4c8d-a36e-098ae40d2365` on instance 60708394
completed 186,664 files and 360 packs: 27,867,725,307 ciphertext bytes and
35,868,392,703 published bytes. The pipeline took 633,884 ms; recorded operation
runtime was 654,647 ms and request-to-completion was approximately 678 seconds.

The operation used seven decoders and seven verifiers. Its file-sync stage
accumulated 2,399.412 worker-seconds across 186,664 file flushes. Decoder-to-assembly
queue waits accumulated 1,165.999 worker-seconds; publication waits recorded
231.346 seconds. These counters overlap one another and network work: they are
not additive pieces of wall time. Network waits were substantial too, including
individual ~537 MB packs taking 146–209 seconds.

All 360 packs succeeded on their first attempt, with no watchdog restarts.
Maximum admission wait was 212 microseconds, so this pass targets local
small-file costs rather than the previously repaired permit controller.

## Streaming capture hash reuse

When EOF proves that the entire input is a single FastCDC chunk, the already
computed full-file BLAKE3 digest is also the chunk digest. The generic streaming
capture path now reuses it rather than hashing the same bytes a second time.
Unknown EOF, multiple chunks, callback failures and read failures retain their
existing behavior. Manifest chunk boundaries and hash formats do not change.

## Sealed capture read-ahead

Each cached capture-segment descriptor now has 64 KiB of bounded read-ahead.
Adjacent records do not seek between their headers and payloads. Nonadjacent
references still seek explicitly. Four cached descriptors bound extra read-ahead
memory to 256 KiB per worker, within the existing conservative preparation budget.

Every returned record still validates its bounds, header, payload digest and
descriptor identity. Buffered bytes cannot hide an on-disk mutation or truncation.
Container seals, file/directory syncs, captured-source identities and retry
checkpoint formats are unchanged.

A deterministic 1,024-record fixture requires two raw body reads and one seek,
instead of a header read, payload read and seek for every record. This is a
syscall-count result, not a prediction of live cloud throughput.

## Bounded upload receipt batching and range claims

The existing frozen ranges already bound a capture to 1,024 files and normally
32 MiB. Tiny-file completions now share that range's authenticated receipt rather
than forcing an additional checkpoint every 256 files and an empty terminal
receipt. The 32 MiB payload boundary still publishes large-file prefixes before
EOF. Newly appended chunks must cross the same CAS durability barrier before
their receipt; metadata-only completions may reuse an earlier completed barrier.
Receipt encryption, atomic replacement and directory syncs are unchanged.

A reader's anchored source open now also supplies its initial generation check.
It no longer opens and discards an extra descriptor or clones the held descriptor
just to read the same immutable generation. Post-read descriptor identity and
anchored-path generation checks remain mandatory, including the separate source
check when capturing through a CoW view.

Only range claims are reordered: one existing reader takes expensive ranges
while the other readers drain short ranges. Frozen jobs, ranges, file indices
and receipt names do not change, and no extra threads are admitted. Per-plan
lease checks are shared across chunks from the same lease; each source still
receives its own bounds and authenticated-payload validation. Queue admission
reuses an in-memory size estimate instead of scanning/statting a frozen plan
again on every backpressured scheduler turn. Workers still reject missing or
changed inputs before sealing.

## Rolling download publication on overlap-capable hosts

Previously, publication waited for every file in a submitted verification cohort.
A slow file could hold already verified files and the entire next assembly
window behind it, leaving other verifiers idle.

Overlap-mode restores now publish a ready subset when its time/byte window is
due, capacity is needed, or publication will let an assembly window refill idle
verifiers. Published files release the existing aggregate file/byte reservation;
the assembler can then submit the next bounded window while a slow peer finishes.
Publication still coalesces parent-directory barriers across the ready subset.
Capacity pressure alone cannot flush one tiny file at a time while a cohort is
still queued: publication waits for the byte/time trigger or until a ready
subset can free a worker lane. There are no additional workers or unbounded
queues.

A publication guard pauses new file-sync admissions through all rename,
directory-sync and identity checks. Already-admitted flushes may finish. The
guard resumes waiting workers on both success and failure; cancellation wakes
and joins them before rollback. Staged hosts, including the existing APFS path,
retain whole-cohort publication and do not start a new sync cohort during
publication.

Every file still passes its full digest, mode, inode/path and post-flush identity
checks. Existing durable intents precede partial creation and path mutation;
subset publication neither rewrites those intents nor makes an incomplete
restore committed. Failure/cancellation/crash recovery must still restore the
original destinations.

Added I/O diagnostics are `partial_publication_batches`,
`verification_refill_batches` and `file_sync_publication_wait_us`. The last is
cumulative worker wait, not additional wall time.

## Safety and measurement

Required file-content flushes, directory barriers, authentication, publication
ordering, durable recovery receipts and join-before-cancellation cleanup remain
mandatory. No application CPU/GPU limits are imposed. Local synthetic tests do
not replace an authorized matched before/after cloud transfer; no remote
deployment or live cloud transfer is part of this implementation pass.

The regression fixtures cover:

- Short-read chunk boundaries, reader/callback failure and unchanged digests.
- Bounded capture read-ahead, out-of-order references, corrupt/truncated cached
  containers and recovery after an invalid slice.
- A 1,024-file tiny/empty/duplicate capture, receipt replay without reopening
  completed sources, exact-segment prefixes, changed source rejection and
  cancellation/retry.
- Real short-range capture progress beside blocked large ranges, with the same
  reader count.
- Ready-subset publication/refill before a gated slow verifier finishes,
  publication-time sync pausing, original-mode/bytes preservation, tampering
  rejection and rollback after error/cancellation/crash.
- The existing 4,096-file publication/journal workload in both overlap and staged
  modes on every host, including its directory-barrier coalescing assertion.

## Local validation results

`cargo test --locked --workspace` passed 719 library tests and 12 integration
tests, including 19 new regressions. Six opt-in tests are excluded by default.

The separate `NOLAND_SYNTHETIC_FILES=4096` synthetic storage fixture also passed:
initial capture, unchanged capture, a one-save mutation, fresh restore and
unchanged restore. Every restored file's digest is checked; the unchanged capture
rehashes no files, the save update rehashes only its one file, and unchanged
restore downloads no packs. This fixture uses only an isolated local provider.

The ordinary publication fixture now checks 4,096 files in each scheduling mode.
Both flush all 4,096 files, preserve the intent log and initial checkpoint, check
every published file's bytes, and retain the directory-sync coalescing assertion.
These modes are not a matched before/after speed comparison; Linux overlap
scheduling is intentionally not the default for the APFS development host.

All seven gated rolling-publication tests also passed five consecutive runs.
Production workspace binaries type-check; Rust formatting and diff checks pass.
All-target workspace Clippy retains the same 33 baseline diagnostic locations
with no new warnings.

Validation logs are under `state-agent/target/upload-review/small-files-*`.
Validation was local; no live cloud speedup is claimed.
