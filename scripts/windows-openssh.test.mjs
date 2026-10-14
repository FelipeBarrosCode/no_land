import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { cleanWindowsEnvironment, readPeRuntime, resolveOpenSshRoot, sshTools, stageOpenSshRuntime, verifyOpenSshRuntime } from './windows-openssh.mjs';

function pe(machine = 0x8664, imports = [], delay = false) {
  const b = Buffer.alloc(2048);
  b.write('MZ');
  b.writeUInt32LE(0x80, 0x3c);
  b.writeUInt32LE(0x4550, 0x80);
  b.writeUInt16LE(machine, 0x84);
  b.writeUInt16LE(1, 0x86);
  b.writeUInt16LE(240, 0x94);
  b.writeUInt16LE(0x20b, 0x98);
  const section = 0x98 + 240;
  b.writeUInt32LE(1536, section + 8);
  b.writeUInt32LE(0x1000, section + 12);
  b.writeUInt32LE(1536, section + 16);
  b.writeUInt32LE(512, section + 20);
  if (imports.length) {
    const dir = 0x98 + 112 + (delay ? 13 : 1) * 8;
    b.writeUInt32LE(0x1000, dir);
    const stride = delay ? 32 : 20;
    let nameOffset = 1024;
    imports.forEach((name, i) => {
      const offset = 512 + i * stride;
      if (delay) b.writeUInt32LE(1, offset);
      b.writeUInt32LE(0x1000 + nameOffset - 512, offset + (delay ? 4 : 12));
      b.write(name, nameOffset);
      nameOffset += name.length + 1;
    });
  }
  return b;
}

function fixture(fn) {
  const root = mkdtempSync(join(tmpdir(), 'noland-openssh-test-'));
  try { fn(root); } finally { rmSync(root, { recursive: true, force: true }); }
}

function tools(root, machine = 0x8664, imports = []) {
  for (const tool of sshTools) writeFileSync(join(root, `${tool}.exe`), pe(machine, imports));
}

test('checks normal and delay imports', () => fixture((root) => {
  for (const delay of [false, true]) {
    const path = join(root, 'test.exe');
    writeFileSync(path, pe(0x8664, ['KERNEL32.dll', 'libcrypto.dll'], delay));
    assert.deepEqual(readPeRuntime(path), { machine: 0x8664, imports: ['KERNEL32.dll', 'libcrypto.dll'] });
  }
}));

test('rejects missing crypto and transitive DLLs, accepts case-insensitive closure', () => fixture((root) => {
  tools(root, 0x8664, ['KERNEL32.dll', 'LIBCRYPTO.dll']);
  assert.throws(() => verifyOpenSshRuntime(root, 'x86_64-pc-windows-msvc'), /libcrypto.dll/);
  writeFileSync(join(root, 'libcrypto.dll'), pe(0x8664, ['vcruntime140.dll']));
  assert.throws(() => verifyOpenSshRuntime(root, 'x86_64-pc-windows-msvc'), /vcruntime140.dll/);
  writeFileSync(join(root, 'vcruntime140.dll'), pe());
  verifyOpenSshRuntime(root, 'x86_64-pc-windows-msvc');
}));

test('rejects executable and DLL architecture mismatches', () => fixture((root) => {
  tools(root);
  assert.throws(() => verifyOpenSshRuntime(root, 'aarch64-pc-windows-msvc'), /Wrong architecture/);
  tools(root, 0xaa64, ['libcrypto.dll']);
  writeFileSync(join(root, 'libcrypto.dll'), pe());
  assert.throws(() => verifyOpenSshRuntime(root, 'aarch64-pc-windows-msvc'), /Wrong architecture/);
}));

test('stages executables, DLLs, helpers and notices together, removing stale files', () => fixture((root) => {
  const source = join(root, 'source');
  const destination = join(root, 'runtime');
  mkdirSync(source); mkdirSync(destination);
  tools(source, 0x8664, ['libcrypto.dll']);
  writeFileSync(join(source, 'libcrypto.dll'), pe());
  writeFileSync(join(source, 'ssh-sk-helper.exe'), pe());
  writeFileSync(join(source, 'LICENSE.txt'), 'license');
  writeFileSync(join(destination, 'stale.dll'), 'stale');
  stageOpenSshRuntime(source, destination, 'x86_64-pc-windows-msvc');
  assert.equal(readFileSync(join(destination, 'LICENSE.txt'), 'utf8'), 'license');
  assert.deepEqual(readPeRuntime(join(destination, 'ssh-sk-helper.exe')).imports, []);
  assert.throws(() => readFileSync(join(destination, 'stale.dll')), /ENOENT/);
  stageOpenSshRuntime(destination, destination, 'x86_64-pc-windows-msvc');
}));

test('selects one explicit distribution and rejects mixed overrides', () => fixture((root) => {
  tools(root);
  assert.equal(resolveOpenSshRoot('x86_64-pc-windows-msvc', root, { NOLAND_OPENSSH_ROOT: root }), root);
  assert.throws(() => resolveOpenSshRoot('x86_64-pc-windows-msvc', root, {
    NOLAND_SSH_BIN: join(root, 'ssh.exe'), NOLAND_SSH_KEYGEN_BIN: join(root, 'other', 'ssh-keygen.exe'),
  }), /one distribution/);
}));

test('rejects incomplete explicit roots instead of silently falling back', () => fixture((root) => {
  assert.throws(() => resolveOpenSshRoot('x86_64-pc-windows-msvc', root, { NOLAND_OPENSSH_ROOT: root }), /No complete/);
}));

test('removes build-host PATH and runtime overrides from smoke tests', () => {
  const env = cleanWindowsEnvironment({ Path: 'build-host', PATH: 'other', SystemRoot: 'Windows', NOLAND_SSH_BIN: 'host', GST_PLUGIN_PATH_1_0: 'host', OPENSSL_CONF: 'host', HOME: 'home' });
  assert.equal(env.Path, undefined);
  assert.equal(env.PATH, join('Windows', 'System32'));
  assert.equal(env.NOLAND_SSH_BIN, undefined);
  assert.equal(env.GST_PLUGIN_PATH_1_0, undefined);
  assert.equal(env.OPENSSL_CONF, undefined);
  assert.equal(env.HOME, 'home');
});

test('rejects malformed PE input', () => fixture((root) => {
  const path = join(root, 'bad.exe');
  writeFileSync(path, 'placeholder');
  assert.throws(() => readPeRuntime(path), /Invalid PE/);
}));

test('Windows x64 packages every microphone pipeline plugin, not just capture and conversion', () => {
  const config = JSON.parse(readFileSync(new URL('../src-tauri/tauri.windows.x64.conf.json', import.meta.url), 'utf8'));
  for (const plugin of ['gstcoreelements', 'gstapp', 'gstaudioconvert', 'gstaudioresample', 'gstopus', 'gstrtp', 'gstudp', 'gstwasapi']) {
    assert.ok(config.bundle.resources.some((pattern) => pattern.includes(`*${plugin}*`)), `Missing ${plugin} resource`);
  }
});
