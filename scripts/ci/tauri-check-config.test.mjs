import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const workflow = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');
const shipping = JSON.parse(readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));

test('Tauri JavaScript packages match the locked Rust major/minor versions', () => {
  const npm = JSON.parse(readFileSync(new URL('../../package-lock.json', import.meta.url), 'utf8'));
  const cargo = readFileSync(new URL('../../src-tauri/Cargo.lock', import.meta.url), 'utf8');
  for (const [crate, name] of [
    ['tauri', '@tauri-apps/api'],
    ['tauri', '@tauri-apps/cli'],
    ['tauri-plugin-updater', '@tauri-apps/plugin-updater'],
  ]) {
    const entry = cargo.match(new RegExp(`\\[\\[package\\]\\]\\s+name = "${crate}"\\s+version = "([^"]+)"`));
    assert.ok(entry, `${crate} exists in Cargo.lock`);
    const javascript = npm.packages[`node_modules/${name}`]?.version;
    assert.ok(javascript, `${name} exists in package-lock.json`);
    assert.equal(javascript.split('.').slice(0, 2).join('.'), entry[1].split('.').slice(0, 2).join('.'), name);
  }
});

test('check and both clippy lanes do not require release bundle artifacts', () => {
  const overrides = [...workflow.matchAll(/TAURI_CONFIG: '([^'\n]+)'/g)]
    .map((match) => JSON.parse(match[1]));
  assert.equal(overrides.length, 3);
  for (const override of overrides) {
    assert.equal(override.build.frontendDist, 'ci-dist');
    assert.deepEqual(override.bundle.externalBin, []);
    assert.deepEqual(override.bundle.resources, []);
  }
});

test('release desktop tests exclude only packaging resources and keep native coverage', () => {
  const fullSuite = readFileSync(new URL('../../.github/workflows/_tests.yml', import.meta.url), 'utf8');
  const desktop = fullSuite.split('\n  desktop-tests:\n')[1]?.split('\n  state-agent-tests:\n')[0];
  assert.ok(desktop, 'desktop release-test job exists');
  const step = desktop.split('      - name: Run desktop workspace tests\n')[1];
  assert.ok(step, 'desktop workspace tests remain enabled');
  const override = step.match(/TAURI_CONFIG: '([^'\n]+)'/);
  assert.ok(override, 'release tests override packaging-only resources');
  assert.deepEqual(JSON.parse(override[1]), { bundle: { resources: [] } });
  assert.match(step, /run: cargo test --locked --manifest-path src-tauri\/Cargo\.toml --workspace --all-targets/);
  assert.match(desktop, /run: node scripts\/prepare-mic-sidecar\.mjs dev/);
  assert.doesNotMatch(desktop, /NOLAND_SKIP_NATIVE_BUILD|NOLAND_SKIP_APP_BUILD_RS/);
});

test('shipping bundles still require their real resources and remote agents', () => {
  assert.ok(Array.isArray(shipping.bundle.resources));
  assert.ok(shipping.bundle.resources.length > 0);
  assert.ok(shipping.bundle.resources.some((resource) => resource.includes('/prebuilt/')));
});
