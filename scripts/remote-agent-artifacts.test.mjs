import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, cpSync, chmodSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { REMOTE_AGENT_FILES, verifyRemoteAgentArtifacts, prepareRemoteAgentResources, verifyPackagedRemoteAgents } from './remote-agent-artifacts.mjs';

function fixture(t, wrongArchitecture = false) {
  const source = mkdtempSync(join(tmpdir(), 'noland-agent-test-'));
  t.after(() => rmSync(source, { recursive: true }));
  for (const [arch, machine] of [['x86_64', 62], ['aarch64', 183]]) {
    const dir = join(source, 'prebuilt', arch);
    mkdirSync(dir, { recursive: true });
    const sums = [];
    for (const name of REMOTE_AGENT_FILES) {
      const bytes = Buffer.alloc(64);
      bytes.writeUInt32BE(0x7f454c46, 0);
      bytes[4] = 2;
      bytes[5] = 1;
      bytes.writeUInt16LE(wrongArchitecture ? 62 : machine, 18);
      writeFileSync(join(dir, name), bytes);
      sums.push(`${createHash('sha256').update(bytes).digest('hex')}  ${name}`);
    }
    writeFileSync(join(dir, 'SHA256SUMS'), `${sums.join('\n')}\n`);
  }
  return source;
}

test('requires complete, checksummed bundles for both remote architectures', (t) => {
  verifyRemoteAgentArtifacts(fixture(t));
});
test('rejects a corrupt remote executable', (t) => {
  const source = fixture(t);
  writeFileSync(join(source, 'prebuilt/aarch64/noland-state-agent'), 'corrupt');
  assert.throws(() => verifyRemoteAgentArtifacts(source), /checksum mismatch/);
});
test('rejects x86 executables in an ARM bundle even with valid checksums', (t) => {
  assert.throws(() => verifyRemoteAgentArtifacts(fixture(t, true)), /CPU architecture/);
});
test('rejects missing ARM artifacts', (t) => {
  const source = fixture(t);
  rmSync(join(source, 'prebuilt/aarch64/noland-mic-receiver'));
  assert.throws(() => verifyRemoteAgentArtifacts(source), /ENOENT/);
});

test('packaged archives preserve checksums when only opaque resources are shipped', (t) => {
  const source = fixture(t);
  prepareRemoteAgentResources(source);
  const packaged = mkdtempSync(join(tmpdir(), 'noland-packaged-agent-test-'));
  t.after(() => rmSync(packaged, { recursive: true }));
  for (const arch of ['x86_64', 'aarch64']) {
    const dest = join(packaged, 'prebuilt', arch);
    mkdirSync(dest, { recursive: true });
    for (const name of ['agents.tar.gz', 'SHA256SUMS']) {
      cpSync(join(source, 'prebuilt', arch, name), join(dest, name));
    }
    // Simulate a client's ELF rewrite. It cannot affect the archived resource.
    writeFileSync(join(source, 'prebuilt', arch, 'noland-state-agent'), 'rewritten-rpath');
  }
  verifyPackagedRemoteAgents(packaged);
});

test('does not accept corrupted executable bytes inside a well-formed archive', (t) => {
  const source = fixture(t);
  prepareRemoteAgentResources(source);
  const dir = join(source, 'prebuilt', 'x86_64');
  writeFileSync(join(dir, 'noland-state-agent'), 'corrupt');
  execFileSync('tar', ['-czf', 'agents.tar.gz', ...REMOTE_AGENT_FILES, 'SHA256SUMS'], { cwd: dir });
  assert.throws(() => verifyPackagedRemoteAgents(source), /checksum mismatch/);
});

test('rejects a missing packaged archive or mismatched external manifest', (t) => {
  const source = fixture(t);
  prepareRemoteAgentResources(source);
  const manifest = join(source, 'prebuilt', 'aarch64', 'SHA256SUMS');
  writeFileSync(manifest, `${readFileSync(manifest, 'utf8')}\n`);
  assert.throws(() => verifyPackagedRemoteAgents(source), /manifest mismatch/);
  rmSync(join(source, 'prebuilt', 'x86_64', 'agents.tar.gz'));
  assert.throws(() => verifyPackagedRemoteAgents(source));
});

test('rejects unexpected archive entries before extracting them', (t) => {
  const source = fixture(t);
  prepareRemoteAgentResources(source);
  const dir = join(source, 'prebuilt', 'x86_64');
  writeFileSync(join(dir, 'unexpected'), 'not an agent');
  execFileSync('tar', ['-czf', 'agents.tar.gz', ...REMOTE_AGENT_FILES, 'SHA256SUMS', 'unexpected'], { cwd: dir });
  assert.throws(() => verifyPackagedRemoteAgents(source), /archive contents/);
});

test('all desktop configuration overlays ship archives and manifests, not raw ELF resources', () => {
  for (const name of ['tauri.conf.json', 'tauri.windows.x64.conf.json', 'tauri.windows.arm64.conf.json']) {
    const config = JSON.parse(readFileSync(new URL(`../src-tauri/${name}`, import.meta.url), 'utf8'));
    const resources = config.bundle.resources;
    assert.ok(resources.includes('../state-agent/prebuilt/*/agents.tar.gz'), name);
    assert.ok(resources.includes('../state-agent/prebuilt/*/SHA256SUMS'), name);
    assert.ok(!resources.includes('../state-agent/prebuilt/**/*'), name);
    assert.ok(resources.includes('../vm-cloud-mic-agent/Cargo.lock'), name);
  }
});

test('remote bootstrap expands resources before checking or installing agents', () => {
  const script = readFileSync(new URL('../state-agent/scripts/bootstrap-remote.sh', import.meta.url), 'utf8');
  const unpack = script.indexOf('tar --no-same-owner --no-same-permissions -xzf "$PREBUILT/agents.tar.gz"');
  const verify = script.indexOf('sha256sum -c SHA256SUMS');
  const install = script.indexOf('install -m 0755 "$PREBUILT/noland-state-agent"');
  assert.ok(unpack >= 0 && verify > unpack && install > verify);
});

test('remote archive extraction strips writable build-machine permissions', (t) => {
  const source = fixture(t);
  const dir = join(source, 'prebuilt', 'x86_64');
  chmodSync(join(dir, 'noland-lifecycle-agent'), 0o777);
  execFileSync('tar', ['-czf', 'agents.tar.gz', ...REMOTE_AGENT_FILES, 'SHA256SUMS'], { cwd: dir });
  const dest = join(source, 'extracted');
  mkdirSync(dest);
  cpSync(join(dir, 'agents.tar.gz'), join(dest, 'agents.tar.gz'));
  const bootstrap = readFileSync(new URL('../state-agent/scripts/bootstrap-remote.sh', import.meta.url), 'utf8');
  const umask = bootstrap.match(/^umask \d+$/m)?.[0];
  const unpack = bootstrap.match(/^  tar .*agents\.tar\.gz.*$/m)?.[0];
  assert.ok(umask && unpack);
  assert.ok(unpack.includes('--no-same-owner'));
  execFileSync('bash', ['-c', `${umask}\n${unpack}`], { env: { ...process.env, PREBUILT: dest } });
  const metadata = statSync(join(dest, 'noland-lifecycle-agent'));
  assert.equal(metadata.mode & 0o022, 0);
  assert.equal(metadata.mode & 0o111, 0o111);
  if (process.getuid) assert.equal(metadata.uid, process.getuid());
});

test('cached deployment reuse rejects untrusted ownership and writable content', (t) => {
  const runtime = readFileSync(new URL('../src-tauri/src/services/shared_storage/agent_runtime.rs', import.meta.url), 'utf8');
  const probe = runtime.match(/const TRUSTED_WORKSPACE_PROBE: &str = r#"([\s\S]*?)"#;/)?.[1];
  assert.ok(probe);
  const root = mkdtempSync(join(tmpdir(), 'noland-workspace-probe-'));
  t.after(() => rmSync(root, { recursive: true }));
  const link = join(root, 'state-agent');
  execFileSync('ln', ['-s', join(root, 'state-agent-123'), link]);
  // Stub Linux-specific metadata commands so the actual probe's control flow
  // can be exercised on macOS too. The symlink check uses the real filesystem.
  const script = `
stat() { printf '%s\\n' "\${PROBE_OWNER:-0}"; }
readlink() { printf '%s\\n' /opt/noland/state-agent-123; }
find() {
  case "$*" in
    *-maxdepth*) printf '%s' "\${PROBE_WRITABLE_PARENT:-}" ;;
    *) printf '%s' "\${PROBE_UNTRUSTED:-}" ;;
  esac
}
${probe.replace('link=/opt/noland/state-agent', `link="${link}"`)}
`;
  const run = (extra = {}) => execFileSync('sh', ['-c', script], { env: { ...process.env, ...extra } });
  run();
  assert.throws(() => run({ PROBE_OWNER: '501' }));
  assert.throws(() => run({ PROBE_UNTRUSTED: '/opt/noland/state-agent-123/prebuilt/x86_64/noland-lifecycle-agent' }));
  assert.throws(() => run({ PROBE_WRITABLE_PARENT: '/opt/noland' }));
  assert.ok(runtime.includes('shell_escape(&format!("{TRUSTED_WORKSPACE_PROBE}\\n{manifest_probe}"))'));
});
