# Authenticated Cloud Storage lock recovery

This is an explicit administrative operation, not automatic lock expiry or a
transfer retry. The initial native transport supports **Google Drive only**.
Other providers fail without making repository changes.

## Workflow

The desktop app exposes three MCP tools:

1. `storage_locks_inspect`: authenticate activity metadata and inspect recovery
   readiness. No agent deployment, service restart, transfer, deletion preview or
   repository mutation is performed.
2. `storage_lock_recovery_preview`: freeze one claim, operation, owner, repository
   root and account/profile binding in a short-lived native preview.
3. `storage_lock_recover`: consume that exact preview after fresh native human
   approval and `confirm_no_other_writers=true`. Clear only the selected activity
   object, verify its absence, and retain encrypted recovery receipts.

All three tools require the all-instance grant. The clear tool additionally
requires control permission and fresh human approval. Neither read tool can
clear a claim. The clear action never starts **Download games** or deletes game
packs, bundles, manifests, deletion plans, local files or provider credentials.

Approval explicitly confirms that **all other PCs, clients and external writers
using this repository have stopped and their prior provider requests have
settled**. Legacy owner hashes do not identify a
machine; the app must not pretend that it can infer the owner from age.

## Required checks

- Resolve credentials and the repository key through the running app's existing
  profile manager. Never export them, read credential files manually, put tokens
  in request URLs/command arguments, or print provider error bodies.
- Resolve the same provider root as ordinary transfers. Bind the preview to that
  exact Drive folder, active profile, repository key and provider account.
  For `drive.file` authorization, resolve the application's folder through an
  exact `'root' in parents` query; do not require access to root's own metadata.
  Returned real parent IDs are accepted only for that documented root alias.
- Query the complete provider inventory, including stopped and unregistered
  instances. The ordinary PC list hides those machines and is **not** evidence
  that they cannot resume. Exactly the target PC must remain owned.
- Verify zero active agent operations **and** observe the agent's real repository
  OS lock unlocked. Preview RPCs may own that lock without an operation row. The
  bounded probe never creates, unlinks or replaces the lock file.
- Authenticate the exact unchanged activity and freeze every other activity
  which must be retained. Reject an unresolved recovery fence and any pending
  deletion rather than clearing or resuming either implicitly.
- Reject changed accounts/profiles/keys, expired previews, incomplete/ambiguous
  listings, duplicate object identities, corrupt envelopes and unknown liveness.
- Hold native action exclusion and freeze native account/profile configuration
  during the mutation. Do not bypass provisioning or instance-action locks.

Provider/agent checks are fresh execution observations, not a timeout that
expires somebody else's claim. A stopped VM still blocks administrative
recovery because it can resume.

## Fencing and receipts

Before clearing the target, persist an authenticated prepared receipt and publish
an execution-unique `lock-recovery:<uuid>` activity in the existing admission
namespace. Ordinary managed clients already honor that namespace; the remote
agent does not need updating.

Recheck that exactly the frozen target, retained activities and this recovery
fence exist and that no pending deletion appeared. Re-authenticate the target immediately before removing
its exact Drive file ID. Verify target absence while the fence still protects
admission, persist and read back the completion receipt, remove only this
execution's fence, and verify both old identities absent.

Parallel restore readers can leave multiple interrupted instance claims.
Recovery still removes only one human-approved target, never the other claims.
Prepared/completion receipts authenticate a constant-size count/hash commitment
to the canonically ordered retained activities. Changes, additions or losses in
that snapshot stop recovery; old receipts without a commitment mean no retained
claims. Full-capacity inventory leaves room for the recovery's own fence.

The receipt files remain under `maintenance/lock-recoveries`. Prepared and
complete records are immutable, separately named, encrypted with the repository
catalog key and read back after upload. No payload objects are involved.

Drive recovery relies on the same complete read-after-write visibility and
execution-unique immutable claim identities as managed repository admission.
External/unfenced writers must be stopped by the human; the protocol cannot
protect against a client deliberately ignoring admission.

## Failure behavior

Consume a preview before the first cloud mutation. A lost reply cannot silently
create another attempt. Before target removal starts, a known rejected admission
may clean up only its own acknowledged fence.

Once target removal starts, any error, cancellation, timeout or lost provider
response leaves the recovery fence and prepared receipt for investigation.
Process exit does not prove a provider request has settled. Another recovery
fence cannot be force-cleared as an abandoned PC claim. Never automatically
retry, discard the receipt, erase pending deletion state or remove every lock.

For the original owner's **terminal metadata-readback integrity failure only**,
`storage_lock_recovery_preview` accepts `failed_job_id`. The native core verifies
that its own original job finished with that known read-only failure and
correlates its recovery identity with authenticated prepared/completion
receipts. This is not authorization from an error message: fresh native human
approval, current account/profile binding, complete provider inventory,
stopped-writer/provider-settlement confirmation and OS-lock checks still apply.

Resume preserves the original recovery ID, target and fence. It never creates
another prepared plan. If an authenticated completion receipt already exists,
only the original fence is released; target deletion is not repeated. A missing
target without a completion receipt, another owner, unknown/network failure,
changed receipts, changed/unselected activities outside the retained commitment
or pending deletion blocks this path.

After acknowledgement of a new metadata upload, Drive may update its
metadata-only version. Verification may perform up to three bounded **reads**
of that exact new file ID, requiring the exact uploaded ciphertext, authenticated
value and a stable final identity/version. It never re-uploads during these
checks and never adopts changed bytes. Existing-claim verification stays strict.

## Validation

Protocol regressions cover fresh complete inventory, explicit consent, active
operations/OS-lock rejection, changed/corrupt/unexpected claims, pending deletions,
competing admission, uncertain deletes, cancellation and failed completion
receipt publication. A real isolated local-provider fixture proves ordinary
repository admission is excluded during recovery, works afterward, and leaves
pack and manifest bytes untouched.

Native tests cover frozen/expired previews, private account/key bindings,
namespace/query validation, malformed listing rejection, mandatory native
approval, read-only inspection and the actual non-creating OS-lock probe.

Logs are under `state-agent/target/upload-review/lock-recovery-*`. Native
production packaging on Windows still requires its CI runner; local macOS tests
do not certify a signed Windows installer.
