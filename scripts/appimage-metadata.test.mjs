import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
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

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'appimage-metadata-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const bundle = join(root, 'bundle with spaces');
  const input = join(root, 'input AppDir');
  for (const dir of [bin, bundle, join(input, 'usr/share/applications'), join(input, 'usr/bin')]) {
    mkdirSync(dir, { recursive: true });
  }
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
`;
  const appimage = join(bundle, 'No Land.AppImage');
  executable(appimage, `${prelude}
if (process.argv[2] !== '--appimage-extract') process.exit(2);
event('extract-input');
fs.cpSync(path.join(root, 'input AppDir'), path.join(process.cwd(), 'squashfs-root'), { recursive: true, verbatimSymlinks: true });
`);
  executable(join(root, 'output-runtime'), `${prelude}
if (process.argv[2] !== '--appimage-extract') process.exit(2);
event('extract-output');
fs.cpSync(path.join(root, 'repacked'), path.join(process.cwd(), 'squashfs-root'), { recursive: true, verbatimSymlinks: true });
`);
  executable(join(root, 'repacker'), `${prelude}
event('repack ' + process.env.ARCH + ' ' + process.env.SOURCE_DATE_EPOCH);
fs.cpSync(process.argv[2], path.join(root, 'repacked'), { recursive: true, verbatimSymlinks: true });
if (process.env.BAD_REPACK) fs.writeFileSync(path.join(root, 'repacked/.DirIcon'), 'not a PNG');
fs.copyFileSync(path.join(root, 'output-runtime'), process.argv[3]);
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
fi
source "$1" "$2" "$3"
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
    assert.match(f.events(), new RegExp(`checksum ${outputArch}\\nextract-tool\\nrepack ${outputArch} 1793440000\\nextract-output`));
    const repaired = join(f.root, 'repacked');
    assert.equal(lstatSync(join(repaired, 'AppRun')).mode & 0o777, 0o755);
    assert.ok(lstatSync(join(repaired, '.DirIcon')).isFile());
    assert.deepEqual(readFileSync(join(repaired, '.DirIcon')), readFileSync(join(f.input, 'No Land.png')));
    assert.equal(readlinkSync(join(repaired, 'No Land.desktop')), 'usr/share/applications/No Land.desktop');
    assert.equal(readFileSync(join(repaired, 'usr/bin/sidecar'), 'utf8'), 'bundled sidecar');
    assert.ok(lstatSync(f.appimage).mode & 0o111);
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

test('re-extracts and rejects metadata corrupted by repacking', (t) => {
  const f = fixture(t);
  const result = f.run('x64', { BAD_REPACK: '1' });
  assert.notEqual(result.status, 0);
  assert.match(f.events(), /extract-output/);
  assert.doesNotMatch(result.stdout, /Repacked and verified/);
});

test('rejects unsupported architectures and ambiguous AppImages', (t) => {
  const f = fixture(t);
  assert.match(f.run('riscv64').stderr, /Unsupported AppImage architecture/);
  cpSync(f.appimage, join(f.bundle, 'other.AppImage'));
  assert.match(f.run().stderr, /Expected exactly one AppImage/);
  assert.equal(f.events(), '');
});
