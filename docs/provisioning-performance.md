# Provisioning performance

The new and existing-instance paths share these scheduling rules:

1. Generate and upload SSH credentials before creating a VM.
2. Poll VM readiness at 5 seconds initially, 10 seconds after a minute, and
   30 seconds after three minutes. Preserve the original wall-clock timeout.
3. Recover the package manager once and install the union of missing runtime
   prerequisites in one serialized APT transaction.
4. Deploy the state agent. Release packages contain prebuilt Linux x86_64 and
   aarch64 binaries, independently of the architecture of the client computer.
5. Validate NVIDIA and finish any driver-mismatch reboot before concurrent work.
6. Run Sunshine/display/audio and WireGuard configuration as independent lanes.
   Once WireGuard exists, deploy/configure lifecycle and network agents in
   parallel while Sunshine setup continues.
7. Join both lanes, configure the microphone, and begin local tunnel activation
   and pairing. A failed lane is drained, not abandoned, before a retry starts.

## Remote-agent artifacts

`.github/workflows/_build.yml` builds state, lifecycle, network, and microphone
agents on native Ubuntu 22.04 runners for **both x86_64 and ARM64**. It also
includes the matching eBPF object. Every desktop package includes both sets.
Installers select by the **remote VM's** `uname -m`, never the client's CPU.

Artifacts inherit the client package's distribution trust. SHA-256 manifests
detect transfer corruption; they are not a substitute for package signing.
Package validation rejects missing artifacts, checksum mismatches, and a binary
with the wrong ELF machine type. Development/source-only installations retain
the locked source-build fallback. Remote runtime packages remain necessary for
dynamically linked GStreamer and libelf dependencies.

## Safety boundaries

- APT/dpkg must not run in multiple lanes. During coordinated provisioning,
  lock waits are non-destructive; they never kill another lane's package job.
- WireGuard exclusively owns streaming firewall rules. Sunshine must not reopen
  public ports after the network lane applies tunnel-only access.
- The NVIDIA reboot barrier remains serial.
- Microphone configuration remains before pairing because it may restart
  PipeWire. It is **not** safe to run it after gameplay starts without first
  splitting compilation/installation from audio-graph activation.
- The state agent remains a prerequisite to preserve restore/backup safety.
  This implementation does not claim `Ready` while mandatory setup is pending.

## Measurement and remaining work

Structured `provisioning stage started/finished` logs report durations for the
overall flow, package transactions, state/lifecycle/network/microphone agents,
and display/network lanes. RemoteExec also records each SSH command's duration.
Sunshine's service transaction and user identity lookup batch several former
SSH round trips.

Verify on fresh and resumed x86_64/ARM64 VMs, including a driver-mismatch reboot,
stop/switch requests, failed downloads, absent artifacts, and storage restores.
Compare time from SSH availability to successful pairing; VM image download and
boot time are provider-controlled. No wall-clock improvement is guaranteed until
these real-VM runs are measured.

Further work: pre-upload artifact preparation during VM boot, a common extracted
new/resume DAG, complete per-substep persisted timing, and separate core-ready /
fully-provisioned milestones with tracked optional jobs. Those are deliberately
not implemented by detaching unowned installers from the current state machine.
