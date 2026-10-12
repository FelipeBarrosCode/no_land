# Desktop credential access

Desktop state credentials (including Shared Storage), Cloudflare TURN credentials,
and per-instance network control keys share one OS-protected item:
`noland-connect.desktop-secrets.v2` / `vault`.

Windows retains separate Credential Manager entries because its per-item size
limit is too small for a combined credential document. Those entries are cached
and unchanged saves are skipped; no Windows storage format migration is needed.

The app reads that item once per process and keeps its contents in memory. A
process-wide mutex serializes first access and updates. State saves batch changed
credentials into one vault write, verify the write, then save scrubbed state JSON.
Unchanged secrets do not trigger another OS read or write. Secrets are never moved
to plaintext files to avoid permission prompts.

## Existing installations

Existing per-credential items are imported lazily. macOS may still ask permission
for each **old** item during its first import; an app cannot combine or bypass
those existing item permissions. Old items remain in the OS credential store for
compatibility. Explicit deletion markers in the new vault prevent deleted secrets
from being imported again after restart.

After migration, all credential consumers use the same item. The actual number of
system prompts depends on Keychain lock state, code signing, and the permission
the user grants. This does not promise exactly one dialog under every macOS policy.
If an item cannot be read, or a write fails verification, the operation reports an
error rather than falling back to plaintext or replacing inaccessible credentials.

## Manual macOS verification

Use a disposable test user/Keychain and a consistently signed desktop build:

1. Start with an existing installation containing state, storage, TURN, and
   instance-control credentials. Approve old-item imports and confirm settings,
   remote control, and storage still work.
2. Restart. Confirm access is to the single vault item, not each old credential.
3. Save several settings together and repeatedly save unchanged settings. Confirm
   no per-credential prompts and no credential loss.
4. Delete a storage profile or TURN credentials and restart. Confirm deletion
   persists despite old entries still being present.
5. Deny access to the vault. Confirm a secure-storage error, no legacy fallback,
   and no secrets in `state.json`.
