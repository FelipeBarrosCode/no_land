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

The desktop provisioning flow is in `src-tauri/src/services/lifecycle_agent.rs` and is invoked by orchestration after the state-agent is deployed. Existing state-agent API-12 installations are deliberately rejected and reinstalled as API 13 so the trusted service identity change is applied.

## Defaults and settings

Automatic shutdown is disabled by default. The desktop settings screen exposes:

- enabled/disabled;
- inactivity timeout: 0.25–24 hours, default 3 hours;
- top application count to back up: 1–10, default 3.

The agent uses a 900–86,400 second timeout range. The configured provider action is currently `destroy`; `stop` remains supported by the daemon configuration model for future policy choices.

## Remote paths and service identity

The lifecycle service is installed as `/etc/systemd/system/noland-lifecycle-agent.service` and runs as `root` with the target user's primary group. Its runtime and durable paths are:

| Purpose | Path |
| --- | --- |
| Configuration | `/etc/noland/lifecycle/config.json` |
| Capability | `/run/noland/lifecycle/storage-capability.json` |
| Status RPC | `/run/noland/lifecycle/agent.sock` |
| Runtime database | `/var/lib/noland/lifecycle/runtime.db` |
| Activity socket | `/run/noland/sunshine-events.sock` |
| State-agent RPC | `/run/noland/state-agent.sock` |

The state-agent also runs as `root` with its existing bounded eBPF capabilities and uses the target user's group only for socket access. Its runtime directory and database are root-owned, so a compromised streaming-user process cannot replace the state-agent socket or alter the verification database. The lifecycle RPC client additionally rejects a state-agent peer whose Unix UID is not root.

The lifecycle runtime directory uses `RuntimeDirectoryPreserve=restart`. This is required because the capability is in `/run` and must survive a service restart during configuration. A host reboot removes the tmpfs capability and therefore fails safe; re-provisioning is required before autonomous shutdown can resume.

## Activity and application selection

Preferred activity input is newline-delimited JSON on `/run/noland/sunshine-events.sock`:

```json
{"type":"user_activity","source":"keyboard","timestamp_ms":123456789,"synthetic":false,"magnitude":1.0}
```

Sources are `keyboard`, `mouse`, `controller`, and `touch`. Synthetic events and controller drift below the configured dead zone do not reset inactivity. On Linux, the daemon also has a fallback scanner for Sunshine/Moonlight virtual input devices under `/dev/input`.

Application usage is attributed through state-agent active sessions and the foreground X11 window. Usage is kept in the runtime database and ranked by foreground activity, then recency/runtime tie-breakers. When the timeout is reached, the top N set is frozen transactionally. Later activity cannot change the selected set for that run.

## Backup and shutdown safety

For each frozen application, the lifecycle agent:

1. starts a `personal_state` backup through state-agent;
2. polls the operation until a known terminal state;
3. treats missing, unknown, interrupted, failed, or cancelled status as unsafe;
4. asks state-agent to verify the exact application, bundle UUID, and commit UUID;
5. retries bounded backup failures, then enters `BACKUP_FAILED_SAFE`.

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
   - `noland-state-agent.service` is `root:<target-group>`;
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
