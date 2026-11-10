import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  appendFileSync, chmodSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
  readlinkSync, rmSync, symlinkSync, writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const script = fileURLToPath(new URL('./fix-appimage-metadata.sh', import.meta.url));
// Verified against the versioned upstream assets, not the moving continuous tag.
const hashes = {
  x86_64: 'ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0',
  aarch64: 'f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158',
};
const continuousHashes = {
  x86_64: 'ac7fd56feade6002c2f57db39587113275d351fce911a5fbb29ea84cd0666cab',
  aarch64: '982e935f0253741873b2b3f7d5dbf2b196d18875d13d0ae616b79cc22e5cc395',
};

function executable(path, source) {
  writeFileSync(path, `#!${process.execPath}\n${source}\n`, { mode: 0o755 });
}

function appendSquashfsHeader(path) {
  // Keep the mock runtime executable while exercising the real offset scanner.
  const header = Buffer.alloc(96);
  header.write('hsqs');
  header.writeUInt32LE(131072, 12);
  header.writeUInt16LE(6, 20);
  header.writeUInt16LE(4, 28);
  appendFileSync(path, Buffer.concat([Buffer.from('\n/* '), header, Buffer.from(' */\n')]));
}

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'appimage-metadata-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const bundle = join(root, 'bundle with spaces');
  const input = join(root, 'input AppDir');
  for (const dir of [bin, bundle, join(input, 'usr/share/applications'), join(input, 'usr/bin')]) {
    mkdirSync(dir, { recursive: true });
  }
  // Both published v0.1.23 and v0.1.27 had a root-owned 0700 SquashFS root.
  // FUSE hid that defect, while Firejail's kernel mount denied access to AppRun.
  chmodSync(input, 0o700);
  writeFileSync(join(input, 'AppRun'), '#!/bin/sh\nexit 0\n', { mode: 0o644 });
  writeFileSync(join(input, 'usr/bin/sidecar'), 'bundled sidecar', { mode: 0o755 });
  writeFileSync(join(input, 'usr/share/applications/No Land.desktop'),
    '[Desktop Entry]\nType=Application\nName=No Land\nExec=no-land\nIcon=no-land\n');
  // Tauri's root PNG may use the product name rather than the desktop Icon value.
  writeFileSync(join(input, 'No Land.png'), Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=', 'base64'));
  symlinkSync('/missing/build-workspace/No Land.png', join(input, '.DirIcon'));
  symlinkSync('/missing/build-workspace/No Land.desktop', join(input, 'No Land.desktop'));

  const prelude = `
const fs = require('node:fs');
const path = require('node:path');
const root = process.env.FIXTURE_ROOT;
function event(value) { fs.appendFileSync(path.join(root, 'events'), value + '\\n'); }
function runtimeDirectoryModes(directory) {
  // type2-runtime's mkdir_p uses 0700 without restoring directory inode modes.
  fs.chmodSync(directory, 0o700);
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory()) runtimeDirectoryModes(path.join(directory, entry.name));
  }
}
`;
  const appimage = join(bundle, 'No Land.AppImage');
  executable(appimage, `${prelude}
if (process.argv[2] !== '--appimage-extract') process.exit(2);
event('extract-input');
fs.cpSync(path.join(root, 'input AppDir'), path.join(process.cwd(), 'squashfs-root'), { recursive: true, verbatimSymlinks: true });
runtimeDirectoryModes(path.join(process.cwd(), 'squashfs-root'));
`);
  appendSquashfsHeader(appimage);
  executable(join(root, 'output-runtime'), `${prelude}
if (process.argv[2] !== '--appimage-extract') process.exit(2);
event('extract-output');
fs.cpSync(path.join(root, 'repacked'), path.join(process.cwd(), 'squashfs-root'), { recursive: true, verbatimSymlinks: true });
runtimeDirectoryModes(path.join(process.cwd(), 'squashfs-root'));
`);
  appendSquashfsHeader(join(root, 'output-runtime'));
  executable(join(bin, 'unsquashfs'), `${prelude}
const args = process.argv.slice(2);
const source = args[args.length - 1];
const offset = Number(args[args.indexOf('-o') + 1]);
const destination = args[args.indexOf('-d') + 1];
const payload = fs.readFileSync(source);
if (!args.includes('-no-progress') || !args.includes('-o') || !args.includes('-d') ||
    payload.indexOf('hsqs') !== offset || payload.readUInt16LE(offset + 28) !== 4) process.exit(2);
const input = fs.realpathSync(source) === fs.realpathSync(path.join(root, 'bundle with spaces/No Land.AppImage'));
event(input ? 'extract-input-filesystem' : 'extract-output-filesystem');
if (process.env.BAD_FILESYSTEM_EXTRACT === (input ? 'input' : 'output')) {
  console.error('SquashFS extraction failed');
  process.exit(1);
}
const original = path.join(root, input ? 'input AppDir' : 'repacked');
fs.cpSync(original, destination, { recursive: true, verbatimSymlinks: true });
fs.chmodSync(destination, fs.statSync(original).mode & 0o7777);
`);
  executable(join(root, 'repacker'), `${prelude}
event('repack ' + process.env.ARCH + ' ' + process.env.SOURCE_DATE_EPOCH);
const args = process.argv.slice(2);
fs.writeFileSync(path.join(root, 'repack-args.json'), JSON.stringify(args));
const [source, destination] = args.filter(arg => !arg.startsWith('--mksquashfs-opt='));
fs.cpSync(source, path.join(root, 'repacked'), { recursive: true, verbatimSymlinks: true });
fs.chmodSync(path.join(root, 'repacked'), fs.statSync(source).mode & 0o7777);
if (process.env.BAD_REPACK) fs.writeFileSync(path.join(root, 'repacked/.DirIcon'), 'not a PNG');
if (process.env.BAD_ROOT_REPACK) fs.chmodSync(path.join(root, 'repacked'), 0o700);
if (process.env.BAD_APPRUN_REPACK) fs.chmodSync(path.join(root, 'repacked/AppRun'), 0o700);
fs.copyFileSync(path.join(root, 'output-runtime'), destination);
`);
  executable(join(root, 'tool-runtime'), `${prelude}
if (process.argv[2] !== '--appimage-extract') process.exit(2);
event('extract-tool');
fs.mkdirSync('squashfs-root');
fs.copyFileSync(path.join(root, 'repacker'), 'squashfs-root/AppRun');
fs.chmodSync('squashfs-root/AppRun', 0o755);
`);
  executable(join(bin, 'curl'), `${prelude}
const args = process.argv.slice(2);
const url = args.find((arg) => arg.startsWith('https://'));
if (!url || !args.includes('--fail')) process.exit(2);
fs.writeFileSync(path.join(root, 'download-url'), url);
event('download ' + url);
fs.copyFileSync(path.join(root, 'tool-runtime'), args[args.indexOf('--output') + 1]);
`);
  // Model authenticated upstream assets without network access or executing Linux
  // binaries. The checker enforces both the requested release and its per-arch pin.
  executable(join(bin, 'sha256sum'), `${prelude}
const manifest = fs.readFileSync(0, 'utf8').trim();
const [, expected, tool] = manifest.match(/^([a-f0-9]{64})  (.+)$/) || [];
const url = fs.readFileSync(path.join(root, 'download-url'), 'utf8');
const arch = path.basename(tool || '').replace(/^appimagetool-/, '').replace(/\\.AppImage$/, '');
const version = url.split('/download/')[1]?.split('/')[0];
const releases = { '1.9.1': ${JSON.stringify(hashes)}, continuous: ${JSON.stringify(continuousHashes)} };
const actual = process.env.BAD_TOOL ? 'corrupt-download' : releases[version]?.[arch];
event('checksum ' + arch);
if (!actual || expected !== actual) {
  if (!process.argv.includes('--status')) console.error(tool + ': FAILED\\nsha256sum: WARNING: 1 computed checksum did NOT match');
  process.exit(1);
}
`);

  return {
    root, input, bundle, appimage,
    events: () => existsSync(join(root, 'events')) ? readFileSync(join(root, 'events'), 'utf8') : '',
    run(arch = 'x64', env = {}) {
      // macOS ships Bash 3.2. Supply only mapfile's NUL-delimited subset there;
      // Linux CI uses its real builtin. All production commands still execute.
      const harness = `
if ! type mapfile >/dev/null 2>&1; then
  mapfile() {
    [[ $# == 3 && "$1" == -d && "$2" == '' ]] || return 2
    local entry index=0
    eval "$3=()"
    while IFS= read -r -d '' entry; do
      eval "$3[index]=\\$entry"
      index=$((index + 1))
    done
    return 0
  }
  export -f mapfile
fi
exec bash "$1" "$2" "$3"
`;
      return spawnSync('bash', ['-c', harness, 'fixture', script, bundle, arch], {
        encoding: 'utf8', timeout: 10_000,
        env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, FIXTURE_ROOT: root,
          SOURCE_DATE_EPOCH: '1793440000', ...env },
      });
    },
  };
}

for (const [arch, outputArch] of [['x64', 'x86_64'], ['arm64', 'aarch64']]) {
  test(`${arch}: repairs and re-verifies metadata with a pinned tool`, { timeout: 15_000 }, (t) => {
    const f = fixture(t);
    const result = f.run(arch);
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    assert.match(result.stdout, /Repacked and verified AppImage metadata:/);
    assert.match(f.events(), new RegExp(`/download/1\\.9\\.1/appimagetool-${outputArch}\\.AppImage`));
    assert.match(f.events(), /extract-input-filesystem/);
    assert.match(f.events(), new RegExp(`checksum ${outputArch}\\nextract-tool\\nrepack ${outputArch} 1793440000\\nextract-output-filesystem`));
    assert.doesNotMatch(f.events(), /extract-(?:input|output)\n/);
    const repaired = join(f.root, 'repacked');
    assert.equal(lstatSync(repaired).mode & 0o7777, 0o755);
    for (const directory of ['usr', 'usr/bin', 'usr/share', 'usr/share/applications']) {
      assert.equal(lstatSync(join(repaired, directory)).mode & 0o7777,
        lstatSync(join(f.input, directory)).mode & 0o7777, directory);
    }
    assert.equal(lstatSync(join(repaired, 'AppRun')).mode & 0o777, 0o755);
    const args = JSON.parse(readFileSync(join(f.root, 'repack-args.json'), 'utf8'));
    assert.ok(args.includes('--mksquashfs-opt=-root-mode'));
    assert.ok(args.includes('--mksquashfs-opt=755'));
    assert.ok(lstatSync(join(repaired, '.DirIcon')).isFile());
    assert.deepEqual(readFileSync(join(repaired, '.DirIcon')), readFileSync(join(f.input, 'No Land.png')));
    assert.equal(readlinkSync(join(repaired, 'No Land.desktop')), 'usr/share/applications/No Land.desktop');
    assert.equal(readFileSync(join(repaired, 'usr/bin/sidecar'), 'utf8'), 'bundled sidecar');
    assert.equal(lstatSync(f.appimage).mode & 0o7777, 0o755);
  });
}

test('tool checksum mismatch is visible and prevents tool execution', (t) => {
  const f = fixture(t);
  const original = readFileSync(f.appimage);
  const result = f.run('x64', { BAD_TOOL: '1' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr + result.stdout, /checksum|FAILED/i);
  assert.doesNotMatch(f.events(), /extract-tool|repack /);
  assert.deepEqual(readFileSync(f.appimage), original);
});

test('rejects a missing AppRun before downloading the tool', (t) => {
  const f = fixture(t);
  rmSync(join(f.input, 'AppRun'));
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /no AppRun entry point/);
  assert.doesNotMatch(f.events(), /download/);
});

test('rejects ambiguous packaged desktop files', (t) => {
  const f = fixture(t);
  cpSync(join(f.input, 'usr/share/applications/No Land.desktop'), join(f.input, 'usr/share/applications/other.desktop'));
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Expected exactly one packaged desktop file/);
  assert.doesNotMatch(f.events(), /download/);
});

test('rejects non-PNG metadata before downloading the tool', (t) => {
  const f = fixture(t);
  writeFileSync(join(f.input, 'No Land.png'), 'not a PNG');
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.doesNotMatch(f.events(), /download/);
});

test('extracts the filesystem and rejects metadata corrupted by repacking', (t) => {
  const f = fixture(t);
  const original = readFileSync(f.appimage);
  const result = f.run('x64', { BAD_REPACK: '1' });
  assert.notEqual(result.status, 0);
  assert.match(f.events(), /extract-output-filesystem/);
  assert.doesNotMatch(result.stdout, /Repacked and verified/);
  assert.deepEqual(readFileSync(f.appimage), original);
});

test('an executable AppRun does not hide a root directory inaccessible to other users', (t) => {
  const f = fixture(t);
  chmodSync(join(f.input, 'AppRun'), 0o755);
  assert.equal(lstatSync(f.input).mode & 0o7777, 0o700);
  const result = f.run();
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.equal(lstatSync(join(f.root, 'repacked')).mode & 0o7777, 0o755);
});

test('runtime extraction permissions do not stand in for packaged directory modes', (t) => {
  const f = fixture(t);
  chmodSync(f.input, 0o755);
  const extracted = spawnSync(f.appimage, ['--appimage-extract'], {
    cwd: f.root, encoding: 'utf8', env: { ...process.env, FIXTURE_ROOT: f.root },
  });
  assert.equal(extracted.status, 0, extracted.stderr);
  assert.equal(lstatSync(join(f.root, 'squashfs-root')).mode & 0o7777, 0o700);
  assert.equal(lstatSync(join(f.root, 'squashfs-root/usr')).mode & 0o7777, 0o700);
  const result = f.run();
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.equal(lstatSync(join(f.root, 'repacked')).mode & 0o7777, 0o755);
  assert.equal(lstatSync(join(f.root, 'repacked/usr')).mode & 0o7777, 0o755);
});

for (const phase of ['input', 'output']) {
  test(`${phase} filesystem extraction failure preserves the original artifact`, (t) => {
    const f = fixture(t);
    const original = readFileSync(f.appimage);
    const result = f.run('x64', { BAD_FILESYSTEM_EXTRACT: phase });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /SquashFS extraction failed/);
    assert.deepEqual(readFileSync(f.appimage), original);
    assert.doesNotMatch(result.stdout, /Repacked and verified/);
    if (phase === 'input') assert.doesNotMatch(f.events(), /download/);
  });
}

for (const [setting, diagnostic] of [
  ['BAD_ROOT_REPACK', /AppDir root must have mode 0755/],
  ['BAD_APPRUN_REPACK', /AppRun must have mode 0755/],
]) {
  test(`rejects ${setting} after repacking without replacing the original`, (t) => {
    const f = fixture(t);
    const original = readFileSync(f.appimage);
    const result = f.run('x64', { [setting]: '1' });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, diagnostic);
    assert.deepEqual(readFileSync(f.appimage), original);
    assert.doesNotMatch(result.stdout, /Repacked and verified/);
  });
}

test('rejects unsupported architectures and ambiguous AppImages', (t) => {
  const f = fixture(t);
  assert.match(f.run('riscv64').stderr, /Unsupported AppImage architecture/);
  cpSync(f.appimage, join(f.bundle, 'other.AppImage'));
  assert.match(f.run().stderr, /Expected exactly one AppImage/);
  assert.equal(f.events(), '');
});
