import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

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
    ...fs, dirname, downloadWithResume, Error, console: { log() {} },
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
