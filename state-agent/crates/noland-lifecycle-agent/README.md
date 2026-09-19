# noland-lifecycle-agent

Autonomous, fail-safe lifecycle controller for Noland Vast instances. The agent tracks local user activity and application usage, freezes the top-ranked applications after an inactivity timeout, backs up and verifies every selected application through the local state-agent, and only then asks Vast to stop or destroy the instance.

## Safety properties

- Disabled by default; enabling requires a non-zero instance ID.
- Uses only Unix-domain sockets for local activity and RPC. No TCP listener is opened.
- Uses a monotonic inactivity anchor while running and grants a full timeout after process start or a disabled-to-enabled config reload.
- Freezes each run's selected application set transactionally.
- Treats malformed, missing, unknown, interrupted, cancelled, or unverifiable backup state as unsafe.
- Never invokes the provider unless every frozen application is verified.
- Stores only operational metadata in SQLite. Storage credentials, master keys, and Vast API keys are loaded from the tmpfs capability file and are never persisted, returned by status RPC, or intentionally logged.
- The state-agent Unix peer must be root; commit verification requires exact application, bundle, and commit identities.
- Capability expiry is checked immediately before every provider attempt.

## Configuration

The default configuration file is `/etc/noland/lifecycle/config.json`. Fields use camelCase:

```json
{
  "enabled": false,
  "instanceId": 0,
  "inactivitySeconds": 10800,
  "backupAppLimit": 3,
  "controllerDeadZone": 0.15,
  "stateAgentSocket": "/run/noland/state-agent.sock",
  "statusSocket": "/run/noland/lifecycle/agent.sock",
  "activitySocket": "/run/noland/sunshine-events.sock",
  "databasePath": "/var/lib/noland/lifecycle/runtime.db",
  "capabilityPath": "/var/lib/noland/lifecycle/storage-capability.json",
  "vastBaseUrl": "https://console.vast.ai",
  "providerAction": "destroy"
}
```

`inactivitySeconds` must be 900–86400, `backupAppLimit` 1–10, and `controllerDeadZone` 0–0.95. `providerAction` is `destroy` or `stop`.

## Capability security caveat

Vast does not currently expose a narrowly scoped capability for stopping or destroying exactly one instance. The current desktop provisioning flow may therefore place a full Vast API key in the capability document. That file must be root-owned, mode `0600`, short-lived, and located on tmpfs (the default `/run` path). A future deployment should replace direct API-key provisioning with a broker that issues a single-instance, single-action capability.

## Local protocols

Activity input is newline-delimited JSON on the configured Unix stream socket. Lines are limited to 4 KiB. The status socket uses `noland-rpc` request/response envelopes and supports `GetHealth`, `GetStatus`, `RecordActivity`, and `ReloadConfig`.

The lifecycle client expects the state-agent methods `GetActiveAppSessions`, `ResolveProcessToApp`, `StartBackup`, `GetOperationStatus`, and `VerifyBackupCommit`. See `src/state_agent.rs` for exact request and response shapes. These lifecycle-facing methods must be available before enabling the agent in production.

## Workspace integration

This crate is a member of the parent `state-agent` workspace and uses the workspace's existing dependencies. It intentionally has no HTTP client dependency: Vast requests use the system `curl` binary with the bearer token supplied through curl's private stdin configuration.
