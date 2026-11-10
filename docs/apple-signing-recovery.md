# Recovering Apple release signing

Production macOS builds keep Developer ID signing, secure timestamps, hardened
runtime, entitlements, notarization, stapler validation and Gatekeeper checks.
The build's private PATH shim delegates to `/usr/bin/codesign` and retries only
timestamped signing requests with recognized transient Apple transport failures.
It keeps the original arguments unchanged, waits 15 then 30 seconds, and stops
after three attempts. Identity, certificate, entitlement and verification errors,
timeouts and interrupted processes still fail closed. There is no unsigned or
`--timestamp=none` release fallback.

This covers Tauri's initial signing and post-build dependency repair, where every
nested GStreamer library may require a timestamp. It prevents an isolated Apple
timestamp outage from forcing a full rebuild. The shim is build-host tooling and
is not shipped in the application.

Run the offline regression suite with:

```sh
node --test scripts/ci/codesign-retry.test.mjs
```

For hosted failures, inspect the failed job and rerun it only after the enclosing
workflow finishes; GitHub refuses rerunning a job in a still-running workflow.
Retries retain all existing signing credentials and trust checks. Package
filesystem validation installs its own `squashfs-tools` and `zstd` prerequisites;
installations in another job do not carry over to its fresh runner.

Large SDK downloads resume the same partial file through up to twelve attempts
within a thirty-minute deadline, with bounded exponential backoff. A truncated
HTTP response (curl exit 18) switches subsequent attempts to HTTP/1.1, as do
explicit HTTP/2 transport failures. Range failures restart safely and interruption
still fails immediately. TLS validation is never disabled.
