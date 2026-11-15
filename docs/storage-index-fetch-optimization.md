# Provider-neutral storage browsing, fetching and scheduling

## Scope and baseline

This work optimizes the existing repository format. It does not replace the
chunk CAS, UUID-based immutable packs, encrypted manifests or authenticated
publication protocol with provider-specific objects.

The completed Hookshot restore `55e4985f-ef89-4558-a94c-b7f259850231` on
instance 60789119 recorded 461,762 ms of pipeline time, 481,450 ms of internal
operation time and approximately 491 seconds from creation to completion.
It downloaded 27,896,130,237 ciphertext bytes. Approximately 8,965 MiB of
additional local verification reads were visible near the end of the run;
that screenshot is not an exact final read-back measurement.

The older pipeline measurement was 633,884 ms on a different instance.
These runs are not a controlled provider or scheduling comparison.

## Portable addressing

```text
logical file -> authenticated chunk hashes
             -> immutable pack key + authenticated record locations
             -> provider adapter / optional opaque object locator
```

Drive file IDs, S3 version IDs and provider validators must not become the
portable identity of a game, chunk or pack. A locator is an optimization hint,
not authentication. An ETag is not generally a content hash.

Current caches and the SQLite remote-content index are reused where suitable.
Cache scopes must distinguish repository/key identity and provider namespace;
ambiguous account/endpoint scope must produce a cache miss rather than reuse.
No credentials or repository keys may be returned in browse snapshots or
persisted as cache identifiers.

## Browsing is not a transfer or deployment

The selection interface has two independent sources:

- **Upload games** displays the agent's indexed local applications.
- **Download games** displays the authenticated cloud application catalog.

Opening either selector must not deploy/restart an agent or recursively scan
game contents. Capability/update errors remain visible instead of triggering
an implicit update. Actual operation submission retains its stricter runtime
compatibility, repository-admission and selected-object checks.

The frontend requests a cache-only snapshot first and a coalesced fresh
snapshot afterward. Cached rows remain visible during refresh and refresh
failure, with explicit freshness. A view that has closed or changed instance,
profile or repository cannot accept an older request's result. Empty verified
results are distinguishable from errors and unavailable caches.

Mutation controls require a successful fresh result. That UI gate supplements,
but never replaces, live native transfer/deletion validation. Refreshing does
not block closing the selector or interacting with the rest of the app.

The native `get_instance_storage_browse_snapshot` command accepts `instanceId`,
`target` (`cloud` or `remote_pc`) and `refresh`. A cache-only call (`false`) does
no network I/O or credential renewal. A fresh call (`true`) joins equivalent
in-flight refreshes. Responses carry `entries`, an opaque `scope`, `observedAt`,
and `freshness`. The private per-context memory cache retains at
most 16 entries, with a 30-second freshness window.

Browse retention is also bounded by 10,000 rows and 8 MiB per snapshot and
16 MiB aggregate (including bookkeeping headroom). Charges include backing
Vec/String capacities. Byte-pressure admission evicts only idle LRU entries;
an oversized or blocked refresh preserves its prior rows/timestamp and reports
an error. Coalesced completion channels share charged values through `Arc`.

Reopening within that native freshness window needs only the cache-only call;
explicit **Refresh** always bypasses it. While a refresh is already running,
the old rows are marked cached so a new reader joins the same request.

When rclone already exists on the desktop PATH, cloud metadata can be read
locally through the shared storage library and private ephemeral sessions.
Otherwise, browsing uses only an existing protocol-compatible agent.
Unavailable/incompatible agents yield an explicit update/reconnection error,
not an implicit install. Transfer execution keeps exact-source/trust checks.
Selection changes and operation admission/completion invalidate browse state.
A failed refresh retains rows and their original observation timestamp, but
marks them stale.

### Missing helper on a newly provisioned PC

An existing API-compatible agent does not imply rclone is installed. A failed
cloud catalog read now performs a bounded, read-only helper-presence probe;
healthy reads incur no extra prerequisite round trip. Native errors expose only
allowlisted reason codes and owned messages, never provider stderr or arbitrary
diagnostic details.

`prepare_instance_storage_helper` is an explicit user-clicked action. It uses
the existing pinned-SHA256 installer, checks storage idleness, and prepares only
rclone. It never deploys/restarts the agent, starts a transfer or modifies a
repository. Listing/refreshing does not call it. The target account/SSH identity
is frozen and rechecked before installation; later settings changes cannot
retarget the in-progress setup.

On 2026-11-15 the user separately authorized this repair for PC 60812509.
rclone 1.76.0 was verified afterward, the same agent process predated the helper
installation, active operations remained zero and the app displayed a refreshed
cloud catalog. This was prerequisite repair and metadata browsing, not a game
transfer benchmark.

## Metadata and payload fetches

Small metadata objects should not require a temporary whole-file copy or
redundant metadata lookups when one bounded read can safely supply the data.
Every provider retains a compatibility fallback. Optimized reads must enforce
size bounds, propagate authentication/transport errors, classify exact absence
correctly and reap cancelled processes.

Immutable cached envelopes and authenticated chunk observations are disposable
hints. They must not override current deletion receipts, pending maintenance,
changed commit identities or an uncertain publication. Cache loss/corruption
must allow safe rebuilding. A failed listing is never an empty inventory.

Batch independent work within bounded provider admission. Final publication
markers remain separate and last. Upload completion receipts, immutable-write
protection, source-generation checks and file/directory durability barriers
remain mandatory.

The `read_small` provider method has an exact-object, bounded-memory contract.
Local storage and rclone have optimized implementations; the legacy fallback
retains a stat and private temporary download because older providers do not
promise a classified missing-object download error. The optimized healthy
catalog fixture uses four provider operations instead of five, overlapping
commit-marker verification with encrypted catalog retrieval. These counts are
storage-interface calls, not hidden HTTP requests inside rclone. Tombstone
listing still happens after catalog authentication.

`MetadataReadMetrics` distinguishes attempts, payload bytes, admission/retry
wait and local buffer time. Its process-wait measurement includes process
startup/provider/pipe/exit time: it is not pure network latency. Raw provider
stderr, arguments, credentials and signed URLs are not exposed by this path.

Agent catalog refreshes share only overlapping requests, with two refresh slots
and a bounded queue; a later request still observes external changes. Catalog
and pending-deletion reads overlap within one provider's admission budget.
Responses include display-only freshness and request/setup/catalog/pending
timings. These additions retain RPC API version 22.

SQLite schema 6 adds a separate immutable-metadata cache, globally bounded to
128 entries/64 MiB with an 8 MiB per-entry cap. It stores encrypted catalog
envelopes and keyed observation receipts, not credentials, and leaves existing
chunk-index receipts unchanged. Hits still authenticate the cached value and
check current object presence/size, LATEST, COMMITTED and tombstones. Reuse
relies on the repository's immutable-object contract.

Where the session supplies an unambiguous account/endpoint namespace, rotating
credentials need not invalidate immutable observations. Otherwise a private
keyed exact-config binding conservatively misses after credential rotation.
Legacy operation bindings or deletion previews may therefore require a fresh
submission/preview after upgrade; no claim or deletion fence is auto-cleared.
Terminal operation records retain pipeline/read-back/I/O/catalog diagnostics
instead of losing them with transient progress.

## Adaptive pack scheduling

The previous baseline already orders pack starts by priority and earliest
manifest/chunk use. Adaptive selection must improve on that baseline rather
than introduce a global ordered-delivery barrier.

Decisions should favor useful sequential work and completion of unfinished
files, preserve launch priorities, age delayed candidates and refill free
download slots immediately. Feedback is hint-only and must be bounded.
Repeated authenticated records from restarted streams cannot double-count
assembly or scheduler progress. Shared chunks and record order within immutable
packs mean that not all extra read-back can be eliminated.

Authentication, full-file verification, inode/path identities, mandatory
flushes, durable intents, publication and join-before-rollback are unchanged.

The implementation uses a sparse file/pack incidence graph for cross-pack files.
Candidate credits favor the next manifest-order pack and closure of an inline
partial. Every eighth admission follows the original order within the remaining
priority tier, bounding bypass without waiting for running packs. A 512-slot
nonblocking mailbox coalesces file-state hints. Dropped hints can only reduce
optimization quality, never change reconstruction or verification.

Aggregate resource admission includes a 2 MiB eligibility-bitmap allowance and
14 MiB graph/build/mailbox allowance. Oversized metadata or workloads without
cross-pack dependencies use the original static order. Scheduler metrics report
the selected/fallback mode, graph bytes, construction/decision/feedback time,
incidence visits, fairness and reordered decisions.

Final I/O diagnostics include exclusive read-back byte buckets for first
out-of-order placement, hash-state eviction and missing hash state, plus total
read-back and peak unfinished files/bytes. These are local disk reads, not extra
cloud downloads.

The deterministic 192-file equivalence fixture reduced hash evictions from 64
to zero, read-back from 1,032 bytes to zero and peak unfinished files from 192
to 128 with identical restored bytes. The 186,664-file/360-pack metadata fixture
performs 373,328 sparse incidence visits and 360 score-tree updates; its estimated
construction peak is 12,392,192 bytes plus a 23,336-byte bitmap. These are
synthetic mechanism/scale checks, not measured Hookshot cloud speedups.

## Measurement and rollout

Measure cached display time, fresh verification latency, provider request
counts, process launches, time to first byte, payload bytes, retry/throttle
time, local consumer backpressure, read-back causes and total operation time.
Parallel worker counters overlap and must not be added to wall time.

Targets of sub-500 ms cached display and roughly sub-5 second healthy-network
refresh are acceptance goals, not measured cloud guarantees.

Run deterministic fixtures and local provider tests first. Live comparisons
need separate approval and matching instance, repository revision, admission
and cache conditions. Use isolated destinations; never erase current game
files or saves to manufacture a cold restore.

Provider-specific change feeds, native persistent transport replacements and
sparse authenticated range reads require capability-specific implementation
and validation. They are not prerequisites for portable correctness, and must
not be enabled merely because a provider advertises S3 compatibility. No
automatic migration, repacking, cloud benchmark, deployment or release is
authorized by local validation.

The current optimized rclone implementation still launches a process for each
metadata request. There is no new persistent native transport, stable-Drive-ID
resolver cache, Changes API cursor or S3 range-fetch rollout. Those remain
capability-gated follow-ups, not hidden assumptions behind the portable path.

## Local validation

- Shared workspace: 782 unit tests and 12 integration tests passed; nine opt-in
  tests excluded from the default run (`index-fetch-workspace-final.log`).
- Separate storage/adapter run with official SHA256-verified rclone 1.76.0:
  229 tests passed, none ignored. All configurations and objects were isolated
  local fixtures, including real empty/missing/literal-key reads, catalog
  recovery/authentication and full fixture deletion
  (`fetch-real-full-v1760-20261115-14.log`).
- Scheduling suite including the opt-in 186k-file scale fixture: 13 passed
  (`scheduling-verified-final-20261115-09.log`).
- Desktop script suite: 431 passed, including snapshot lifecycle, stale result,
  explicit refresh and resource-admission source regressions
  (`index-fetch-scripts-integration-final.log`).
- Native desktop shared-storage suite: 102 passed, including cache byte
  accounting, exact wire contract, coalescing, invalidation and existing
  lock-recovery/operation regressions (`index-fetch-native-complete.log`).
- Frontend build and 1,487-key/nine-locale checks passed.
- All-target shared-workspace Clippy completed with existing warnings; this is
  not a claim of a warning-free `-D warnings` baseline.

Validation logs are under `state-agent/target/upload-review/`. No validation
step provisioned an instance, changed a live cloud repository, restarted a
remote agent or launched a native mobile build.
