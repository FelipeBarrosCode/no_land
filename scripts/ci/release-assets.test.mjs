import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'noland-release-assets-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'scripts'));
  mkdirSync(join(root, 'src-tauri'));
  copyFileSync(new URL('../prepare-release-assets.mjs', import.meta.url), join(root, 'scripts/prepare-release-assets.mjs'));
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ productName: 'Noland Connect' }));
  const bundles = new Map();
  for (const [artifact, platform, name] of [
    ['linux-x64', 'linux-x86_64', 'Noland Connect_0.1.18_amd64.AppImage'],
    ['linux-arm64', 'linux-aarch64', 'Noland Connect_0.1.18_aarch64.AppImage'],
    ['macos-x64', 'darwin-x86_64', 'Noland Connect.app.tar.gz'],
    ['macos-arm64', 'darwin-aarch64', 'Noland Connect.app.tar.gz'],
    ['windows-x64', 'windows-x86_64', 'Noland Connect_0.1.18_x64-setup.exe'],
    ['windows-arm64', 'windows-aarch64', 'Noland Connect_0.1.18_arm64-setup.exe'],
  ]) {
    const directory = join(root, 'release-assets', `tauri-${artifact}`);
    mkdirSync(directory, { recursive: true });
    const bundle = join(directory, name);
    // Opaque fixture bytes: never invoke installers, compilers or packagers.
    writeFileSync(bundle, `fixture ${platform}`);
    writeFileSync(`${bundle}.sig`, `signature-${platform}\n`);
    bundles.set(platform, bundle);
  }
  return {
    root, bundles,
    run: () => spawnSync(process.execPath, [join(root, 'scripts/prepare-release-assets.mjs'), 'v0.1.18'], {
      cwd: root, encoding: 'utf8', env: { ...process.env, GITHUB_REPOSITORY: 'example/no_land', WINDOWS_STORE_BASE_URL: '' },
    }),
  };
}

test('curated release metadata contains signed updater assets for all six platforms without macOS name collisions', t => {
  const { root, run } = fixture(t);
  const result = run();
  assert.equal(result.status, 0, result.stderr);
  const publish = join(root, 'release-publish');
  const metadata = JSON.parse(readFileSync(join(publish, 'latest.json'), 'utf8'));
  assert.equal(metadata.version, '0.1.18');
  assert.equal(Object.keys(metadata.platforms).length, 6);
  const urls = Object.values(metadata.platforms).map(platform => platform.url);
  assert.equal(new Set(urls).size, 6);
  for (const [platform, entry] of Object.entries(metadata.platforms)) {
    assert.equal(entry.signature, `signature-${platform}`);
    const url = new URL(entry.url);
    assert.equal(url.origin, 'https://github.com');
    assert.ok(url.pathname.startsWith('/example/no_land/releases/download/v0.1.18/'));
    const name = decodeURIComponent(url.pathname.split('/').pop());
    assert.ok(existsSync(join(publish, name)), name);
    assert.ok(existsSync(join(publish, `${name}.sig`)), name);
  }
  assert.ok(readdirSync(publish).includes('Noland.Connect_x64.app.tar.gz'));
  assert.ok(readdirSync(publish).includes('Noland.Connect_arm64.app.tar.gz'));
});

for (const failure of ['missing bundle', 'missing signature', 'empty signature', 'ambiguous bundle']) {
  test(`release preparation blocks a ${failure}`, t => {
    const { bundles, run } = fixture(t);
    const bundle = bundles.get('windows-aarch64');
    if (failure === 'missing bundle') rmSync(bundle);
    if (failure === 'missing signature') rmSync(`${bundle}.sig`);
    if (failure === 'empty signature') writeFileSync(`${bundle}.sig`, ' \n');
    if (failure === 'ambiguous bundle') {
      const other = bundle.replace('0.1.18', '0.1.19');
      copyFileSync(bundle, other);
      copyFileSync(`${bundle}.sig`, `${other}.sig`);
    }
    const result = run();
    assert.equal(result.status, 1);
    assert.match(result.stderr, /windows-aarch64/u);
  });
}
