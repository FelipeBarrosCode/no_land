# Automatic Backup and Shutdown

No Land's automatic backup/shutdown feature is a remote, fail-safe lifecycle subsystem. The desktop app only provisions configuration and credentials; the gaming instance performs monitoring, backup, verification, and the final provider action. This is intentional: the desktop can go offline without interrupting an in-progress backup.

## Runtime architecture

```text
Sunshine/Moonlight input
        |
        v
noland-lifecycle-agent (root systemd service)
        |
        +-- SQLite WAL runtime state
        +-- state-agent Unix RPC
        |     +-- active sessions and app attribution
        |     +-- backup operation polling
        |     +-- exact commit verification
        |     +-- the only repository writer
        |
        +-- Vast HTTPS API (only after all apps verify)
        |
        +-- local status/activity Unix sockets
```

The daemon is `state-agent/crates/noland-lifecycle-agent`. It is a separate process from both Sunshine and `noland-state-agent`; a monitor failure must not stop streaming.

The desktop provisioning flow is in `src-tauri/src/services/lifecycle_agent.rs` and is invoked by orchestration after the state-agent is deployed. Older state-agent installations are deliberately rejected and reinstalled as API 17 so lifecycle selection can be validated against the state-agent's canonical backup-candidate index and catalog bundle heads.

## Defaults and settings

Automatic shutdown is disabled by default. The desktop settings screen exposes:

- enabled/disabled;
- inactivity timeout: 5 minutes–24 hours, default 3 hours;
- top application count to back up: 1–10, default 3.

Newly provisioned instances always receive a **disabled** lifecycle configuration, regardless of the saved desktop toggle. Automatic backup and shutdown only activate after the user explicitly enables them from the settings screen, which applies the configuration to the provisioned instances. Re-enabling after a terminal safe failure is likewise an explicit settings action.

The agent uses a 300–86,400 second timeout range (5 minutes to 24 hours). The configured provider action is currently `destroy`; `stop` remains supported by the daemon configuration model for future policy choices.

## Remote paths and service identity

The lifecycle service is installed as `/etc/systemd/system/noland-lifecycle-agent.service` and runs as `root` with the target user's primary group. Its runtime and durable paths are:

| Purpose | Path |
| --- | --- |
| Configuration | `/etc/noland/lifecycle/config.json` |
| Capability | `/var/lib/noland/lifecycle/storage-capability.json` |
| Status RPC | `/run/noland/lifecycle/agent.sock` |
| Runtime database | `/var/lib/noland/lifecycle/runtime.db` |
| Activity socket | `/run/noland/sunshine-events.sock` |
| State-agent RPC | `/run/noland/state-agent.sock` |

The state-agent runs as the target desktop user with only `CAP_BPF` and `CAP_PERFMON`. This lets restore publication obey the same ownership boundary as the streamed desktop without granting `CAP_DAC_OVERRIDE`. Its runtime directory and database are owned by that user. The lifecycle service remains the root-owned policy boundary and only connects to the configured local state-agent socket.

The capability is stored in the durable lifecycle state directory with root-only file permissions, so it survives service restarts and host reboots. Re-provisioning is still required after the instance's persistent state is replaced or when the capability expires.

## Activity and application selection

Preferred activity input is newline-delimited JSON on `/run/noland/sunshine-events.sock`:

```json
{"type":"user_activity","source":"keyboard","timestamp_ms":123456789,"synthetic":false,"magnitude":1.0}
```

Sources are `keyboard`, `mouse`, `controller`, and `touch`. Synthetic events and controller drift below the configured dead zone do not reset inactivity. On Linux, the daemon also has a fallback scanner for Sunshine/Moonlight virtual input devices under `/dev/input`.

Application usage is attributed through state-agent active sessions and the foreground X11 window. Only canonical, persisted identities from the state-agent shared-storage index may enter the ranking; synthetic executable fallbacks and desktop plumbing such as Plasma are excluded. Usage is kept in the runtime database and ranked by foreground activity, then recency/runtime tie-breakers. When the timeout is reached, the ranking is revalidated against the canonical index and the top N valid set is frozen transactionally. Later activity cannot change the selected set for that run.

## Backup and shutdown safety

For each frozen application, the lifecycle agent:

1. reads the application's committed catalog history and starts a non-empty `complete_application` baseline when none exists; after that baseline, it writes `personal_state` overlays through the same state-agent/shared-storage repository;
2. polls the operation until a known terminal state;
3. treats missing, unknown, interrupted, failed, or cancelled status as unsafe;
4. asks state-agent to verify the exact application, bundle UUID, and commit UUID;
5. retries bounded backup failures, then enters `BACKUP_FAILED_SAFE`.

The selected mode is frozen across retries so a just-committed baseline cannot cause an unverified retry to switch to a state-only snapshot. Restore applies the latest complete baseline first, then the latest personal-state bundle only when that bundle was captured after the baseline. Empty complete bundles are rejected, and launchability is checked against the actual executable, desktop entry, or Steam installation rather than remembered catalog metadata alone.

The Vast action is unreachable unless every frozen application is in `VERIFIED` state. The capability is revalidated immediately before every provider attempt, including retries. Provider failures end in `SHUTDOWN_FAILED_SAFE`; they do not cause an unsafe retry loop or a local desktop dependency.

A restart with an unfinished run resumes only known persisted work. An indeterminate provider operation is not treated as success. The configured Vast stop/destroy calls are expected to be idempotent, and the implementation treats already-gone instance responses as complete.

## Capability security

The capability contains the rclone session, repository key, and the Vast credential needed by the instance. It is root-owned, mode `0600`, and stored on `/run` (tmpfs); it is never written to the lifecycle database, status RPC, or normal logs. Capability bytes are streamed over SSH stdin into root-only randomized staging rather than copied through a workload-user-owned temporary file. The installer verifies hashes before installation.

Vast currently does not provide a known instance-scoped stop/destroy token. The MVP therefore provisions the full Vast API key inside this root-only capability. This remains an account-wide authority if the VM root is compromised. A broker-issued, single-instance/single-action token is the required hardening follow-up. Capability expiry is currently 30 days; expiry causes a safe failure rather than a provider call.

The deployed state-agent source is extracted into a root-owned timestamped directory and exposed through the root-owned `/opt/noland/state-agent` symlink. The lifecycle installer validates that the symlink resolves under `/opt/noland/state-agent-*` and that the source tree has no non-root-owned or group/world-writable content before building.

## Failure behavior

The following conditions must never stop or destroy the instance:

- no ranked applications;
- incomplete or failed backup;
- exact commit mismatch;
- a selected application disappearing from the shared-storage index;
- state-agent unavailable or untrusted peer;
- expired/missing capability;
- lifecycle service restart or crash;
- provider timeout/failure;
- desktop app or WebSocket/SSH connection offline;
- inability to persist local desktop settings.

Settings rollout is transactional from a safety perspective. If any instance cannot be configured, the app attempts to configure every provisioned instance as disabled, including the instance whose update failed. It records `disabled_after_error` when that succeeds and `disable_pending` when any remote disable cannot be confirmed. Orchestration treats lifecycle provisioning errors as blocking, rather than silently leaving an enabled remote agent behind.

## Manual validation on a disposable instance

Do not run the final destroy test against a valuable instance. Use a disposable instance and a mock provider or a temporary `stop` action first.

1. Deploy state-agent and lifecycle assets through the desktop provisioning flow.
2. Confirm:
   - `noland-state-agent.service` is `<target-user>:<target-group>` and retains only `CAP_BPF` and `CAP_PERFMON`;
   - `noland-lifecycle-agent.service` is `root:<target-group>`;
   - the two socket directories are not writable by the streaming user;
   - the lifecycle status socket is `/run/noland/lifecycle/agent.sock`;
   - the capability is `root:root` mode `0600` and exists only while configured/enabled.
3. Query `GetHealth` and `GetStatus` over the lifecycle Unix socket. Verify the expected instance ID and enabled state.
4. Send representative activity events and verify they reset inactivity without erasing accumulated app usage.
5. Verify ranking and frozen top-N selection with two or more known applications.
6. Run the state-agent backup tests and confirm an exact bundle/commit mismatch blocks the provider.
7. Exercise failure cases: no apps, failed operation, unknown operation, missing capability, expired capability, and provider timeout. Every case must end in a safe state with zero provider calls where backup verification is incomplete.
8. With `tc netem`, exercise latency, packet loss, burst loss, and a full outage. Confirm monitoring continues if the desktop disconnects.
9. For the end-to-end policy test, use a short disposable timeout, keep the session inactive until the timeout, verify all selected backups, and confirm only then that the mock/temporary provider action is called.
10. Repeat after a lifecycle-agent restart and verify the capability and persisted run behavior. Re-provision after a host reboot because `/run` is tmpfs.

Useful checks on the instance:

```sh
systemctl --no-pager --full status noland-state-agent.service noland-lifecycle-agent.service
stat -c '%U:%G %a %n' /run/noland /run/noland/state-agent.sock /run/noland/lifecycle /run/noland/lifecycle/agent.sock
journalctl -u noland-lifecycle-agent.service --no-pager
```

Never interpret the existence of a socket file alone as readiness; use `GetHealth` and verify the instance ID/configuration state.
