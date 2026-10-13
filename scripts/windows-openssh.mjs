import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

export const sshTools = ['ssh', 'scp', 'ssh-keygen'];

// Only OS-owned imports may escape the private runtime. In particular,
// libcrypto and VC redistributable DLLs must not be supplied by the build PATH.
const systemDll = /^(?:api-ms-win-.+|ext-ms-win-.+|advapi32|bcrypt|bcryptprimitives|crypt32|cryptbase|cryptsp|dbghelp|gdi32|iphlpapi|kernel32|kernelbase|msvcrt|mswsock|netapi32|normaliz|ntdll|ole32|oleaut32|powrprof|psapi|rpcrt4|secur32|setupapi|shell32|shlwapi|ucrtbase|user32|userenv|version|wevtapi|winhttp|wininet|wintrust|wldap32|ws2_32|wtsapi32)\.dll$/iu;

export function readPeRuntime(path) {
  const bytes = readFileSync(path);
  const invalid = () => { throw new Error(`Invalid PE executable: ${path}`); };
  if (bytes.length < 64 || bytes.toString('ascii', 0, 2) !== 'MZ') invalid();
  const pe = bytes.readUInt32LE(0x3c);
  if (pe + 24 > bytes.length || bytes.readUInt32LE(pe) !== 0x4550) invalid();
  const machine = bytes.readUInt16LE(pe + 4);
  const sectionCount = bytes.readUInt16LE(pe + 6);
  const optionalSize = bytes.readUInt16LE(pe + 20);
  const optional = pe + 24;
  const sections = optional + optionalSize;
  if (optionalSize < 120 || sections + sectionCount * 40 > bytes.length) invalid();
  const magic = bytes.readUInt16LE(optional);
  if (![0x10b, 0x20b].includes(magic)) invalid();
  const directories = optional + (magic === 0x20b ? 112 : 96);
  const offsetFor = (rva) => {
    for (let i = 0; i < sectionCount; i++) {
      const s = sections + i * 40;
      const address = bytes.readUInt32LE(s + 12);
      const size = bytes.readUInt32LE(s + 16);
      if (rva >= address && rva < address + size) {
        const offset = bytes.readUInt32LE(s + 20) + rva - address;
        if (offset >= bytes.length) invalid();
        return offset;
      }
    }
    invalid();
  };
  const imports = [];
  const nameAt = (rva) => {
    const start = offsetFor(rva);
    const end = bytes.indexOf(0, start);
    if (end < 0) invalid();
    return bytes.toString('ascii', start, end);
  };
  for (const [index, stride, nameField] of [[1, 20, 12], [13, 32, 4]]) {
    if (directories + index * 8 + 8 > sections) continue;
    const rva = bytes.readUInt32LE(directories + index * 8);
    if (!rva) continue;
    let offset = offsetFor(rva);
    for (;;) {
      if (offset + stride > bytes.length) invalid();
      if (bytes.subarray(offset, offset + stride).every((b) => b === 0)) break;
      // Modern delay-import descriptors use RVAs, not absolute addresses.
      if (index === 13 && !(bytes.readUInt32LE(offset) & 1)) invalid();
      imports.push(nameAt(bytes.readUInt32LE(offset + nameField)));
      offset += stride;
    }
  }
  return { machine, imports };
}

export function verifyOpenSshRuntime(root, target) {
  const machine = target.startsWith('aarch64-') ? 0xaa64 : target.startsWith('x86_64-') ? 0x8664 : null;
  if (!machine) throw new Error(`Unsupported Windows OpenSSH target: ${target}`);
  const files = new Map(readdirSync(root, { withFileTypes: true })
    .filter((entry) => entry.isFile()).map((entry) => [entry.name.toLowerCase(), entry.name]));
  const pending = sshTools.map((tool) => `${tool}.exe`);
  const seen = new Set();
  while (pending.length) {
    const name = pending.pop().toLowerCase();
    if (seen.has(name)) continue;
    seen.add(name);
    if (!files.has(name)) throw new Error(`Missing Windows OpenSSH dependency ${name} in ${root}`);
    const info = readPeRuntime(join(root, files.get(name)));
    if (info.machine !== machine) throw new Error(`Wrong architecture for ${name} in ${root}; expected ${target}`);
    for (const dependency of info.imports) {
      if (!systemDll.test(dependency)) pending.push(dependency);
    }
  }
}

export function resolveOpenSshRoot(target, srcTauriDir, env = process.env) {
  const overrides = ['NOLAND_SSH_BIN', 'NOLAND_SCP_BIN', 'NOLAND_SSH_KEYGEN_BIN']
    .map((key) => env[key]?.trim()).filter(Boolean).map((path) => resolve(dirname(path)));
  const explicit = env.NOLAND_OPENSSH_ROOT?.trim();
  if (explicit) overrides.push(resolve(explicit));
  if (new Set(overrides.map((path) => path.toLowerCase())).size > 1) {
    throw new Error('Windows OpenSSH overrides must all come from one distribution directory; set NOLAND_OPENSSH_ROOT.');
  }
  const candidates = overrides.length ? [overrides[0]] : [
    join(srcTauriDir, '.native-deps', 'cache', `openssh-${target}`, 'root'),
    join(env.SystemRoot || 'C:\\Windows', 'System32', 'OpenSSH'),
  ];
  const root = candidates.find((candidate) => sshTools.every((tool) => existsSync(join(candidate, `${tool}.exe`))));
  if (!root) throw new Error(`No complete Windows OpenSSH distribution for ${target}; set NOLAND_OPENSSH_ROOT.`);
  verifyOpenSshRuntime(root, target);
  return root;
}

export function stageOpenSshRuntime(source, destination, target) {
  verifyOpenSshRuntime(source, target);
  if (resolve(source).toLowerCase() === resolve(destination).toLowerCase()) return;
  rmSync(destination, { recursive: true, force: true });
  mkdirSync(destination, { recursive: true });
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    if (entry.isFile() && (/\.(?:exe|dll)$/iu.test(entry.name) || /license|notice/iu.test(entry.name))) {
      copyFileSync(join(source, entry.name), join(destination, entry.name));
    }
  }
  verifyOpenSshRuntime(destination, target);
}

export function cleanWindowsEnvironment(env = process.env) {
  const clean = { ...env };
  for (const key of Object.keys(clean)) {
    if (/^(?:path|NOLAND_.*|GST_.*|GSTREAMER_.*|OPENSSL_.*)$/iu.test(key)) delete clean[key];
  }
  const systemRoot = env.SystemRoot || 'C:\\Windows';
  clean.PATH = join(systemRoot, 'System32');
  return clean;
}

export function smokeTestOpenSsh(root, tempRoot) {
  const env = cleanWindowsEnvironment();
  const run = (tool, args, expected = 0) => {
    const result = spawnSync(join(root, `${tool}.exe`), args, { cwd: tempRoot, env, encoding: 'utf8', timeout: 30_000, windowsHide: true });
    if (result.error || result.status !== expected) {
      throw new Error(`Packaged ${tool} failed (${result.status}): ${result.error?.message || ''}\n${result.stderr || ''}`);
    }
    return result.stdout;
  };
  run('ssh', ['-V']);
  run('scp', [], 1); // Usage must execute successfully, without connecting anywhere.
  const key = join(tempRoot, 'test key');
  run('ssh-keygen', ['-t', 'ed25519', '-f', key, '-N', '', '-C', 'package-test']);
  const derived = run('ssh-keygen', ['-y', '-f', key]).trim();
  const stored = readFileSync(`${key}.pub`, 'utf8').trim().split(/\s+/u).slice(0, 2).join(' ');
  if (derived !== stored) throw new Error('Packaged ssh-keygen public-key round trip failed');
}
