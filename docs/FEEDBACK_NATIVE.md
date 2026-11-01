# Native feedback handoff

## Scope and wiring

Desktop native implementation lives in `src-tauri/src/services/feedback/`, with Tauri commands in `src-tauri/src/commands/feedback.rs` and persisted program state in `src-tauri/src/models/feedback.rs`. All eleven `feedback_*` commands from `IMPLEMENTATION_COORDINATION.md` are registered in `main.rs`. Arguments use Tauri's camelCase convention; results are JSON objects. Credentials remain native.

`AppContext` owns the service. Startup starts its workers; focus triggers recovery, and stream teardown checkpoints observations. New rental tracking, verified pairing, runtime observations, provider deletion, and authoritative owned-instance reconciliation feed the journal. Provider creation has a separate durable pending intent/recovery guard in `services/provider_creation.rs`; feedback failure must not authorize a second ambiguous paid creation.

## Consent, persistence, and delivery

- Defaults are off. Malformed/future program state fails closed. A usable enrollment, protected credentials, supported desktop platform, configured endpoint, healthy journal, and current permission are required for collection/delivery.
- `NOLAND_FEEDBACK_API_URL` is compile-time configuration; missing/invalid configuration leaves enrollment unavailable. No production endpoint or operator identity is invented. Explicit notices retrieval is separate from local-only status.
- Secrets use the existing native secure store; `state.json` holds references. Enrollment recovery proof/request identity is saved before networking. Disable/delete immediately revoke local capture; pending server controls and deletion receipt polling are durable. A server control operation can still require networking while collection is disabled.
- The atomic spool is `feedback/outbox-v1.json` beside `state.json`, bounded to 100 sessions / 5 MiB, with 30-day pruning and 64 KiB session payload bounds. Unix permissions are restricted. This spool contains feedback data, including review drafts; it is not a claim of encrypted-at-rest storage.
- Session and review revisions are independent. Delivery checks accepted revisions, uses scoped bearer authentication, bounded HTTP requests, no redirects, and retry/backoff handling including Retry-After. Remote rejection/storage failure can leave feedback pending or paused; it must not be treated as successful publication.

## Review clear / failed-destroy safety

`feedback_stage_review(providerInstanceId, null, null)` replaces the old draft with a persisted tombstone at the previous **review** revision plus one (or revision 1). Blank comments normalize to null. It does not retain the old score/comment and does not increment the session revision merely to edit a review.

Thus: stage review → provider destruction fails → Skip stages null/null successfully → later confirmed destruction cannot publish the old draft. Delivery requires confirmed destruction and first-frame eligibility; a tombstone uses DELETE with `X-Review-Revision`, not a PUT containing the old review. External disappearance clears the draft and cannot publish a review.

Important limit: this guarantee requires successful staging persistence. If the clear command fails, it returns an error; the old durable draft may remain. The frontend must report that nonblockingly and must not claim that the clear succeeded. Neither staging nor feedback HTTP invokes provider destruction.

## Measurement capabilities and limits

- Platform support is declared for macOS, Windows, and Linux; this is not device certification. Reviews require a tracked enrolled rental with genuine machine identity and first received-video-frame evidence. Existing/imported rentals are not retroactively enrolled.
- Runtime frame evidence uses the existing C receive counter with a per-start baseline and session-generation/host binding. It measures received decode units, **not** guaranteed displayed frames. The new permission and counters are Rust-side fields; the native C request/stats ABI is not extended by these edits.
- `network-agent` adds processed batch IDs and received-RTT evidence. `network-contracts/src/feedback.rs` validates fresh, watermarked, frame-active observations after a full 60-second warmup, with metric/version, coverage, and route separation. Legacy agents or insufficient/stale evidence yield N/A, not inferred jitter. No raw probe samples are contributed.
- Provider machine/rental IDs use validated decimal strings in feedback JSON, not host/offer aliases or floating-point conversions. Observation gaps/external removal do not invent successful setup or failure evidence.

## Validation boundary (2026-10-26)

This finishing pass was source-only: command registration, changed initializer/call sites, Rust-side FFI/cfg/import consistency, and review tombstone flow were inspected. Changed Rust sources were formatted and checked with standalone `rustfmt` (child-module traversal disabled), and whitespace was checked with `git diff --check`.

**No native compilation or tests were run:** no Cargo check/test/build, Xcode, Gradle, or Tauri invocation. Rustfmt parses/formats source but does not establish name resolution, trait/borrow correctness, linking, or runtime behavior. Cross-platform secure-store behavior, actual provider/feedback integration, restart recovery, frame-counter behavior, and real-device jitter still need authorized downstream validation. No paid API calls, contributions, deployment, branches, or commits were performed.

Tracked deletions under `.tmp/`, `src-tauri/.local-tools/`, and `src-tauri/binaries/` were already present at the start of this pass and were left untouched. No deletion of those paths was found in the new feedback/provider-creation source; Git status alone cannot establish their origin. Missing bundled runtime artifacts remain a packaging limitation, independent of this source-only review.
