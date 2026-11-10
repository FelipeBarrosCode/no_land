import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';
import { verifyFileChecksum } from '../native-download.mjs';

const bootstrap = fs.readFileSync(new URL('../bootstrap-native-deps.mjs', import.meta.url), 'utf8');

test('Opus uses the official release asset with an independent upstream fallback', () => {
  const sentinel = new Error('stop before compiling');
  let captured;
  const source = bootstrap.slice(bootstrap.indexOf('function ensureOpus('), bootstrap.indexOf('function ensureSdl2('));
  assert.throws(() => runInNewContext(`${source}\nensureOpus('/prefix', 'linux');`, {
    join,
    existsSync: () => false,
    isWindowsTarget: () => false,
    isMacTarget: () => false,
    ensureExtractedSourceTarball(...args) { captured = args; throw sentinel; },
  }), error => error === sentinel);
  assert.deepEqual(Array.from(captured[0]), [
    'https://github.com/xiph/opus/releases/download/v1.5.2/opus-1.5.2.tar.gz',
    'https://downloads.xiph.org/releases/opus/opus-1.5.2.tar.gz',
  ]);
  assert.deepEqual(captured.slice(1), ['opus-1.5.2.tar.gz', 'opus-1.5.2']);
});

function downloader(t, downloadWithResume) {
  const root = fs.mkdtempSync(join(tmpdir(), 'noland-opus-download-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const source = bootstrap.slice(bootstrap.indexOf('function downloadFile('), bootstrap.indexOf('function prepareBuildDir('));
  const download = runInNewContext(`${source}\ndownloadFile;`, {
    ...fs, dirname, join, downloadWithResume, verifyFileChecksum, Buffer, Error, console: { log() {} },
  });
  return { destination: join(root, 'opus.tar.gz'), download };
}

test('TLS failure falls back without mixing partial bytes between hosts', t => {
  const calls = [];
  const { destination, download } = downloader(t, (url, partial) => {
    calls.push(url);
    assert.equal(fs.existsSync(partial), false);
    if (url === 'primary') {
      fs.writeFileSync(partial, 'unusable bytes');
      throw new Error('curl exit 35: TLS connection closed');
    }
    fs.writeFileSync(partial, Buffer.from([0x1f, 0x8b, 0x08]));
  });
  download(['primary', 'fallback'], destination, { expectedKind: 'tar.gz' });
  assert.deepEqual(calls, ['primary', 'fallback']);
  assert.deepEqual(fs.readFileSync(destination), Buffer.from([0x1f, 0x8b, 0x08]));
  assert.equal(fs.existsSync(`${destination}.partial`), false);
  download(['primary', 'fallback'], destination, { expectedKind: 'tar.gz' });
  assert.equal(calls.length, 2, 'valid cached archives must be reused');
});

test('all mirrors failing reports both errors without publishing a partial archive', t => {
  const { destination, download } = downloader(t, (url, partial) => {
    fs.writeFileSync(partial, 'partial');
    throw new Error(`TLS failure at ${url}`);
  });
  assert.throws(() => download(['primary', 'fallback'], destination, { expectedKind: 'tar.gz' }),
    /primary: TLS failure at primary[\s\S]*fallback: TLS failure at fallback/);
  assert.equal(fs.existsSync(destination), false);
  assert.equal(fs.existsSync(`${destination}.partial`), false);
});

test('missing output is reported cleanly and falls back instead of throwing ENOENT', t => {
  const { destination, download } = downloader(t, (url, partial) => {
    if (url === 'fallback') fs.writeFileSync(partial, Buffer.from([0x1f, 0x8b, 0x08]));
  });
  assert.throws(() => download('primary', destination, { expectedKind: 'tar.gz' }), /<missing download>/);
  assert.deepEqual(fs.readdirSync(dirname(destination)), []);
  download(['primary', 'fallback'], destination, { expectedKind: 'tar.gz' });
  assert.deepEqual(fs.readFileSync(destination), Buffer.from([0x1f, 0x8b, 0x08]));
  assert.deepEqual(fs.readdirSync(dirname(destination)), ['opus.tar.gz']);
});

test('overlapping downloads cannot remove or rename each other\'s partial file', t => {
  const partials = [];
  const bytes = Buffer.from([0x1f, 0x8b, 0x08]);
  const { destination, download } = downloader(t, (url, partial) => {
    partials.push(partial);
    fs.writeFileSync(partial, bytes);
    if (url === 'outer') {
      download('inner', destination, { expectedKind: 'tar.gz' });
      assert.deepEqual(fs.readFileSync(partial), bytes);
    }
  });
  download('outer', destination, { expectedKind: 'tar.gz' });
  assert.equal(partials.length, 2);
  assert.notEqual(partials[0], partials[1]);
  assert.deepEqual(fs.readFileSync(destination), bytes);
  assert.deepEqual(fs.readdirSync(dirname(destination)), ['opus.tar.gz']);
});

test('a truncated cached package with valid magic is replaced only after its pinned checksum passes', t => {
  const bytes = Buffer.from([0x1f, 0x8b, 0x08, 1, 2, 3, 4]);
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  let calls = 0;
  const { destination, download } = downloader(t, (_url, partial) => {
    calls += 1;
    fs.writeFileSync(partial, bytes);
  });
  fs.writeFileSync(destination, bytes.subarray(0, 3));
  download('upstream', destination, { expectedKind: 'tar.gz', sha256 });
  assert.deepEqual(fs.readFileSync(destination), bytes);
  download('upstream', destination, { expectedKind: 'tar.gz', sha256 });
  assert.equal(calls, 1, 'verified cached downloads are reused');
});

test('a corrupt successful HTTP response never becomes a completed cached package', t => {
  const { destination, download } = downloader(t, (_url, partial) => {
    fs.writeFileSync(partial, Buffer.from([0x1f, 0x8b, 0x08]));
  });
  assert.throws(() => download('upstream', destination, { expectedKind: 'tar.gz', sha256: 'a'.repeat(64) }), /invalid tar\.gz/);
  assert.deepEqual(fs.readdirSync(dirname(destination)), []);
});

test('every managed macOS/Windows SDK package has a pinned upstream checksum', () => {
  const gstreamerChecksums = JSON.parse(fs.readFileSync(new URL('../gstreamer-checksums.json', import.meta.url), 'utf8'));
  assert.equal(Object.keys(gstreamerChecksums).length, 4);
  for (const checksum of Object.values(gstreamerChecksums)) assert.match(checksum, /^[a-f0-9]{64}$/u);
  const source = bootstrap.slice(bootstrap.indexOf('function gstreamerChecksum('), bootstrap.indexOf('function downloadFile('));
  const checksum = runInNewContext(`${source}\ngstreamerChecksum;`, { gstreamerChecksums, basename: path => path.split('/').pop(), Error });
  for (const [name, expected] of Object.entries(gstreamerChecksums)) assert.equal(checksum(`/cache/${name}`), expected);
  assert.throws(() => checksum('/cache/gstreamer-unknown.pkg'), /No pinned GStreamer checksum/);
  for (const file of ['runtimePkg', 'develPkg', 'runtimeMsi', 'develMsi']) {
    assert.ok(bootstrap.includes(`sha256: gstreamerChecksum(${file})`), file);
  }
});
