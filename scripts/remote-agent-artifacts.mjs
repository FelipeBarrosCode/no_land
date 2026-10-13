import { readFileSync, mkdtempSync, mkdirSync, rmSync, copyFileSync } from 'node:fs';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';

export const REMOTE_AGENT_FILES = [
  'noland-state-agent', 'noland-lifecycle-agent', 'noland-network-agent',
  'noland-mic-receiver', 'noland_observer.bpf.o',
];

export function verifyRemoteAgentArtifacts(source) {
  for (const [arch, machine] of [['x86_64', 62], ['aarch64', 183]]) {
    const dir = join(source, 'prebuilt', arch);
    const hashes = new Map(readFileSync(join(dir, 'SHA256SUMS'), 'utf8').trim().split('\n').map((line) => {
      const match = /^([0-9a-f]{64})  ([a-zA-Z0-9_.-]+)$/.exec(line);
      if (!match) throw new Error(`Invalid ${arch} remote-agent checksum manifest`);
      return [match[2], match[1]];
    }));
    for (const name of REMOTE_AGENT_FILES) {
      const bytes = readFileSync(join(dir, name));
      if (!bytes.length || createHash('sha256').update(bytes).digest('hex') !== hashes.get(name)) {
        throw new Error(`Remote agent checksum mismatch: ${arch}/${name}`);
      }
      if (name !== 'noland_observer.bpf.o' && (bytes.length < 20 || bytes.readUInt32BE(0) !== 0x7f454c46 || bytes[4] !== 2 || bytes[5] !== 1 || bytes.readUInt16LE(18) !== machine)) {
        throw new Error(`Wrong Linux CPU architecture for ${arch}/${name}`);
      }
    }
  }
}

// linuxdeploy scans every ELF under usr/lib (including remote-only resources)
// and rewrites its RPATH. Store remote binaries as opaque archives so client
// packaging cannot modify the bytes or accidentally bundle their dependencies.
export function prepareRemoteAgentResources(source) {
  verifyRemoteAgentArtifacts(source);
  for (const arch of ['x86_64', 'aarch64']) {
    const dir = join(source, 'prebuilt', arch);
    execFileSync('tar', ['--format=ustar', '-czf', 'agents.tar.gz',
      ...REMOTE_AGENT_FILES, 'SHA256SUMS'], {
      cwd: dir,
      env: { ...process.env, COPYFILE_DISABLE: '1' },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  }
  verifyPackagedRemoteAgents(source);
}

export function verifyPackagedRemoteAgents(source) {
  const extracted = mkdtempSync(join(tmpdir(), 'noland-packed-agents-'));
  try {
    for (const arch of ['x86_64', 'aarch64']) {
      const dir = join(source, 'prebuilt', arch);
      const archive = join(dir, 'agents.tar.gz');
      const expected = [...REMOTE_AGENT_FILES, 'SHA256SUMS'].sort();
      const options = { cwd: dir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] };
      // Relative archive names also work with GNU tar on Windows, where a drive
      // letter in -f can otherwise be interpreted as a remote host.
      const entries = execFileSync('tar', ['-tzf', 'agents.tar.gz'], options).trim().split(/\r?\n/u).sort();
      if (JSON.stringify(entries) !== JSON.stringify(expected)) {
        throw new Error(`Invalid ${arch} remote-agent archive contents`);
      }
      const types = execFileSync('tar', ['-tvzf', 'agents.tar.gz'], options).trim().split(/\r?\n/u);
      if (!types.every((entry) => entry.startsWith('-'))) {
        throw new Error(`Non-regular file in ${arch} remote-agent archive`);
      }
      const dest = join(extracted, 'prebuilt', arch);
      mkdirSync(dest, { recursive: true });
      copyFileSync(archive, join(dest, '.agents.tar.gz'));
      execFileSync('tar', ['-xzf', '.agents.tar.gz'], { cwd: dest, stdio: ['ignore', 'pipe', 'pipe'] });
      if (!readFileSync(join(dest, 'SHA256SUMS')).equals(readFileSync(join(dir, 'SHA256SUMS')))) {
        throw new Error(`Remote agent archive manifest mismatch: ${arch}`);
      }
    }
    verifyRemoteAgentArtifacts(extracted);
  } finally {
    rmSync(extracted, { recursive: true, force: true });
  }
}
