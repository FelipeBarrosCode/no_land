# Protected application state

`state.json` stores non-sensitive application configuration and references only.
Production state access is wrapped by `KeychainBackedStateStore`, which hydrates
secret values into the in-memory `PersistedAppState` and scrubs them before every
JSON write.

Protected values include:

- app password, Vast API key, Twitch client secret, and SSH password;
- legacy Backblaze key ID/application key and repository crypt password;
- shared-storage provider credentials, provider secret fields, OAuth sessions,
  and repository encryption keys;
- an in-progress WireGuard configuration, which may contain a private key.

Cloudflare TURN credentials and generated SSH private keys already use their own
secure-storage services and remain outside this state vault. Public keys,
fingerprints, usernames, host addresses, profile IDs, file paths, and display
preferences remain in JSON because they are not authentication secrets.

## Migration and failure behavior

On the first schema-v4 load, non-empty legacy values are written to the platform
credential vault and read back for verification before `state.json` is scrubbed.
The process is idempotent. If secure storage cannot be accessed, startup fails
without resetting or deleting the existing state file.

The mobile port uses the `mobile-port` vault namespace and the desktop No Land
app uses `desktop`, preventing one installation from deleting the other's dynamic
profile credentials when both are run on the same computer.

Dynamic shared-storage entries use SHA-256-derived account names and protected
manifests so deleting a profile or OAuth session also deletes its credential-vault
entry. No secret values are included in logs or vault account names.
