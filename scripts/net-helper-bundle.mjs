import { closeSync, lstatSync, openSync, readSync, readdirSync, realpathSync, statSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';

const targets = new Map([
  ['x86_64-unknown-linux-gnu', { format: 'ELF', machine: 62 }],
  ['aarch64-unknown-linux-gnu', { format: 'ELF', machine: 183 }],
  ['x86_64-pc-windows-msvc', { format: 'PE', machine: 0x8664 }],
  ['aarch64-pc-windows-msvc', { format: 'PE', machine: 0xaa64 }],
]);

function targetInfo(target) {
  const info = targets.get(target);
  if (!info) throw new Error(`Unsupported net-helper bundle target: ${target}`);
  return info;
}

function existing(path) {
  try { lstatSync(path); return true; }
  catch (error) { if (error.code === 'ENOENT') return false; throw error; }
}

function containedFile(root, path, label) {
  const actual = realpathSync(path);
  const rel = relative(root, actual);
  if (rel === '..' || rel.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) || isAbsolute(rel)) {
    throw new Error(`${label} points outside the extracted bundle: ${path}`);
  }
  if (!statSync(actual).isFile()) throw new Error(`${label} is not a regular bundled file: ${path}`);
  return actual;
}

/** Inspect headers only. Tests can verify foreign-target payloads without executing them. */
export function verifyNetHelperBinary(path, target) {
  const expected = targetInfo(target);
  const file = openSync(path, 'r');
  try {
    const read = (size, offset) => {
      const bytes = Buffer.alloc(size);
      if (readSync(file, bytes, 0, size, offset) !== size) throw new Error(`Truncated ${expected.format} binary: ${path}`);
      return bytes;
    };
    const header = read(64, 0);
    if (expected.format === 'ELF') {
      if (!header.subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46]))
          || header[4] !== 2 || header[5] !== 1 || header[6] !== 1
          || ![2, 3].includes(header.readUInt16LE(16)) || header.readUInt16LE(52) !== 64) {
        throw new Error(`Expected a native 64-bit little-endian ELF executable for ${target}: ${path}`);
      }
      if (header.readUInt16LE(18) !== expected.machine) throw new Error(`Wrong CPU architecture for ${target}: ${path}`);
      if (process.platform !== 'win32' && (statSync(path).mode & 0o111) === 0) {
        throw new Error(`Bundled noland-net-helper/application is not executable: ${path}`);
      }
    } else {
      if (header.toString('ascii', 0, 2) !== 'MZ') throw new Error(`Expected a native Windows PE executable for ${target}: ${path}`);
      const offset = header.readUInt32LE(0x3c);
      if (offset < 64 || offset > 1024 * 1024) throw new Error(`Invalid PE header offset: ${path}`);
      const pe = read(26, offset);
      const characteristics = pe.readUInt16LE(22);
      if (pe.readUInt32LE(0) !== 0x00004550 || pe.readUInt16LE(24) !== 0x020b
          || (characteristics & 2) === 0 || (characteristics & 0x2000) !== 0
          || pe.readUInt16LE(20) < 112 || statSync(path).size < offset + 24 + pe.readUInt16LE(20)) {
        throw new Error(`Invalid or non-executable 64-bit PE payload: ${path}`);
      }
      if (pe.readUInt16LE(4) !== expected.machine) throw new Error(`Wrong CPU architecture for ${target}: ${path}`);
    }
  } finally {
    closeSync(file);
  }
}

function application(root, windows) {
  const names = new Set(windows ? ['noland-connect.exe', 'noland connect.exe'] : ['noland-connect', 'noland connect']);
  const found = new Set();
  const pending = [[root, 0]];
  let visited = 0;
  while (pending.length) {
    const [directory, depth] = pending.pop();
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (++visited > 200_000) throw new Error('Extracted bundle exceeds net-helper inspection bounds');
      const path = join(directory, entry.name);
      if (!entry.isDirectory() && names.has(entry.name.toLowerCase())) found.add(containedFile(root, path, 'Application'));
      // Do not follow directory symlinks into the build host or a dependency tree.
      if (entry.isDirectory() && depth < 12) pending.push([path, depth + 1]);
    }
  }
  if (found.size !== 1) throw new Error(`Expected exactly one packaged Noland application; found ${found.size} under ${root}`);
  return [...found][0];
}

/** Mirror release privileged-helper lookup, never PATH, a dev override or the host /usr/bin. */
export function verifyPackagedNetHelper(root, target, label = 'extracted bundle') {
  const windows = targetInfo(target).format === 'PE';
  root = realpathSync(resolve(root));
  const appExecutable = application(root, windows);
  verifyNetHelperBinary(appExecutable, target);
  const directories = new Set([dirname(appExecutable)]);
  const portable = !windows && (existing(join(root, 'AppRun')) || existing(join(root, '.DirIcon')) || basename(root).endsWith('.AppDir'));
  if (!windows && !portable) {
    for (const path of ['usr/bin', 'usr/lib/noland-connect', 'usr/lib/noland-connect/resources',
      'usr/lib/Noland Connect', 'usr/lib/Noland Connect/resources']) directories.add(join(root, path));
  }
  const names = [`noland-net-helper${windows ? '.exe' : ''}`, `noland-net-helper-${target}${windows ? '.exe' : ''}`];
  for (const directory of directories) {
    for (const name of names) {
      const candidate = join(directory, name);
      if (!existing(candidate)) continue;
      const helper = containedFile(root, candidate, 'noland-net-helper');
      // Windows also canonicalizes before enforcing the same-directory trust rule.
      const actualDirectory = dirname(helper);
      if (![...directories].some(path => existing(path) && realpathSync(path) === actualDirectory)) {
        throw new Error(`noland-net-helper resolves outside a supported installed directory in ${label}: ${candidate}`);
      }
      verifyNetHelperBinary(helper, target);
      return { appExecutable, helper, portable, target };
    }
  }
  throw new Error(`Missing packaged noland-net-helper in a production lookup location for ${target} (${label}); a build-tree or PATH copy does not count`);
}
