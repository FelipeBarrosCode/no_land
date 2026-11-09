# Embedded MCP and application exit

## Connect

1. Open **Settings → MCP** (or `#/settings?section=mcp`). MCP is off by default.
2. Enable MCP. This single opt-in allows a trusted local agent to manage all
   registered cloud PCs, including PCs registered later, with remote shell/root,
   desktop and ordinary app control. No per-instance or capability form is needed.
3. Click **Copy setup prompt** and paste it into your trusted local agent. It
   includes the Streamable HTTP endpoint and private bearer token; keep it out of
   source control, logs and public chats. Client configuration formats differ.
4. **Copy endpoint** and **Copy port** are available for clients configured by hand.
   The default endpoint is `http://127.0.0.1:47831/mcp`. Authentication is still
   required on every request, but no token-management form is needed.
5. Ask the agent to start with `app_context`, use explicit registered instance
   IDs, and verify results with jobs, instance status, processes and windows.

The Rust SDK (`rmcp`) and listener are linked into Noland Connect. There is no
separately installed local MCP daemon, Node server or Python server. The fixed
Python helper is sent over the app's SSH connection and runs on the Linux cloud
PC using its existing Python installation. Credentials are resolved from the
hydrated backend state, not exported from `state.json`.

The server runs while the app is minimized and stops on quit. A port conflict or
keychain error is visible in Settings; it never falls back to a public interface
or unauthenticated operation. MCP currently negotiates the stateful Streamable
HTTP protocol through revision **2025-11-25**.

## Trust and permissions

- IPv4 loopback binding, exact Host/Origin validation, bearer authentication,
  bounded request bodies and constant-time token-hash comparison.
- Access token in the existing OS-protected secret store, scoped to the app's
  state location. Only the trusted main window can copy/rotate/revoke the token,
  change grants, or answer human approvals. These commands are not MCP tools.
- **Disconnect all agents** ends current sessions and invalidates pending
  approvals, but a client holding a valid token can reconnect. **Rotate token**
  invalidates the old token. **Revoke token and disable** also disables MCP.
- Explicit instance IDs are checked against current backend registration on each
  operation. Newly registered PCs are automatically available, but arbitrary
  unregistered provider instances cannot be controlled. Disabling MCP invalidates
  sessions without stopping the cloud PCs.
- Provisioning, runtime/file mutations and lifecycle operations share backend
  guards with the UI. Busy/conflicting operations fail instead of silently
  superseding another mutation. Read-only inspection remains available.
- Spending, power changes, destruction, deletion, process signaling, local
  transfers, settings changes and destructive restores require a fresh,
  exact-arguments human approval, valid for five minutes and consumed once.
- Paid creation is bound to the searched offer/account/template/storage
  fingerprint. The accepted **estimate** is checked again before creating the
  instance; provider storage/network/other charges are not a billing guarantee.
  A newly created PC needs a separate machine grant for subsequent agent tools.
- Arbitrary SSH is **not a sandbox**. Normal commands run as the desktop user,
  whose existing sudo permissions may already permit privileged operations.
  Enabling MCP includes explicitly choosing root. Only connect agents you trust
  with those VMs. Local file transfers still require approved directories and
  exact-operation confirmation; no whole-home filesystem grant is added.
- Enabling MCP includes desktop viewing/input and can reveal private cloud-PC
  screen contents. It never automates the local desktop. Known cloud security
  prompts are marked human-only; title detection is best effort, not a complete
  security boundary. Local OS/keychain prompts and MFA remain human-controlled.
- App UI actions are explicitly registered buttons with IDs, revisions and
  capability/instance checks. Stale, undelegated and human-only actions are
  rejected. A click acknowledgement is not proof the underlying job succeeded.
- Credential-shaped fields, known current/previous app secrets, PEM private
  keys, bearer tokens and credential-bearing file payloads are redacted. Do not
  deliberately ask an unrestricted shell to exfiltrate secrets. Remote files,
  logs, window names and UI text are untrusted data, never permission grants.

## Tools and resources

The fixed tool catalog is in `src-tauri/src/mcp/catalog.rs`; no generic Tauri
`invoke` or local shell tool is exposed.

| Area | Tools |
| --- | --- |
| Context | `app_context`, `app_health`, `app_logs`, `app_events` |
| Instances | `instances_list`, `instance_inspect`, `offers_search`, `instance_create`, `instance_power`, `instance_destroy` |
| Setup/network/display | `instance_provision`, `provisioning_retry`, `provisioning_stop`, `connection_status`, `connection_repair`, `display_status`, `display_update`, `services_restart` |
| Remote execution | `ssh_exec`, `processes_list`, `process_inspect`, `process_signal` |
| Files | `files_list`, `files_read`, `files_write`, `files_mkdir`, `files_move`, `files_delete`, `files_upload`, `files_download` |
| Applications | `runtime_status`, `runtime_install`, `runtime_doctor`, `runtime_register_app`, `runtime_launch_app`, `library_list`, `library_launch` |
| Stream/settings/storage | `stream_status`, `stream_connect`, `stream_disconnect`, `stream_pair`, `settings_get`, `settings_update`, `storage_status`, `storage_backup`, `storage_restore` |
| UI/desktop | `ui_snapshot`, `ui_navigate`, `ui_action`, `dialogs_list`, `dialog_respond`, `desktop_snapshot`, `desktop_input`, `desktop_window_action` |
| Jobs | `jobs_list`, `job_get`, `job_logs`, `job_input`, `job_cancel` |

Context resources: `noland://app/context`,
`noland://instances/{id}/status`, `noland://jobs`,
`noland://jobs/{id}`, `noland://ui/dialogs`.
The `coordinate_cloud_pc` MCP prompt provides the same safe starting guidance.

### Operations and limits

- Long tools return a `jobId`. Poll `job_get`; use `job_logs` for a bounded
  256 KiB tail. Four jobs execute concurrently, with at most sixteen admitted
  active/queued jobs. Lists are returned under `items` in MCP structured results.
- Supply a stable, unique `request_id` when retrying a mutation. Its hash and
  exact-argument hash are persisted with job metadata, preventing duplicate
  dispatch across retries/restarts within the retained history. After a crash,
  previously running jobs become `interrupted`, not falsely successful.
- `job_cancel` is best effort. SSH notifies its remote supervisor; provisioning
  stops at a safe stage boundary. Other typed operations can finish their
  current operation. Cancelling never claims to roll back remote effects.
- `ssh_exec` supports bounded timeout, working directory and optional interactive
  stdin via `job_input`. SSH uses the app-managed executable/key and pins host
  keys per instance. Nonzero shell exits include the exit code and bounded output.
- Process signaling uses a PID plus boot-ID/start-time identity and Linux pidfds;
  a stale/reused PID is rejected. Remote job cancellation targets its supervisor,
  not a guessed process group from an old record.
- File reads are bounded; binary data is base64. Existing-file writes require an
  expected SHA256. Transfers support individual regular files up to 1 GiB, use
  checksums and no-overwrite publication, and require approved local directories.
  They do not grant arbitrary local filesystem access. SSH/app credential paths
  and kernel/device paths are not exposed by the file tools.
- `storage_status` supplies selectable object paths for `object_ids` in backup
  and restore. Library launch does not silently perform a missing application's
  restore through MCP; request an approved restore first.
- `desktop_snapshot` currently targets the provisioned Linux **X11** desktop.
  The runtime installer provides `wmctrl`/`xdotool`; AT-SPI information is included
  when available. Request `screenshot: true`, then read the job with `job_get` to
  receive an MCP image block. No Wayland/local-desktop automation is claimed.
- Persisted context is labeled as such; `instances_list` refreshes provider state.
  Launch/provision acceptance is not gameplay/audio/performance verification.
- Retain at most 200 job records and bounded in-memory result payloads. Local audit
  files rotate at about 2 MB and contain metadata, not commands, file contents,
  screenshots or access tokens. Logs/results are not restored after restart.

## Exit guard

Both native main-window close and OS/menu Quit prevent the event loop/window
from disappearing before confirmation. React presents **Keep app open**,
**Quit — cloud PCs keep running**, and **Manage cloud PCs**. The dialog reports
unknown/loading rental state explicitly and includes active agent jobs.

There is no bulk destruction in the quit path. Native and web presenters share
one coordinator. React acknowledges after rendering; if it cannot render, a
native fallback takes ownership. Repeated requests reuse the same pending
dialog, and stale timers cannot take over a newer request. Updater restart is
the existing explicit bypass. Closing the stream still disconnects only the
stream, not the VM.

## Focused validation

- `cargo test --manifest-path src-tauri/Cargo.toml -p noland-mcp`: real localhost
  handshake, tool/resource requests, missing token/foreign Origin/Host rejection,
  oversized request rejection, port conflicts, revocation/rotation, scope and
  instance checks, exact approvals, restart idempotency and redaction.
- `python3 -B scripts/ci/mcp-remote.test.py`: bounded/checksummed reads, write
  preconditions, symlink/credential paths, no-overwrite moves, protected deletion,
  literal filenames and invalid job IDs. No VM or credentials required.
- `node --test scripts/instance-controls.test.mjs`: quit request replay, loading
  UI, no empty-rental shortcut, native interception contract and safe retry.
- `npx tsc --noEmit` and `npm run i18n:check`: frontend/localization checks.

Packaged native close behavior on macOS/Windows/Linux, keychain UX, a real
external agent operating a live VM, and cloud desktop input remain manual
end-to-end validation items. Protocol/helper tests do not establish those results.
