import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { nextReleaseVersion } from './release-version.mjs';

test('production and rehearsal use the highest exact stable semver, not prerelease or draft runtime tags', () => {
  const tags = ['v0.1.9', 'v0.1.19', 'v0.1.18', 'v99.0.0-beta.1', 'v50.0.0+metadata',
    'v1.0.0-runtime', 'runtime-ubuntu24', 'v00.9.0', 'v0.01.20', 'v0.1.99.extra', ''];
  assert.equal(nextReleaseVersion('0.1.17', tags), '0.1.20');
  assert.equal(nextReleaseVersion('1.0.0', tags), '1.0.1');
  assert.equal(nextReleaseVersion('0.1.17', []), '0.1.18');
  assert.equal(nextReleaseVersion('0.1.17', ['v0.2.0', 'v0.1.999']), '0.2.1');
  assert.equal(nextReleaseVersion('0.1.17', ['v2.0.0', 'v1.99.99']), '2.0.1');
});

test('version calculation handles many tags without shell pipefail and rejects invalid configured versions', () => {
  assert.equal(nextReleaseVersion('0.1.17', Array.from({ length: 20_000 }, (_, n) => `v0.1.${n}`)), '0.1.20000');
  for (const invalid of ['', 'v1.2.3', '1.2', '01.2.3', '1.2.3-beta', '1.2.3\n']) {
    assert.throws(() => nextReleaseVersion(invalid, []), /stable configured release version/);
  }
});

test('both workflows call the same version calculator after fetching all tags', () => {
  for (const name of ['release', 'nightly']) {
    const workflow = readFileSync(new URL(`../../.github/workflows/${name}.yml`, import.meta.url), 'utf8');
    const version = workflow.split('\n  version:\n')[1].split('\n  build:\n')[0];
    assert.match(version, /fetch-depth: 0/u);
    assert.match(version, /node-version: 22/u);
    assert.match(version, /run: node scripts\/ci\/release-version\.mjs/u);
    assert.doesNotMatch(version, /git tag --list|sort -V|head -n/u);
  }
});

test('setting the release version keeps npm package and lockfile root metadata in sync', t => {
  const root = mkdtempSync(join(tmpdir(), 'noland-release-version-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'src-tauri'));
  for (const path of ['package.json', 'src-tauri/tauri.conf.json']) {
    writeFileSync(join(root, path), JSON.stringify({ version: '0.1.17' }));
  }
  writeFileSync(join(root, 'package-lock.json'), JSON.stringify({
    version: '0.1.17', packages: { '': { name: 'noland-connect', version: '0.1.17' }, 'node_modules/fixture': { version: '1.0.0' } },
  }));
  execFileSync(process.execPath, [fileURLToPath(new URL('../set-release-version.mjs', import.meta.url)), 'v0.1.18'], { cwd: root });
  for (const path of ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json']) {
    assert.equal(JSON.parse(readFileSync(join(root, path), 'utf8')).version, '0.1.18');
  }
  const lock = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8'));
  assert.equal(lock.packages[''].version, '0.1.18');
  assert.equal(lock.packages['node_modules/fixture'].version, '1.0.0');
});
