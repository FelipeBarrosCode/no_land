import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { REMOTE_AGENT_FILES, verifyRemoteAgentArtifacts } from './remote-agent-artifacts.mjs';

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
