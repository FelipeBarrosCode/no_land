# Shared Storage I/O validation

The optimization target is end-to-end capture/restore time with the same verified
content and recovery guarantees. Network transfer time, CPU work, and disk waits
overlap; their cumulative counters must not be added to obtain wall time.

## Full deletion and one-current-bundle policy (API 22)

User-selected policy: full cloud deletion includes every dependent backup sharing
the selected packs. Confirmation lists the transitive closure (including committed
bundles not present in the latest catalog), physical object bytes and pack count.
Dependent backups require a separate checkbox acknowledgement. Changed inventory,
missing/corrupt authenticated indexes and conflicting storage activity fail closed.

Deletion persists an authenticated frozen plan, retires pack-cache hints, removes
bundle metadata/payloads and empty bundle directories, verifies object absence,
then writes a completion receipt. Drive bypasses trash for permanent exact-object
deletion. Pending work is offered as a resumable deletion entry; a retry cannot
expand its original scope. Small maintenance/deletion receipts remain, but do not
contain application payloads. Provider-side retention/versioning may delay quota
reclamation independently of active-object removal.
Backblaze B2 uses `--b2-hard-delete` and OneDrive uses `--onedrive-hard-delete`;
the installed rclone is preflighted before destructive effects. Unsupported
permanent-delete flags require a tooling update, not a silent trash/hide fallback.

Complete backups now converge to one current app bundle after the replacement's
manifest AND catalog publication. Old ancestor metadata is removed, and packs not
referenced by surviving bundles are deleted. Live/source games are not removed.
Newer cloud heads and divergent histories are preserved with a conflict rather
than overwritten. The restore list shows the current complete head, not historical
generation choices. Legacy metadata-only deletion cannot retroactively infer a
lost pack index; it must not guess at unrelated orphan packs.
Retained generations requiring explicit resolution appear in a separate,
delete-only section; they cannot be selected for restore.
Final seal validation and publication hold repository admission and reject a
clean-app commit deleted between transfers, rather than authorize automatic
destruction against a stale local commit record.

API 22 adds repository-wide admission for managed transfers/deletion. Update all
clients/agents before using destructive maintenance and keep external/legacy clients
offline. Claims have no unsafe expiry; foreign abandoned claims require verified
owner recovery. No PC was registered at initial local sign-off; the later read-only
Sync validation below records a subsequently connected target. Live-provider
destructive acceptance still requires authorization. No existing cloud backup has
been deleted during this task.

Claims are execution-unique (including simultaneous retries of the same plan).
An agent-root OS lock prevents clearing an active worker's claim and recovers
abandoned owner previews without relying on a DB row or timeout. Admission checks
pending deletion again after obtaining exclusivity. Rclone inventory is structured
JSON: truncated output, duplicate names, unsafe keys and unknown object sizes fail
closed, with catalogued commits as an additional completeness cross-check. The
coordination contract requires complete read-after-write visibility; arbitrary
eventually consistent mirrors/caches are not supported for destructive maintenance.
These local tests do not certify a live cloud provider's consistency contract.
Before destructive production deployment, verify cross-PC visibility and safe
settlement of interrupted provider requests. An OS lock or a reaped rclone process
alone cannot prove that a server-side upload has settled. Legacy/unfenced writers
and unverified backend coordination remain outside the tested safety envelope.

Local checks passed: desktop host suites; ten client handoff tests; a real rclone
1.76.0 local-provider deletion fixture (pack bytes and empty bundle folder removed,
unrelated app ciphertext still decodes); and twelve isolated mocked-browser
assertions covering dependent consent, cancellation, frozen PC identity, rejection
of metadata-only previews, direct current-head restore selection and delete-only
recovery/retained entries. The browser used a fresh profile, and the rclone fixture
used only its own local configuration, never production credentials. The three
final host suites passed: **374 desktop / 373 Android / 373 iOS** tests across agent,
storage, restore, snapshot, DB, core and discovery. Both explicit rclone CLI fixtures
passed (physical local-payload deletion and permanent-delete flag preflight). The
desktop shared-storage client suite passed 58 tests. Mobile client host compilation,
all three frontend builds and all nine locales per client passed. Three scale
fixtures remain opt-in. At that local sign-off, native Linux deployment, live
multi-PC/provider destructive acceptance, and Android/iOS device packaging remained
pending; API 22 had not been deployed and no real cloud backup was deleted. See the
later live Sync result below. Source changes are now carried by the consolidated
review branches; publishing source does not establish destructive acceptance.

The API 21 evidence below is historical and its pack-retention limitation is
superseded by this follow-up.

## Live Sync prerequisite repair (2026-11-13)

The Sync file tree failed on PC **60541218** because Ubuntu's installed rclone
**1.53.3** rejected `lsjson --stat`. The agent was already healthy on API 22 and
matched the current source fingerprint; this was a dependency failure, not a
missing catalog or a reason to change repository data.

Desktop, Android and iOS now require rclone >= **1.76.0** plus the actual listing
flags before minting an agent storage session. Missing/incompatible installations
use the pinned official x86-64/ARM64 archive, verify its hardcoded SHA-256 before
extraction/execution, and atomically publish to `/usr/local/bin/rclone`. The distro
binary remains in `/usr/bin`. Installation is serialized, checks for active storage
operations/transfers before and after download, and never stops processes or
services. Sudo passwords are sent only through stdin, not logged command arguments.

The live application successfully loaded the cloud catalog after repair, including
the existing **hookshot** and **HELLDIVERS 2** bundles. Rclone reported **1.76.0**;
the agent stayed active with the same PID (**10807**) and zero active operations.
No restore, backup, cloud deletion, instance provisioning, or agent restart was
requested by this repair. This read-only result supersedes the earlier deployment
status, but does not certify destructive maintenance or full restore/launch.

The shared offline suite passed **27 Python cases** and **4 Node guards** against
each client's actual installer/command template. Desktop's complete script suite
passed **390 tests**. The source is embedded by `include_str!`, so mobile packaging
does not need a new resource-path override. Native device packaging remains separate.
Android/iOS host `cargo check --locked --tests` passed with native packaging
disabled; the desktop development app compiled and exercised the actual SSH path.

## Explicit delete actions (API 21, historical)

### Helldivers 2 follow-up: Steam aliases and Proton links

The first real backup on PC 60434119 failed in reconciliation, before any upload,
because discovery canonicalized Steam libraries but left `.steam/steam` in the
Steam root/userdata mapping. The discovered root and userdata mapping now share
the canonical host directory. No-follow scan/capture protections remain enabled.

The retry then exposed Proton's `dosdevices/c: -> ../drive_c` link being rejected
by the blanket `..` link-text policy. Backup and restore now share one validator:
leading parent components are allowed only within the link's logical root;
parent components after named target components, root escapes and `/proc`, `/sys`
and `/dev` targets remain rejected. Link targets are not followed during capture
or publication. A fixture checks selected-game isolation, game/shader/save hashes,
and restoration of the C-drive and Local Settings links with a remapped prefix.
Canonical temporary test roots avoid macOS `/var` aliases in discovery assertions.

The real prefix also contains generated serial/block-device links such as
`dosdevices/com1 -> /dev/ttyS0` and `dosdevices/d:: -> /dev/vdb`. These host-specific
bindings are excluded only inside recognized Proton/Wine/Bottles prefixes and
counted as `excluded_host_device_links` in operation/manifest diagnostics. They
are never followed, deleted or restored onto another PC; ordinary saves/config,
the C-drive mapping and application links remain included. Device links outside
those exact runtime namespaces still fail the normal safety validation.

The follow-up keeps API 21 and uses the existing source-fingerprint deployment
check. No user game/save data is modified by deployment.

Final follow-up validation passed 134 agent, 31 discovery, 36 core and 63 restore
tests on native Linux (264 total). Host suites passed 133 desktop / 132 Android /
132 iOS agent tests, plus 31 discovery, 36 core and 63 restore tests per repository.
Three scale fixtures per full matrix remain opt-in. All eight changed source/test
paths match across the three repositories, with platform-specific code preserved.

The fixed build was installed on **60434119** at **2026-11-13T03:51:35Z**, after
proving zero active/unfinished operations (job `caed58b0-c027-454f-accd-0807c550548d`).
API 21, the eBPF observer and unlimited service CPU/memory settings were verified.

- Current source: `/opt/noland/state-agent-1794541791`
- Agent SHA256: `ed22b84f21f4ca1673c7ad3a968e4861d0779031e31301f1e28e5028539b7611`
- Observer SHA256: `bcc50351bf9aed2adbc348b1700889d014cc17db04e287a65d50f1aecceee0b3`
- Source fingerprint: `508a08acdc6a97e41903c2be93481ab0f15d04671ea0ab61c201fa337957bbf7`
- Rollback: `/opt/noland/helldivers-alias-fix-1794540818/rollback-v4`
- Local evidence: `state-agent/target/helldivers-alias-fix-1794540818/`

Real retry operation `b6532750-1277-426a-b542-2c30734d1b55` (client job
`f617312f-ad8e-40b3-8445-6b0d956f603f`) passed reconciliation/planning and entered
V3 capture/upload: 2,120 selected entries, 27,481,551,093 logical source bytes,
33 excluded host-device links and zero excluded game caches. The first live
acceptance observation showed 4,187,494,713 captured bytes and five confirmed
cloud packs with no error. The backup **completed at 2026-11-13T03:58:01Z**,
with all 115 packs confirmed, 26,564,672,529 encrypted pack bytes transferred,
zero failed/retrying/pending items and no error. Operation wall time was 362,325 ms
(about 6m02s); no comparable-baseline speedup claim is made. The desktop storage
service independently logged success at 03:58:07Z.

`VerifyBackupCommit` confirmed bundle `3315229f-f91e-4c83-8b51-95483c2fe966` and
commit `b9c87a28-0427-454b-9c81-95d000b6c1cf` as the latest committed Helldivers 2
backup. Final health showed zero active/unfinished operations, an active observer
with zero reported losses, and the expected running-binary hash. The real cloud
upload acceptance is now complete; a real-game restore/launch/save-loading run
remains pending and is not implied by the local fixture round trip.

The backup and restore selectors now have separate **Delete from PC** and
**Delete cloud backups** actions. A server-issued ten-minute preview freezes
the exact target, selection and local source identities before confirmation.
The same plan ID is also the monitored operation ID, so an ambiguous submission
can be replayed safely. Neither transfer path automatically removes its source.

PC deletion removes only confirmed, exclusively owned leaf files under the
configured home; package archives, pre-sync copies, system/agent state and shared
paths are protected. Active selected applications and overlapping transfers block
deletion. Local quarantine and parent-descriptor checks prevent path redirection;
changed files fail closed. Empty directories/launcher registrations may remain.

Cloud deletion removes selected backup metadata and writes authenticated deletion
receipts that survive stale catalog pointers and older catalog writers. Restore,
publication and cached unchanged-parent reuse honor those receipts. Shared encrypted
packs remain stored: this action **does not reclaim their cloud storage space**.
Repository-wide garbage collection across multiple PCs is still pending.

Desktop validation: 131 agent tests, 60 storage tests, 7 snapshot tests and 63
restore tests passed; 3 scale fixtures remain opt-in. The 57 Shared Storage client
tests passed, together with frontend build/type checking and all nine locale bundles
(1,457 desktop keys). New regressions cover frozen selections, duplicate confirmation,
recreated files, changed parent/leaf identity, shared ownership, protected files,
authenticated deletion receipts, stale-catalog recovery and backup after deletion.
Native Linux validation passed 132 agent, 60 storage, 7 snapshot and 63 restore
tests (262 total; 3 opt-in fixtures ignored), run as the desktop user. Android and
iOS each passed 130 agent, 60 storage, 7 snapshot and 63 restore tests (260 total),
host Rust checks with native packaging disabled, frontend builds and nine-locale
validation (1,380 Android / 1,398 iOS keys). Mobile native packages/device runs
remain unverified. Platform adapters preserve mobile `lib.rs` registration and
agent-side transfer admission rather than the desktop-only lifecycle guard.

An isolated-browser fixture with mocked Tauri calls verified disabled empty/group
selections, preview before confirmation, cancellation without deletion, cloud-pack
retention disclosure, and confirmation bound to the previewed PC after the selected
PC changes. No production deletion was executed.

Validated deployment to registered PC **60434119** completed
**2026-11-13T03:16:04Z** (job `85f7fade-dbcb-4be3-8f26-c35dc54a492f`).
The final preflight and post-install health checks showed zero active/unfinished
storage operations, API 21 and an active libbpf observer. The running process hash
was independently checked through `/proc/107243/exe`; invalid-target and missing-
confirmation RPC smoke checks passed (job `b70ed9f3-c93e-44b2-bd69-af08c99ebf9f`).

- Source workspace: `/opt/noland/state-agent-1794538659`
- Rollback: `/opt/noland/delete-deploy-1794538659/rollback`
- Agent SHA256: `149b6c6dbb967e109a1fed18d6edb85f967bf899bd42e0df32f67e75d785e0a0`
- Observer SHA256: `bcc50351bf9aed2adbc348b1700889d014cc17db04e287a65d50f1aecceee0b3`
- Source fingerprint: `517855a0639f1b176a2fb2cd67f4c85e1aa4be4429f2088558f3a4eda42c65f9`

Only the state-agent service was restarted. Its observed CPU quota and memory limit
remain unlimited; runtime application services were not changed. Local validation
artifacts are under `state-agent/target/delete-agent-deploy-1794538659/`.

## Streaming upload pipeline (introduced in API 20)

New desktop, Android and iOS backup requests select applications without
performance profiles or shader exclusions. Complete reconciliation includes
app-owned caches, installation files, saves, configuration and mods. Steam adds
only the chosen app's shadercache/compatdata/userdata namespaces, preserving
isolation from other games and shared launcher infrastructure. A scope-version
marker forces rediscovery when upgrading an older, narrower parent backup.

The V3 protocol freezes selected file identities before side effects. A bounded
reader pool feeds ordered FastCDC/full-file hashes and immutable 32 MiB capture
segments. Atomic snapshots and descriptor-based CoW remain preferred; unsupported
filesystems use a fused read/hash/capture pass. Each handoff seals/fsyncs data and
atomically publishes an authenticated receipt with directory durability barriers.
File completion records require the original source identity, size and full hash;
partial-file uploads can never publish a committed application manifest.

One operation-wide scheduler builds packs across file boundaries, persists their
membership before sealing, and refills independent upload slots on completion.
A single codec worker can feed multiple uploads. Small saves retain compact packs;
other files share regular packs. The startup target is at most 64 MiB, steady-state
target 512 MiB, with a one-second partial-builder flush and existing 1 GiB format
maximum. Provider byte limits can reduce targets. FastCDC, Zstd level 1, encryption
and remote formats are unchanged. Existing watchdog/application-owned retries stay
in effect.

Recovery authenticates existing receipts before remote effects, preserves sealed
pack IDs/ciphertext, replays durable partial-file bytes through the same chunker,
then reads the remaining suffix only if the source generation still matches.
Fully captured files can resume after the live save changes. Missing/corrupt
metadata, wrong repositories/keys and changed partial sources fail closed. Worker
and transfer ownership ensures producers are joined before capture cleanup.

Resource limits separately account for reader/codec workspaces, capture handoffs,
sealing/ready/active ciphertext and retained retry data. Durable staging is **not**
bounded by the in-memory queue: worst-case capture plus ciphertext remains admitted
on disk until a sealed operation checkpoint replaces the streaming checkpoint.
Per-writer disk reservations enforce free-space headroom. Automatic preparation
and transfer budgets each use half the available-memory estimate. Runtime games
retain unrestricted CPU/GPU access; these controls only budget backup workers.
Unknown memory uses a conservative fallback, and resume can lower saved preparation
worker counts when current resources or active sessions require it. Transient CoW
views live under the operation-owned capture tree, including crash leftovers.

Progress reports selected files/source bytes, captured/reused bytes, prepared
ciphertext, actual uploaded payload, confirmed objects, active workers and the
current wait reason. Partial pack totals are never shown as the whole workload or
used to display a misleading completion percentage. Diagnostics include first
upload latency, slot starvation, capture backpressure and peak reserved/upload
usage. Cumulative worker time is not elapsed wall time.
The final snapshot clears live worker/queue counts through manifest commit, and
completed-operation details retain the streaming diagnostics after live progress
is cleared. First-upload latency is measured from the streaming pipeline's start
to upload-task admission, not a verified network first byte. Upload gauges count
reservations, not simultaneously sending sockets. The streaming
`hashing_duration_ms` includes pipeline waits; overlapping phase counters must not
be summed. Discovery and reconciliation have separate timing counters.

Regression fixtures cover >2 GiB pre-EOF upload, a slow first upload with one
packer/two slots, durable-handoff cancellation, partial-source changes, receipt and
ciphertext corruption, exact decoded file hashes, immutable retry reuse, legacy
V1/V2 checkpoints, cache scope and restored saves. Cloud throughput remains to be
measured on the same PC/provider/application with cold and warm runs. Compare
`NOLAND_STREAM_PACK_MIB=128|256|512`, resource-admitted 2/4 readers and 4/8 uploads;
record first-upload latency, total wall time, source throughput, network payload,
provider finalization, CPU/disk pressure and restore verification. No measured
speedup multiplier is claimed from synthetic correctness fixtures.

### Upload utilization follow-up (2026-11-14)

The desktop PR branch now preserves preparation lookahead inside the existing
operation-wide byte budget. New pack targets use at most
`budget / (upload_slots + 1)`, leaving room for the next codec input instead of
letting uploads consume the entire window. Admission accounts for indivisible
maximum-size chunks, not just nominal targets. Tiny budgets cannot guarantee
overlap, and oversized frozen
packs may drain alone: retries never change an existing pack ID or ciphertext to
adopt a new target. Already-sealed compressed packs use their actual byte size for
admission; an unsealed frozen plan over the absolute budget still fails explicitly.
Once capture/planning/sealing finishes, the sealed-only tail can spend the soft
lookahead too, while retaining the absolute object/byte bounds. Frozen packs that
fit together do not unnecessarily drain one at a time.

Only already-ready small ciphertext packs are grouped (up to 64 MiB and a bounded
share of object slots); a first pack never waits to form a batch. Each object
reserves capacity. Durable member receipts release shared bytes and surplus
object slots immediately while the child's final process slot remains occupied
until exit. Weighted backend admission can grant the currently free width rather
than waiting for a group's entire requested width; rclone's transfer/checker
arguments use that actual grant.

Authenticated plan persistence runs through one bounded, owned writer, so the
coordinator can continue driving uploads during plan fsync. Codecs cannot consume
a plan until its receipt is durable. Cancellation closes and joins the writer
before capture cleanup; no fire-and-forget persistence was introduced. Reuse
lookups prefetch and consume matching windows of at most 256 hashes while keeping
the 4,096-entry authenticated, repository/key-bound positive/negative cache.

Preparation is proportional to resources rather than a fixed reader/codec pair.
The automatic upload/download policy below supersedes the old four-reader,
12-preparation-worker and eight-upload caps. For 19 CPUs with sufficient memory,
active Automatic uploads admit ten readers and nine codecs, even with a game
session; memory or I/O pressure can lower admission. The operation-scoped gameplay
bypass is described below. Overrides only lower admission. An old session is ignored
only with positive absence evidence for root/tracked processes and an empty
cgroup-v2 subtree; live children and uncertain process identities remain protected.
The tracked-PID lookup uses a covering index. No runtime CPU/GPU quota or affinity
setting is changed.

Noncritical diagnostic ticks are coalesced into operation-wide SQLite batches.
Phase/retry boundaries flush immediately, and completion receipts are still
persisted before capacity release. Failed diagnostic samples are discarded rather
than blocking cancellation cleanup or poisoning another group's accounting.
Additional credential-free counters separate capture durability, channel waits,
plan persistence, codec admission, ready queues, grouping and completion-to-refill
waits. They do not measure NIC traffic or prove provider throughput.

Regressions include incompressible input with exact byte-budget saturation,
codec progress while every upload slot is held, upload completion during blocked
plan persistence, ready-group member refill before straggler exit, frozen retry
and corruption rejection, conservative session liveness, bounded batch lookups,
partial backend grants and diagnostic-write failure during cancellation.
At the upload-only validation checkpoint the app was stopped and changes were
not deployed. A matched
same-provider cold/warm throughput comparison on an explicitly authorized instance
is still required before claiming a measured speedup.

The earlier upload-only verification passed **577 library tests, with six opt-in tests ignored**,
plus **10 testkit integration tests** and **10 JavaScript contract/progress tests**.
Production workspace binaries type-check; workspace formatting and diff checks
pass. On macOS, use a canonical `TMPDIR` to avoid the `/var` symlink alias in
anchored-path fixtures. An existing traversal fixture was corrected to actually
escape its logical root; production symlink validation was not weakened.
Clippy completed with existing baseline warnings and no diagnostics overlapping
the changed/new source lines; this is not a clean strict-warning baseline.

### Automatic upload and download ceilings

Both directions now use the same `WorkerResources` model. CPU detection accounts
for OS affinity/quota. Raw available memory comes from host availability and the
tightest visible Linux cgroup-v1/v2 ancestor (including nested/delegated mounts),
macOS free/inactive/speculative pages, or Windows available physical memory.
Unknown memory retains a finite 512 MiB fallback rather than assuming all RAM is
free. Resource detection never reads application state or credentials.

Each workspace/buffer admission class uses one quarter of raw available memory.
Throughput CPU admission uses the effective CPU count and Balanced uses half.
Normal GameplaySafe uses a quarter outside transfers, but active uploads and
downloads bypass it as described below. I/O pressure halves the resulting ceiling.
Memory costs and a high 128-worker emergency guard further bound CPU pools.
Backup readers and compressors share one admitted total, preserve the minimum
reader/codec pair, and are bounded by actual jobs/ranges. Saved counts and optional
benchmark overrides remain lowering-only upper bounds; large saved counts are
re-admitted before allocating channels or threads.
Resumed captures retain the current requested performance mode: a Balanced
checkpoint with 32 readers/32 codecs on 128 CPUs re-admits at 16/16 on 64 CPUs,
or less with memory or I/O pressure, without changing frozen ciphertext IDs.

Transfer admission additionally respects unchanged provider limits: Drive and
OneDrive allow at most 16, object-storage providers at most 32, and smaller
providers retain their existing limits. The logical staged-ciphertext byte window
is derived from available memory and transfer count, not a fixed profile-sized
1/2/4 GiB window. It is still distinct from process RSS and disk-retained retry data.
Explicit serialized tuning defaults remain compatible; fresh automatic requests
resolve the resource model instead of treating those defaults as their ceiling.
For example, 64 CPUs and 64 GiB available memory admit 32 readers/32 codecs and
up to 32 object-storage transfers (16 for Drive/OneDrive) in active throughput mode.
Those are ceilings, not a claim that every worker or socket stays active.

Restore admission accounts for all active decoders, verification buffers/stacks,
the assembler and queued authenticated records as one CPU/memory working set.
Provider slots and explicit pipeline options remain upper bounds. The old four
default workers and hidden 16-verifier cap no longer prevent larger hosts from
using parallel decoding and readback. Publication windows, checksummed journals,
hash verification, rollback and joined cancellation retain their existing bounds.
Low-resource cases expose their minimum-pipeline-floor exception explicitly.
Final execution independently enforces provider slots and the requested workload,
normalizing GameplaySafe to operation-local Throughput and respecting pressure
changes during manifest preparation. Preflight limits the
verification pool to eligible files; diagnostics distinguish that actual pool
width from the resource ceiling and the observed busy-worker peak.

Generic download workers no longer divide their object ceiling by rclone's range
stream count. Rclone copy-to downloads instead acquire weighted slots and set
their actual range-stream width from the grant. Ordered pack streams acquire one
slot from that same controller, preventing the two modes from multiplying the
provider budget. Preferred copy-to width scales approximately with the square
root of the budget; a partial final grant still uses spare slots.

Transfer concurrency adapts below its admitted ceiling: provider errors reduce
the remote target, while local I/O pressure applies a noncompounding half-ceiling
cap. Fresh healthy observations spanning five quiet seconds remove that cap
without requiring pack completion or undoing remote backoff. FIFO weighted
admission prevents new arrivals from overtaking waiting packs. Recovery never
exceeds the resource/provider ceiling and does not kill active children.
Slow local progress/journal callbacks are excluded from download network feedback.
Ceilings are resolved per operation/resume; running CPU pools are not live-resized.
No application CPU/GPU quotas, affinities or cgroup limits are imposed.

Numeric diagnostics distinguish requested ceilings, effective limits, occupied
slots and actual worker peaks. These are not simultaneous-socket measurements.
Fixtures cover high-compute/resource-poor profiles, nested memory limits, provider
caps, parallel generic downloads, multi-decoder/readback overlap, pressure
hysteresis, recovery, cancellation and exact authenticated restore. A real matched
provider benchmark is still required before claiming a throughput improvement.

Local automatic-ceiling validation passed **656 library tests** (six opt-in tests
ignored), **10 testkit integration tests**, and **10 JavaScript contract/progress
tests**. Nested child-process test output is not double-counted. Production
workspace binaries type-check, and formatting/diff checks pass. Concurrency
fixtures exercise 12 generic downloads, five authenticated pack decoders and 24
readback verifiers, rather than checking displayed worker counts alone.
Normal workspace/all-target Clippy completes with 33 existing diagnostic locations
and no primary diagnostic spans overlapping changed or new source lines. The
repository is not a strict warning-free baseline.

The pack-locality fixture now controls only its own partial-builder age, retaining
its exact one-asset-pack/one-save-pack assertions under full-suite load. A separate
boundary test preserves the real one-second flush deadline and immediate EOF
flush; production timing and checkpoint/pack formats are unchanged.

This automatic-ceiling pass modifies desktop sources only and remains local and
uncommitted. At that validation checkpoint the native application was stopped.
No instance provisioning, agent
restart, remote deployment or new cloud benchmark was performed in this pass.

### GameplaySafe bypass during uploads and downloads

Active storage operations now resolve `GameplaySafe` to `Throughput` with
`TransferWorkload::for_active_transfer()`. Explicit Balanced backup requests remain
Balanced. The bypass applies to upload preparation (readers/codecs), upload network
tuning, download tuning, restore constructors and final restore admission, so a
saved/manual GameplaySafe value cannot silently restore the quarter-CPU policy.
Explicit worker-count upper bounds and frozen replay limits remain lowering-only.

This is an operation-local value, not a mutable global or persisted setting.
Normal session classification and GameplaySafe resource rules are unchanged.
When a transfer finishes, fails or is cancelled, normal protection is already
intact; there is no cleanup flag to forget or a first-finishing transfer that can
throttle an overlapping transfer. Diagnostics distinguish baseline and effective
workloads.

On a 30-CPU host with 20 GiB available memory and no I/O pressure, normal
GameplaySafe still admits eight CPU slots outside transfers. An active Automatic
upload admits 15 readers and 15 codecs; a restore has a 30-slot aggregate CPU
budget instead of eight. Network ceilings remain provider-bounded: up to 30 for
S3-like providers, 16 for Drive/OneDrive and eight for smaller providers. Actual
decoder/transfer occupancy is still limited by work, memory and provider capacity.
Memory pressure, I/O backpressure, retry pacing, authenticated verification,
durability and application CPU/GPU freedom are not disabled.

Regression coverage includes the 30-CPU profile, explicit Balanced requests,
provider/memory/pressure bounds, raw restore options, frozen ceilings, and normal
session protection before/during/after success, failure, cancellation and
overlapping transfers. This follow-up changes code only; it does not deploy or
restart the remote agent or start a new transfer.
Local follow-up validation passed **665 library tests** (six opt-in tests ignored),
**10 integration tests**, **10 JavaScript contract/progress tests**, production
binary checks, formatting and diff checks. Normal all-target Clippy completed
with the same 33 baseline diagnostic locations and no diagnostics overlapping
changed/new source lines.

### Deletion-preview RPC readiness repair

A reported cloud-deletion preview failed with a truncated RPC response while the
agent service restarted. Read-only follow-up found a healthy API 22 agent, a
complete health response, no active/unfinished operations, and a trusted source
marker matching the checkout. This does not prove that any cloud object was deleted.

The client updater previously returned as soon as it saw an API-compatible agent,
even when that was the old daemon and its detached replacement was still building.
The next preview could therefore lose its RPC connection when installation
eventually restarted the service. Readiness now requires the specific bootstrap's
successful exit receipt before checking replacement health.

Per-endpoint read/write admission permits concurrent RPCs but makes deployments
wait for complete replies and prevents later RPCs from entering during an update.
Waiting updaters recheck the installed version after gaining exclusivity. Numeric
UUID workspace names avoid same-second collisions; setup creates its own directory
without removing an existing workspace. A changed source snapshot is rejected
before upload rather than stamped with a fingerprint from different bytes.

Empty replies explain that the agent may have restarted. Partial frames, mismatched
request IDs and invalid JSON still fail closed. Confirmed deletion is never
automatically repeated with a new plan or treated as definitely rejected merely
because its reply was lost.

Validation passed **66 native Shared Storage tests**, including **14 agent-runtime
tests**, and **21 JavaScript contract/progress/deletion checks**. Rust formatting
and diff checks pass. The local development app reloaded the client fix. These
checks did not confirm or execute cloud deletion; reopening and accepting a fresh
preview remains a human-controlled step.

### Adaptive upload and download controller follow-up

Read-only diagnostics for completed upload
`39bfcf7c-249d-4a02-84c0-d91adfe925b6` on instance 60691161 established the
controller bottleneck before this follow-up: 98 packs totaling 25,596,732,844 bytes
completed in 1,073,975 ms, with 1,004,486 ms attributed to upload. Although the
backend ceiling was 16, 71 pack completions sampled an effective limit of one.
One pack waited 934.818 seconds for admission before spending 4.499 seconds in
the transferring phase. Thirteen slow-peer restarts and one idle restart sent
896 MiB of extra payload. Aggregate limiter wait was 11,953.826 worker-seconds,
not wall time; overlapping worker durations must not be summed into a timeline.

The shared controller and rclone supervision now apply the following in both
directions:

- **Fair weighted admission:** first-polled requests reserve capacity in FIFO
  order, including before a woken task runs. A group gets the smaller of its
  requested width and available capacity rather than waiting to fill a group.
  Cancelling head, middle or reserved-but-unpolled requests releases ownership
  and wakes successors. Completed upload members still release capacity early,
  retaining one process slot until the child exits.
- **Separate local and remote limits:** pressure caps new admission at
  `max(minimum, ceil(ceiling / 2))`; repeated pressure cannot halve that result
  again. The effective limit is the smaller of this cap and the remote target.
  A false pressure observation starts a five-second quiet period, and a later
  fresh false observation removes the cap. Stale pressure never expires just
  because time passed. Clearing pressure does not undo rate-limit/backoff
  penalties. Real rate limiting halves the remote target; other retryable remote
  errors subtract one. Four successful saturated attempts after the remote
  cooldown recover one slot, never exceeding the provider/resource ceiling.
- **No historical-rate throttling:** different pack sizes, concurrency regimes
  and group widths cannot lower global admission merely because individual
  per-pack rates differ. Temporary local-pressure recovery does not wait for
  four large packs to finish.
- **Context-aware isolated replays:** healthy peer history is bounded to 16
  entries and 120 seconds, with exact generation/concurrency/width matching.
  Slow-peer decisions require two complete consecutive network-time windows,
  independent live reserved slots and three credible healthy samples. Pressure,
  concurrency changes and meaningful local consumer backpressure discard mixed
  samples. A solo flow cannot be speculatively restarted against past full-link
  speeds. Watchdog/straggler replays do not also reduce global admission as if
  they were provider-wide errors; actual provider failures still do.
- **Recovery while blocked:** upload progress ticks, copy-to downloads without
  observers, and ordered download waits (including blocked consumers) keep
  sampling pressure. Download kernel probes are interval-cached rather than
  reading `/proc/pressure/io` for every 256 KiB decoder handoff. Observer/context
  work is excluded from measured download network time.
- **Honest diagnostics:** defaulted numeric fields now include queued admission
  requests and controller generation, alongside effective limits and reserved
  slots. These are admission samples, not simultaneous sending/socket counts.

Provider/memory ceilings, source-generation checks, immutable ciphertext IDs,
authenticated receipts, retry bounds, child reaping, publication barriers and
deletion safeguards are unchanged. No application CPU/GPU limits are imposed.
No deployment, remote restart or new transfer was performed for this follow-up,
and a matched live upload/download speedup remains unmeasured.

Local validation passed **700 library tests** (six opt-in tests ignored),
**12 integration tests** (including two provider-compatibility tests), and
**21 JavaScript safety/progress checks**. Production workspace binaries and the
desktop client/test sources type-check; Rust formatting and diff checks pass.
All-target Clippy retains the same 33 baseline diagnostic locations, with none
in the four controller/integration source files changed by this follow-up.
A repeat exposed a macOS fixture-launch
race in the deletion deadline test; test-only scripts now run through the system
shell, and the complete workspace suite passed again without changing production
deletion behavior. The separate desktop test rebuild with
`NOLAND_SKIP_NATIVE_BUILD=1` could not link the omitted Moonlight native symbols;
it did not execute client tests. This does not replace the earlier native
readiness validation or establish a native desktop packaging result.

### API 20 validation results

All checks below ran locally on macOS against the three source trees:

| Suite | Desktop | Android repository | iOS repository |
| --- | ---: | ---: | ---: |
| Agent | 127 passed | 126 passed | 126 passed |
| CAS / snapshot / classifier | 24 / 6 / 8 passed | 24 / 6 / 8 passed | 24 / 6 / 8 passed |
| State core / restore | 35 / 63 passed | 35 / 63 passed | 35 / 63 passed |
| Frontend build and final TypeScript check | Passed | Passed | Passed |
| Localization (9 locales each) | 1,448 keys | 1,371 keys | 1,389 keys |

One agent scale fixture and two restore scale fixtures per repository remain opt-in.
Desktop storage (58) and database (31) regressions also passed. Desktop client tests
passed 57 Shared Storage cases plus the application-only request-schema case;
the selected-operation JavaScript progress suite passed five cases. Both mobile
repositories passed host `cargo check --locked --tests` with
`NOLAND_SKIP_NATIVE_BUILD=1` and a `TAURI_CONFIG` override clearing bundle resources,
external binaries and macOS frameworks. These are Rust checks, not native device
builds. The Linux snapshot implementation passed an x86_64 cross-target check
using the installed stable toolchain. Changed Rust formatting, diff checks and
baseline-relative platform merges passed, preserving platform-specific code.

Native packaging, API 20 Linux deployment, live save/launch acceptance and a
same-PC/provider throughput comparison remain pending. The source changes are
local and uncommitted; API 19 deployment results do not validate the V3 revision.

## Reference workload

A completed native Linux/ext4 workload on agent/client 0.1.28 provides the existing
baseline. These measurements precede the new optimization changes.

| Measurement | Baseline |
| --- | ---: |
| Manifest files | 186,664 |
| Logical file bytes | 35,868,392,703 |
| Encrypted packs | 360 |
| Uploaded bytes including metadata | 27,896,131,364 |
| Downloaded bytes including metadata | 27,896,130,237 |
| Backup wall time | 4,283,782 ms |
| Restore wall time | 2,481,819 ms |
| Reconciliation | 145,860 ms / 963,010 path visits |
| Planning | 34,036 ms |
| Capture/hashing wall time | 460,669 ms |
| Additional restore verification reads | 8,365,739,355 bytes |
| Maximum outstanding upload journal entries | 2 |
| Remote list calls during backup | 181 |

The old `download_duration_ms` measures the streaming reconstruction pipeline,
including local backpressure, rather than independent network capacity. Snapshot
worker time overlaps capture/hashing; packing time overlaps uploads. Journal
attempt counts do not expose every retry internal to rclone.

## Local correctness and scalability

Run from `state-agent/`. Use a fresh `CARGO_TARGET_DIR` if a pre-existing target has
incompatible SQLite/bindgen build artifacts.

```sh
cargo test --locked --workspace
NOLAND_SYNTHETIC_FILES=4096 cargo test --locked -p noland-state-agent --lib backup::scale::synthetic_storage_workload -- --ignored --exact --nocapture
cargo test --locked -p noland-restore --lib pipeline::tests::large_small_file_publication_workload -- --ignored --exact --nocapture
cargo test --locked -p noland-restore --lib pipeline::locality_tests::database_backed_codec_restore_workload -- --ignored --exact --nocapture
```

The capture workload creates a private runtime installation with assets and one
save. It checks first capture, unchanged capture, one-save mutation, fresh
restore, and unchanged restore, including every restored file's hash. Its output
is marked `STORAGE_SCALE`; the large publication workload emits
`RESTORE_PUBLICATION`. Neither starts an application or touches existing saves.
Local synthetic results are not a measured cloud or real-game speedup.

The 4,096-file local validation confirmed zero rehashed files on unchanged capture,
one rehashed file after changing the save, every restored output verified, and
zero additional restore read-back bytes. Repeating restore reused all 4,096 files
and downloaded no packs. These are correctness/I/O results; concurrent development
work makes the elapsed times unsuitable for comparing cloud throughput.

The 186,664-file publication workload also passed with every output checked and
a peak of 256 unpublished files. The overlapped pipeline was exercised on APFS;
Linux/ext4 speedup still requires the native comparison below.

That publication-only workload feeds a shared tiny chunk directly and omits the
database. It does not measure manifest/index decoding, realistic pack records,
database publication, or restore-to-backup cache maintenance. Keep its timing
separate from an end-to-end restore result.

## Follow-up pipeline checks

The second local batch adds descriptor-anchored small-file capture, current
classification on content reuse, single-pass resumed chunk verification,
authenticated metadata/cache streaming, reusable pack decoders and owned payload
handoffs. Restore retains durable rollback and adds guarded leaf-only tombstones,
transactional launcher prerequisites, and a 128-active-hasher limit with verified
readback on eviction. Database publication is batched and measured separately;
checkpoint generation survives restart and avoids reloading unchanged learned
rows. Clients use SSH stdin, adaptive status polling, scoped operation handles,
stable admission IDs, and terminal seal gating.

Anchored metadata inspection uses `fstatat(AT_SYMLINK_NOFOLLOW)`, not a content
open: mode-000 files can still be classified without reading them, while content
capture never follows a replacement symlink. Catalog publication now reports a
failed `LATEST` update instead of silently leaving a valid stale pointer. Retrying
the same committed catalog verifies its identity and republishes only the pointer,
preserving the immutable encrypted history.

Status polling has an optional progress-only projection: lifecycle correlation,
terminal errors, progress and pack counters stay available without retransmitting
the full operation result, retry document, metrics or non-pack journal aggregate.
Regression coverage uses an operation detail larger than the transport's 4 MiB
frame limit and verifies a projected fixture below 64 KiB, with identical progress
and pack totals. Full responses remain available when no projection is requested.

The disk-backed codec fixture restores 16,384 small files through authenticated
metadata and pack decoding, verifies every output and trusted database row, and
reports `RESTORE_DB_CODEC`. Its one deduplicated payload deliberately stresses
file-count/publication costs; it is not a realistic cold-network throughput test.
The first run exposed a repeated app-wide association scan: a 24.22-second
database tail in a 32.43-second pipeline. Input-first indexed lookups reduced the
tail to 2.03 seconds and the pipeline to 11.91 seconds on the same local fixture;
all 16,384 outputs and trusted database rows passed verification. A VM-step
regression also checks that a fixed lookup batch stays bounded as the app grows.
These development runs had variable filesystem flush times and are not a
controlled comparison with the deployed 0.1.28 agent or a cloud speedup claim.

The next comparison must include a disk-backed database and retain the original
pack/manifest compatibility and durability requirements. In addition to the
workload cases below, record:

- Small-file snapshot staging bytes and directory operations, source reads,
  retries, and peak capture/planning memory.
- Unsealed-chunk verification reads during retry, distinct from authenticated
  sealed-pack receipt validation.
- Metadata serialization/decompression allocation peaks and local cache bytes.
- Remote-index rows actually changed, restore database-commit elapsed time,
  database lock time, and checkpoint work on an unchanged learned generation.
- Decoder CPU, owned payload handoffs, incomplete-file hash-state high-water
  count, and verification readback used when inline hashing reaches its bound.
- Client status-request count/bytes, SSH/SCP invocations, unchanged progress
  suppression, and terminal-state delivery after reconnection.

Failure coverage includes concurrent source/parent replacement, metadata-only
classification changes, corrupted captured chunks, failed index retrieval,
replacement directories at tombstone paths, unstable rollback capture, and
repeated interruption of recovery. Mobile lifecycle tests must distinguish seal
admission from successful seal completion. Selected-app progress tests must cover
multiple sequential operation IDs and late events from an earlier operation.

Native release packaging and fresh cross-PC launch acceptance remain separate
from host Rust/TypeScript checks. The old Hookshot bundle lacks its separate
launcher; runtime registration/path rebasing must be completed before claiming
cross-PC restore-and-launch acceptance.

## Native Linux comparison

Local verification uses host workspace tests for each of the desktop, Android,
and iOS source trees. Client Rust verification is `cargo check --locked --tests
--no-default-features` with native packaging disabled, not a signed/native release
build. Frontend checks include TypeScript, all nine locale bundles, and selected
operation-progress regressions. Strict restore Clippy and isolated three-client
monitor/submission tests supplement the workspace suites. Linux snapshot code is
also cross-type-checked separately; this does not validate the Linux BPF observer.
After a local disk-exhaustion failure, free only disposable build artifacts and
rerun the affected tests; do not reduce production disk-admission safety margins.

Use an authorized test instance and isolated destination. Keep provider, machine,
filesystem, content, and gameplay activity comparable to the baseline. Record the
actual agent build/API version and whether source data is warm in the page cache.
Do not drop system-wide caches on a shared or actively used machine.

Measure these cases independently:

1. Cold complete backup and native Shared Storage restore.
2. Unchanged backup: no file payload rehash/upload after an established trusted
   baseline, unless an observation gap or metadata change requires verification.
3. One changed save: unchanged installation assets retain their verified identity.
4. Restore followed by unchanged backup on a fresh agent database.
5. A disconnected client reconnecting to the same still-running operation.
6. An interrupted transfer retrying its immutable captured revision.
7. Upgrade from a legacy cache-excluding backup: rediscover unindexed app caches,
   verify their bytes after restore, and confirm application/save loading.

Capture wall time, verified/published files and bytes, extra read-back, disk
read/write bytes, file/directory flush time, file opens, upload concurrency and
queue waits, remote listings/process invocations, retries, and peak memory/disk
usage. Time until the application is usable is separate from transfer completion.

Acceptance requires complete content verification, durable publication, bounded
queues/handles, unchanged rollback behavior, and retained saves. A faster result
that skips validation or loses retry state is not an optimization.

## Third review follow-up

Local regressions cover seal/transfer mutual exclusion, cancellation while queued,
stable all-app child selection, completed-child reuse, immutable retry after a
source mutation, bounded chunk hints, and schema-v5 status projection migration.
No-op backups with a warm authenticated cache download no manifest. A cold parent
cache downloads the manifest and index once, with both included in operation
network counters (including failed attempts).

Ready-only upload grouping must retain per-object count/byte admission, refill
past a straggler, and reuse per-pack receipts after grouped-transfer failure.
Deployment checks must notice source-only changes with an unchanged RPC API,
ignore build outputs, and defer refresh while compatible work is active. Android
source-cache identity uses archive content rather than the API version.

At this review stage, capture-to-upload overlap was still deferred. The later
upload follow-up below adds the durable partial-capture protocol and interruption
coverage. Native upload speedup and device-specific packaging remain unmeasured.

## Foreground restore and straggler follow-up

A later native run on instance 60380867 completed all 186,664 files and
35,868,392,703 logical bytes in 1,429,616 ms. This is a different VM from the
baseline above, not a controlled speedup comparison. Its journal recorded
GameplaySafe with four downloads and one verifier on a 15-vCPU machine. The
open session at admission was Selkies' background Python desktop service. Four
pack operations stayed in flight for approximately 99% of the pack phase; one
16.9 MB object took 534 seconds end-to-end. That time included provider startup,
decoding and local backpressure, so it was not a measurement of network speed.

The follow-up implementation keeps genuine/unknown application sessions
protected, but excludes narrowly identified desktop/support services from
gameplay-budget selection. It does not exclude Python interpreters, all of
`/opt`, or the general desktop cgroup. Restore progress now records the selected
workload, CPU count, download/verification budgets and buffering limits.

Authentication and decompression run on restore-owned workers rather than
serializing every stream's CPU work on its parent async task. The admitted pack
count bounds these workers (at most 32), each handoff is at most 256 KiB, and the
existing bounded plaintext queue remains unchanged. Admission includes the
additional encrypted handoff buffers. Cancellation stops and joins every decoder
before rollback, including a decoder waiting on a full assembly queue. Per-file
hashes, file syncs, directory barriers and prerequisite rollback remain mandatory.

Rclone streams have a 60-second network-idle deadline and a 30-second EOF/exit
deadline. These clocks exclude consumer/decoder/publication waits. An individual
trickling stream can be restarted once after two consecutive 30-second network
windows at least eight times slower than three recent healthy samples matching
the controller generation, reserved concurrency and transfer width. Samples need
at least 1 MiB/250 ms of weight, with local consumer time below one-third of
network time and no sampled I/O pressure. A lone flow or group occupying every
reserved slot cannot qualify. Retries retain the normal
attempt/backoff bounds, kill and reap the old child before restarting, and replay
only the same authenticated immutable pack. Consumer/integrity failures are not
network-retry candidates. No speculative parallel duplicate is started.

Each pack's journal retains credential-free network, consumer, permit, retry and
exit-wait timings, attempts and watchdog restarts. Long waits publish live phase
updates. A `stream_plan` also records requested records, unique plaintext bytes
and selected-manifest chunk references/bytes: a small compressed or highly reused
object can represent substantially more destination work. Logical transfer
progress stays monotonic; actual wire bytes count partial failed attempts too.
Global pipeline counters separate decode CPU, decoder dispatch and assembly-queue
waits. None of these overlapping counters may be summed into elapsed wall time.

Regression coverage includes background-service classification, true/unknown
applications, concurrent decode on a single-thread executor, bounded-queue
cancellation, authenticated replay without duplicate publication, corruption,
idle/trickle/EOF stalls, slow local consumers, live wait diagnostics, child
reaping before retry, and additive journal detail. Native cloud throughput and
the historical straggler's precise cause still require a new authorized run;
these changes do not establish that every slow pack was a provider failure.

## Historical API 19 upload follow-up (2026-11-12)

The first upload implementation added the six improvements below. Its local
validation was followed by an authorized API 19 agent deployment and native Linux
tests. The API 20 streaming protocol above supersedes its fresh-request profiles
and V2 batch scheduling; old checkpoints retain their original resume path.

- **Upload supervision:** single-object, grouped and metadata uploads use one
  retry owner, with rclone's internal retry loops disabled. Byte-progress idle is
  bounded at 60 seconds, provider finalization gets its own 300-second budget,
  and process/stderr exit is bounded at 30 seconds. A finalizing member of a group
  gets finalization grace even when the entire group has not reached 100%.
  Callback work is excluded from watchdog clocks; observable local IO pressure
  suppresses idle/slow-peer decisions. One slow-peer restart is allowed after a
  30-second window at least eight times slower than three healthy peers. Old
  children are killed and reaped before retry. Integrity/immutable-conflict and
  local observer errors do not become network retries.
- **Diagnostics:** journal upload detail records attempt number, payload bytes
  reported by rclone across failed attempts, transfer/local-IO/observer waits,
  limiter/backoff/finalization/exit waits, phases and watchdog reasons. Fast
  successful copies supply a lower bound when they finish before the first stats
  event. These are payload measurements, not packet-level wire accounting. Group
  diagnostics include a group ID and object count: repeated group counters in
  per-pack rows must not be summed. Adaptive throughput feedback excludes
  observed finalization and callback time.
- **Preparation budget:** hashing and compression share a CPU/memory budget.
  Balanced uses about half the available CPUs; Fast/Full can use the available
  CPUs up to the bounded implementation ceiling. Available host/container memory,
  Linux IO pressure and genuine foreground sessions reduce background worker
  admission. Unknown memory uses a conservative fallback. Environment overrides
  may lower the admitted counts. This policy affects backup workers only.
- **Grouped transfers:** verified per-object rclone completion receipts release
  that object's rolling-window bytes and spare weighted slots immediately. One
  process slot remains reserved through exit. A failed group retains completed
  journal receipts; only its private staging links for completed objects are
  removed after reaping, so retry cannot consume released slots again. Original
  immutable ciphertext remains available.
- **Capture overlap:** a v2 authenticated local checkpoint persists the selected
  jobs, identities and snapshot ownership before network work. Capture batches
  contain at most 1,024 files / 256 MiB, admitting one oversized file alone. Each
  batch seals/fsyncs its CAS lease and authenticated receipt before packing or
  uploading. One prefetched capture overlaps a count/byte-bounded rolling set of
  pack/upload batches. CPU workers and network slots are divided across those
  batches, not multiplied by their count; oversized batches reserve the entire
  window. Cross-batch chunk deduplication and stable per-batch pack receipts are
  preserved. Complete manifest/index publication still waits for every batch.
  Retries reuse durable captures even if those source saves later change. Changed
  *uncaptured* source identities fail closed and require a new backup revision.
  Corrupt partial receipts never trigger silent recapture. All capture/packing
  workers join on interruption. Cleanup ownership survives the final checkpoint
  replacement and removes only agent-owned leases/snapshots/receipts. Legacy v1
  checkpoints remain readable; older agents reject v2 rather than publishing a
  partial revision as complete. Remote bundle/pack formats are unchanged.
- **Scaling:** aggregate live progress is coalesced across concurrent requests;
  grouped completion updates are transactional. Rclone uses targeted immutable
  checks with `--no-traverse`, so pack upload no longer lists repository-wide
  history. Local storage also uses targeted writes; compatibility providers keep
  the existing operation-scoped inventory fallback.

Regression coverage includes a 3,074-file interrupted partial capture with live
upload/capture overlap, bounded prefetch, cross-batch duplicate chunks, immutable
retry after a source save changes, complete authenticated chunk readback, changed
uncaptured identities, corrupt partial checkpoints, group-slot release before a
straggler exits, failed-attempt byte accounting, child reaping, provider
finalization, slow peers, exit stalls and targeted-copy arguments. Existing
unchanged-backup, restore, publication and recovery tests remain required.

Validation: all three host-side `state-agent` workspace test suites passed.
Final agent suites passed 120 tests on desktop and 119 each on Android/iOS;
storage passed 58, DB 31 and restore 63 in each repository. The three designated
scale fixtures remain opt-in. Clippy completed with existing workspace warnings.
These checks do not substitute for native device packaging or a live cloud
upload benchmark.
