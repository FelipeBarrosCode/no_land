# Preview-bound deletion optimization

## Changes

- A rolling window admits at most **four** deletion/control requests. It refills
  on any completion, not the end of a fixed wave. All pack identity/size checks
  finish before any destructive state is published or any object is removed.
- A sealed, executor-created `PrevalidatedDeletion` capability permits rclone to
  skip its redundant existence check. The executor still checks immediate absence
  for every object **and independently checks the entire frozen set again** before
  finalization. Already-absent objects remain valid on a resumed plan.
- Bundle tombstones are durable before unpublishing commit markers. Metadata
  objects are removed once rather than by two overlapping loops.
- Preview selection uses one authenticated inventory instead of loading the
  catalog/manifests separately and repeating them during dependency expansion.
  Confirmation still obtains a fresh inventory and checks its frozen identity.
  Catalog/manifest commit mismatches continue to fail closed.
- Manual deletion sessions have **120-second per-command deadlines**, including
  metadata/control requests. A timeout kills/reaps the local child, reports an
  uncertain cloud outcome and does not immediately launch an overlapping retry.
  The existing provider-wide retry gate still shares rate-limit cooldowns between
  the workers. Long-running backup/restore streams and application runtimes do
  not inherit these maintenance deadlines or any CPU/GPU quotas.

For 115 packs, the ordinary successful pack path changes from **575 to 460 rclone
commands**: preflight stat, exact delete, immediate absence check, independent final
absence check. This excludes metadata, admission, retries and provider-internal API
calls. It is a command-count reduction, not a measured Google Drive speedup.

## Progress and diagnosis

The agent persists phase, validated packs, confirmed-absent objects/packs/bytes,
last confirmed progress time, concurrency and existing storage request counters.
SQLite updates are throttled to twice per second, with phase boundaries forced.
The desktop event projection and deletion dialog consume these snapshots, bound to
the exact preview ID and PC. Reconnect/monitoring-paused events retain confirmed
counters, but only the authoritative confirmation result marks deletion complete.
Verified-absent bytes are **not** a measurement of returned provider storage quota.
Structured native errors retain their actionable details instead of rendering
`[object Object]`; an uncertain timeout never becomes a success notification.

`get_storage_operation_status` and desktop MCP `storage_operation_status` inspect
an existing operation using `GetOperationStatus`'s small progress projection. They
do not acquire the destructive instance action lock, install tooling, deploy an
agent, mint a cloud session, start or retry work. The MCP tool remains scoped to a
registered, explicitly granted instance and needs no shell/control permission.

## Failure safety

The rolling window stops admitting work on the first observed error and drains
every admitted request before returning and releasing repository access. No detached
tasks outlive the provider/session. Cancellation retains the existing fail-closed
recovery protocol; killing a process does not prove a server-side request settled.

Frozen dependent-backup consent, durable plans and tombstones, retired-pack cache
invalidation, pending-deletion fences, exclusive repository admission, permanent
provider deletion flags, empty-directory checks and authenticated completion
receipts remain mandatory. There is no prefix purge, wildcard deletion, catalog-only
"completion", silent trash fallback, or reuse of a preview as confirmation-time
evidence. Unrelated apps and on-PC files remain outside cloud-deletion scope.

Native bulk-delete APIs and a persistent rclone process are deferred: this change
keeps exact per-object results and the existing recovery format rather than adding
an unverified batch protocol. Actual backend consistency and settlement of interrupted
requests still require separately authorized live acceptance.

## Isolation

Implementation and acceptance checks used isolated desktop and Android/iOS source
checkouts. Original source worktrees and their staging areas were preserved. A native
desktop development instance was subsequently built and opened at the user's request;
no remote agent redeployment, storage-operation restart, or additional live deletion
was performed. Profile files and credentials were not manually inspected or copied.
Follow-up PR branches consolidate outstanding source work without discarding those
worktrees. The requested Vite frontend runs on
`http://127.0.0.1:1420/`; native cloud actions still require the Tauri backend.

## Validation and disk budget

Accepted local verification:

- **383 host tests**: agent 136, storage 79, restore 63, snapshot 7, database 31,
  core 36 and discovery 31. This includes the 115-pack bounded-concurrency fixture,
  no-delete-before-preflight barrier, exact-once metadata removal, final reappearance
  detection, uncertain-delete recovery and timeout child reaping.
- **28 source-linked client unit tests**, **397 full script tests**, and **22 isolated
  browser assertions** exercising the real deletion dialog with a mocked Tauri
  boundary. No browser test contacted a native backend or cloud provider.
- **Three real-rclone local fixtures** passed: physical payload/directory removal,
  permanent-delete flag parsing without remote IO, and the one-command exact-delete
  path with already-absent retry and unrelated data preservation.
- TypeScript and deletion-policy/locale guards passed for desktop, Android and iOS.
  Desktop and Android client `cargo check --locked --tests` passed. Shared deletion
  source is identical across the three isolated checkouts.

The iOS Rust check was **stopped by the disk budget**, not counted as passing. The
separate mobile-agent rebuilds and native device/release packages remain pending.
The full desktop native test executable requires the Moonlight native libraries;
the focused client unit harness and host type check are not native-runtime tests.
A subsequent full macOS desktop development build passed with the real native
libraries, and the app was launched against Vite. This is not release acceptance.
No live Drive deletion, provider-consistency acceptance or cloud speed benchmark
was performed by this optimization work.

Android/iOS build caches and `dist` outputs were removed at the user's request,
recovering **13.35 GiB**, and were not recreated. Subsequent checks ran sequentially,
reused retained test executables or the existing shared desktop cache, disabled
incremental compilation, used one Cargo build job and monitored free disk space.
The resumed verification preserved more than **16 GiB free**. Earlier disk-exhausted
logs are superseded by the accepted resumed logs; stopped/failed runs are not passes.
Original source worktrees, signing inputs, backups and validation evidence were
preserved. The follow-up consolidation PR publishes source only, not a live-agent
deployment or release.
