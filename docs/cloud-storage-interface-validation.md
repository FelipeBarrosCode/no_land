# Cloud Storage interface and helper packaging

## Interface changes

- Background storage activity has a separate store slot from foreground stream
  setup/provisioning. Foreground startup, progress and cleanup cannot replace or
  clear an upload, download, direct-upload or confirmed-deletion handle.
- The floating activity panel is outside route and foreground-modal visibility
  guards. Cancellation remains bound to the exact PC and operation; deletion
  does not acquire an unsafe generic cancel button.
- Deletion still requires the original frozen preview, full-cloud-deletion
  validation and acknowledgement of dependent backups. Once accepted, its
  request runs in the background and the selection dialogs close. Closing a
  dialog never retries a deletion or creates a replacement plan.
- Failed/uncertain deletion responses retain the operation handle and actionable
  error. Paused monitoring retains confirmed counts and last-success age without
  implying completion.
- User-facing actions are **Upload games** / **Download games**. Settings retain
  **Save**. Primary navigation uses **Cloud Storage** and **AI automation**;
  backend command names, MCP protocol details and storage formats are unchanged.
- EDID controls live under server configuration. The reliability input is
  removed without changing the configured backend reliability policy. The three
  first-open dashboard guidance cards are removed.

## Release checks

Windows configurations explicitly declare both managed sidecars. CI verifies the
signed source inputs before bundling. After installation/extraction, both Windows
architectures must contain a native helper beside the application with valid
Authenticode coverage, and application/helper/microphone bytes must match the
signed-input receipt. Helper smoke execution happens only after these checks.

macOS verifies the helper's nested location, Developer ID team, trusted timestamp
and hardened runtime before creating the notarization archive. After stapling,
the app must also pass the existing nested-helper notarization/Gatekeeper checks.
A missing helper prevents submission instead of producing a helper-less release.

These changes do not run a release, submit to Apple or deploy a remote agent.
Actual signed Windows installer validation still requires its native CI runners.

## Local validation

- `npm run build`: passed (existing Vite chunk-size/import warnings).
- `npm run i18n:check`: 1,479 keys across nine locales.
- `npm run test:scripts`: 419 tests passed, including in-memory store concurrency,
  frozen deletion-plan correlation, paused/uncertain monitoring, both Windows
  architectures and offline notarization orchestration.
- `git diff --check`: passed.

Script tests use an isolated GPG home with a short prefix so agent sockets remain
within macOS's Unix socket path limit under the canonical temporary directory.
No user keyring or production signing credentials are used by these fixtures.
