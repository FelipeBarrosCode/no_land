# Instance-scoped restore admission and claim cleanup

## Reproduced failure

Download operation `0313122e-a92f-40a0-825d-297825137c10` on PC 60812509
was blocked by claim `3ff83d6a-ccb8-4886-933a-d77b266ede4c`, associated with
completed restore `55e4985f-ef89-4558-a94c-b7f259850231` on the previous PC.
Read-only inspection found one owned PC, no running agent operations, no pending
deletion and an unlocked local repository-admission OS lock. Inspection did not
change the repository.

The restore wrapper and its inner implementation each created a credential
guard for the same ephemeral session path. The inner guard could shred that
path before the outer repository guard authenticated and removed its cloud
claim. The completed restore then logged a cleanup warning but left its activity
claim to block another PC.

## Ownership and access rules

- Local transfer exclusion remains per managed agent root, using the same
  `repository-admission.lock` OS lock observed by native recovery.
- Storage instance identity is durable across daemon restarts. The runtime
  `AgentConfig.instance_id` fallback changes on restart and must not be used
  as proof of durable ownership.
- New restores use authenticated, instance-scoped **read** claims. Reads on
  distinct instances may coexist; two executions on the same instance do not.
  `RepositoryActivity.access` is `exclusive` or `restore_read`, with optional
  durable `instance_id`. `acquire_for_instance` is the scoped API; the existing
  `RepositoryGuard::acquire` remains exclusive for compatibility.
- Uploads, catalog/seal publication, reclamation, deletion and administrative
  recovery retain **exclusive repository-wide** protection. Upload is not
  universally append-only: convergence can retire data another restore needs.
- Every claim stays visible in `maintenance/activity`, under the existing
  authenticated envelope. Older clients therefore fail closed instead of
  missing a new reader during deletion.
- Missing access fields in legacy records mean exclusive, never read-only.
  Unknown modes, malformed identities, recovery fences and every pending
  deletion block restore admission. A matching operation ID does not let a
  restore bypass a pending deletion.
- Destructive execution independently checks `access == Exclusive`, not merely
  the claim count and operation ID. Even a sole reader with the deletion plan's
  operation ID cannot authorize deleting metadata or packs.
- Shared-reader admission does not expire, remove or adopt another instance's
  claim. Interrupted readers still protect against destructive maintenance.

## Credential lifetime and completion

A credential guard owns only its execution's private temporary files. Guards
for the same logical session must not overwrite or shred each other's files.
Restore admission and final claim release both happen while the outer
credential guard is alive. Temporary credentials are still removed when that
execution finishes or is cancelled; they are never retained in diagnostics.

Durable publication alone does not expose a terminal `COMPLETED` result.
Restore, upload and seal stay in a finalizing state until their repository
release attempt returns and its credential-free diagnostics are recorded.
Normal release verifies exact-key absence after one deletion; an acknowledged
but unconfirmed deletion is a cleanup warning, never permission to retry blindly.

There is no new end-to-end finalization timeout. Ordinary rclone `stat` retains
its existing transport behavior; the deletion-specific control deadline does
not apply to every normal admission read. Post-commit cancellation therefore
drains finalization rather than promising an immediate stop or expiring a claim.

Cleanup failure must be recorded with the affected operation, not hidden only
in tracing. Already committed local data must not be spuriously rolled back or
reported absent because cloud-claim cleanup failed. Uncertain deletion is not
automatically retried and another instance's ownership is never force-cleared.

## Existing legacy claims and rollout

The old blocking claim has no authenticated read-access/instance metadata.
Neither a completed local database row nor an object's age can retroactively
grant it read-only status. It remains protected until an explicitly approved
targeted recovery clears that exact legacy object.

If several interrupted readers remain, targeted recovery can freeze and retain
the other claims while clearing exactly one approved target. Its encrypted
receipt binds a bounded count/hash commitment to the retained inventory; another
recovery fence, changed inventory or pending deletion still blocks the attempt.
There is no bulk clear or implicit retry.

These are source changes, not authorization to deploy/restart a live agent,
erase an existing claim, retry a transfer or delete backup data. The new code
must be installed through the normal authorized agent-update path before it
can change remote admission behavior.

## Required regressions

- Successful restore and immediate use from another agent root leave no claim.
- Original transfer errors, cancellation and failed release preserve correct
  outcome/cleanup diagnostics without leaking credentials.
- Independent guarded sessions cannot delete or overwrite each other's files.
- Read/read admission succeeds across instances, while same-instance attempts,
  exclusive writers, legacy claims and recovery fences still conflict.
- Reader/exclusive races cannot admit a destructive writer alongside a reader.
- Pending deletion is checked before and after admission, including a matching
  operation ID.
- Old clients see all new claims; old encrypted activities and recovery receipts
  still deserialize conservatively.

## Local validation (2026-11-15)

- Shared workspace: **834 unit tests and 12 integration tests passed**; ten
  opt-in tests were excluded from the default run
  (`instance-admission-workspace-final.log`).
- A separate run using the SHA256-verified official rclone 1.76.0 binary passed
  **510 tests, none ignored**, with only the synthetic scale measurement
  excluded. This includes all six local-rclone opt-ins, concurrent complete
  restores on two isolated agent roots, credential cleanup, metadata
  authentication and exact fixture deletion. All rclone configurations were
  explicit local fixtures, not live provider accounts
  (`instance-admission-real-rclone-final.log`).
- Desktop native Cloud Storage checks: **110 passed**, including finalization
  monitoring, cleanup-warning projection and retained-claim selection
  (`instance-admission-native-final.log`).
- Desktop script checks: **452 passed**; frontend build and **1,496 keys across
  nine locales** passed (`instance-admission-scripts.log`,
  `instance-admission-frontend.log`).
- Android and iOS source ports each passed **69 script tests**, TypeScript,
  offline/locked Cargo metadata and syntax checks. Nine-locale checks passed
  with **1,416 Android keys** and **1,434 iOS keys**. Shared source and client
  regression parity were checked; these are not native device builds.
- All-target shared-workspace Clippy completed with warnings, not a
  warning-free baseline. Follow-up test-only lexical lock scopes and comparison
  cleanups passed all **202 default storage tests** again; the new test lint
  findings are resolved. Rust formatting and diff checks passed.

Parent validation logs are in `state-agent/target/upload-review/`. These validation
runs did not deploy or restart an agent, modify a live activity claim, start a cloud
transfer, build a native mobile application, commit, push, merge or release.

Before desktop publication, changed generated translation IDs were re-keyed to
their English-text hashes, with identical Cloud Storage aliases consolidated.
The pre-push checks passed **455 script tests**, **six Gitleaks locale
regressions**, **1,495 keys across nine locales**, and the frontend build.
These later checks are recorded in `pr-publish-locale-*.log`.
