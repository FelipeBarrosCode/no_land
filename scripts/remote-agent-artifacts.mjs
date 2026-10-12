import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createHash } from 'node:crypto';

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
