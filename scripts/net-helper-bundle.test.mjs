import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { verifyNetHelperBinary, verifyPackagedNetHelper } from './net-helper-bundle.mjs';

const targets = ['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'];

// Header fixtures are deliberately not runnable. Native payload smoke tests use
// the real helper's read-only `check` command in verify-bundled-sidecars.mjs.
function binary(target) {
  const windows = target.includes('windows');
  const bytes = Buffer.alloc(512);
  if (windows) {
    bytes.write('MZ'); bytes.writeUInt32LE(64, 0x3c); bytes.write('PE\0\0', 64);
    bytes.writeUInt16LE(target.startsWith('aarch64') ? 0xaa64 : 0x8664, 68);
    bytes.writeUInt16LE(240, 84); bytes.writeUInt16LE(2, 86); bytes.writeUInt16LE(0x020b, 88);
  } else {
    bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1]); bytes.writeUInt16LE(3, 16);
    bytes.writeUInt16LE(target.startsWith('aarch64') ? 183 : 62, 18); bytes.writeUInt16LE(64, 52);
  }
  return bytes;
}

function fixture(t, target, { portable = false, resources = false, suffixed = false } = {}) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'noland net-helper bundle ')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const payload = join(root, portable ? 'Noland.AppDir' : 'installed payload');
  const windows = target.includes('windows');
  const appDirectory = windows ? join(payload, 'Program Files', 'Noland Connect') : join(payload, 'usr/bin');
  const helperDirectory = resources ? join(payload, 'usr/lib/noland-connect/resources') : appDirectory;
  mkdirSync(appDirectory, { recursive: true }); mkdirSync(helperDirectory, { recursive: true });
  const app = join(appDirectory, `noland-connect${windows ? '.exe' : ''}`);
  const helper = join(helperDirectory, `noland-net-helper${suffixed ? `-${target}` : ''}${windows ? '.exe' : ''}`);
  writeFileSync(app, binary(target), { mode: 0o755 }); writeFileSync(helper, binary(target), { mode: 0o755 });
  if (portable) writeFileSync(join(payload, 'AppRun'), '#!/bin/sh\n', { mode: 0o755 });
  return { root, payload, app, helper };
}

for (const target of targets) {
  test(`${target}: the shipped helper is native and reachable beside the installed app, including spaces`, t => {
    const f = fixture(t, target);
    assert.equal(verifyPackagedNetHelper(f.payload, target).helper, f.helper);
  });
  test(`${target}: the target-suffixed sidecar name is recognized`, t => {
    const f = fixture(t, target, { suffixed: true });
    assert.equal(verifyPackagedNetHelper(f.payload, target).helper, f.helper);
  });
  test(`${target}: a helper present only in the build directory does not satisfy bundle inclusion`, t => {
    const f = fixture(t, target);
    const stage = join(f.root, 'build tree', 'binaries', `noland-net-helper-${target}${target.includes('windows') ? '.exe' : ''}`);
    mkdirSync(dirname(stage), { recursive: true }); writeFileSync(stage, binary(target), { mode: 0o755 }); rmSync(f.helper);
    assert.throws(() => verifyPackagedNetHelper(f.payload, target), /Missing packaged noland-net-helper/u);
  });
  test(`${target}: recursive source/resource copies cannot masquerade as an installed helper`, t => {
    const f = fixture(t, target); const nested = join(f.payload, 'unrelated/resources/deep', 'noland-net-helper' + (target.includes('windows') ? '.exe' : ''));
    mkdirSync(dirname(nested), { recursive: true }); writeFileSync(nested, binary(target), { mode: 0o755 }); rmSync(f.helper);
    assert.throws(() => verifyPackagedNetHelper(f.payload, target), /production lookup location/u);
  });
  test(`${target}: wrong-architecture helper payloads fail even with the correct filename`, t => {
    const f = fixture(t, target);
    writeFileSync(f.helper, binary(target.startsWith('aarch64') ? target.replace('aarch64', 'x86_64') : target.replace('x86_64', 'aarch64')));
    assert.throws(() => verifyPackagedNetHelper(f.payload, target), /Wrong CPU architecture/u);
  });
  test(`${target}: placeholder scripts, truncated files and foreign binary formats fail`, t => {
    const f = fixture(t, target);
    for (const bytes of [Buffer.alloc(0), binary(target).subarray(0, 50), Buffer.from('#!/bin/sh\n# debug placeholder npm run tauri:dev\nexit 0\n'.padEnd(128)),
      binary(target.includes('windows') ? 'x86_64-unknown-linux-gnu' : 'x86_64-pc-windows-msvc')]) {
      writeFileSync(f.helper, bytes);
      assert.throws(() => verifyPackagedNetHelper(f.payload, target), /Truncated|Expected|Invalid/u);
    }
  });
  test(`${target}: a wrong-architecture main app cannot make a mismatched bundle pass`, t => {
    const f = fixture(t, target);
    writeFileSync(f.app, binary(target.startsWith('aarch64') ? target.replace('aarch64', 'x86_64') : target.replace('x86_64', 'aarch64')));
    assert.throws(() => verifyPackagedNetHelper(f.payload, target), /Wrong CPU architecture/u);
  });
}

for (const target of targets.filter(target => target.includes('linux'))) {
  test(`${target}: deb/rpm resources layouts supported by the privileged resolver are accepted`, t => {
    const f = fixture(t, target, { resources: true });
    assert.equal(verifyPackagedNetHelper(f.payload, target).helper, f.helper);
  });
  test(`${target}: AppImage/AppDir includes its own executable helper without depending on host-installed tools`, t => {
    const f = fixture(t, target, { portable: true });
    assert.equal(verifyPackagedNetHelper(f.payload, target).helper, f.helper);
    rmSync(f.helper);
    assert.throws(() => verifyPackagedNetHelper(f.payload, target), /Missing packaged noland-net-helper/u);
  });
  test(`${target}: portable images cannot rely on an absolute host resources-directory fallback`, t => {
    const f = fixture(t, target, { portable: true, resources: true });
    assert.throws(() => verifyPackagedNetHelper(f.payload, target), /production lookup location/u);
  });
  test(`${target}: executable permissions survive packaging`, { skip: process.platform === 'win32' }, t => {
    const f = fixture(t, target); chmodSync(f.helper, 0o644);
    assert.throws(() => verifyPackagedNetHelper(f.payload, target), /not executable/u);
  });
  test(`${target}: a helper symlink to the build host is not a bundled payload`, { skip: process.platform === 'win32' }, t => {
    const f = fixture(t, target); const outside = join(f.root, 'outside-helper');
    writeFileSync(outside, binary(target), { mode: 0o755 }); rmSync(f.helper); symlinkSync(outside, f.helper);
    assert.throws(() => verifyPackagedNetHelper(f.payload, target), /outside the extracted bundle/u);
  });
}

test('Windows PE validation rejects a DLL renamed to .exe and malformed header offsets', t => {
  const target = 'x86_64-pc-windows-msvc'; const f = fixture(t, target); const bytes = binary(target);
  bytes.writeUInt16LE(0x2002, 86); writeFileSync(f.helper, bytes);
  assert.throws(() => verifyNetHelperBinary(f.helper, target), /non-executable/u);
  bytes.writeUInt32LE(0xffffffff, 0x3c); writeFileSync(f.helper, bytes);
  assert.throws(() => verifyNetHelperBinary(f.helper, target), /Invalid PE header offset/u);
});

test('unsupported targets are rejected rather than silently skipping net-helper coverage', () => {
  assert.throws(() => verifyPackagedNetHelper('.', 'unknown-platform'), /Unsupported net-helper bundle target/u);
});

test('native artifact checks use the packaged helper and its read-only check command in every shipped format', () => {
  const source = readFileSync(new URL('./verify-bundled-sidecars.mjs', import.meta.url), 'utf8');
  assert.match(source, /import .*verifyPackagedNetHelper.*from '.\/net-helper-bundle\.mjs'/u);
  assert.match(source, /verifyPackagedNetHelper\(root, targetTriple, label\)/u);
  assert.match(source, /run\(helper, \['check', '--config', configPath\]/u);
  for (const label of ['deb package', 'rpm package', 'AppImage', 'MSI package', 'installed NSIS package']) assert.ok(source.includes(label), label);
  assert.ok((source.match(/verifyNetHelperConfiguration\(/gu) || []).length >= 5, 'AppImage/AppDir, deb, rpm and Windows payloads execute the helper check');
});
