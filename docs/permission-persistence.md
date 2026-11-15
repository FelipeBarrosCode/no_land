# Remembered permissions

Normal restarts must retain a device's saved grants **and denials**, without
turning disabled features on or expanding their scopes.

- Desktop AI automation starts with the validated saved configuration captured
  before background maintenance begins. A busy state lock must not substitute
  defaults or broaden instance, shell, elevated, desktop or file-access scopes.
- Desktop, Android and iOS state reloads share the settings-write lock, including
  disk loading/migration. An older reload must not overwrite a newer choice.
  Failed reloads leave the current state intact.
- Notification features share one permission request. Existing native grants
  are reused; native denials are respected. Dismissed/interrupted requests are
  remembered so background events do not repeatedly open prompts. An explicit
  settings action can retry an undecided permission.
- Stored request markers are **not** authorization. Native status is queried
  rather than trusting the webview's cached `Notification.permission`.
- Notification category choices remain effective during a storage failure.
  Returned preference objects cannot mutate the stored/in-memory selection.

OS grants are per installation/device, not transferable between operating
systems. Revocation, application/signing identity changes, keychain access,
elevation and other OS-required reauthorization can still require user action.
Exact-operation confirmations for destruction, spending and other guarded
actions remain mandatory.

The pinned Tauri desktop notification backend reports `Granted` without
establishing the actual OS notification setting. OS-level notification blocking
still governs delivery; mock tests do not certify physical-device behavior.
Android VPN and macOS/iOS microphone flows already check their existing OS
grants. A remaining repeated accepted-grant dialog needs its exact text and
triggering action to identify the responsible API.

Regression coverage: `scripts/ci/permission-persistence.test.mjs` and the
`noland-mcp` Rust tests. Native OS permission/device validation is separate.
