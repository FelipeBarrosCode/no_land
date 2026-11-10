import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { devResourceConfig } from '../dev-resource-config.mjs';

const release = ['../state-agent/prebuilt/*/agents.tar.gz', '../state-agent/prebuilt/*/SHA256SUMS'];
function fixture(t, resources = ['binaries/ssh-[ax]*', '../state-agent/crates/**/*', ...release]) {
  const root = mkdtempSync(join(tmpdir(), 'noland-dev-resources-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'src-tauri'));
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ bundle: { resources, externalBin: ['native-sidecar'], macOS: { frameworks: ['GStreamer.framework'] } } }));
  return root;
}
test('dev omits only absent release archives, not SSH or source resources', t => {
  assert.deepEqual(devResourceConfig(fixture(t), 'aarch64-apple-darwin'), {
    bundle: { resources: ['binaries/ssh-[ax]*', '../state-agent/crates/**/*'] },
  });
});
test('release configuration is never relaxed', t => {
  assert.equal(devResourceConfig(fixture(t), 'aarch64-apple-darwin', [], 'build'), null);
});
test('existing prebuilt archives are retained in development', t => {
  const root = fixture(t); const arch = join(root, 'state-agent/prebuilt/x86_64');
  mkdirSync(arch, { recursive: true });
  for (const name of ['agents.tar.gz', 'SHA256SUMS']) writeFileSync(join(arch, name), 'fixture');
  assert.equal(devResourceConfig(root, 'aarch64-apple-darwin'), null);
});
test('Windows platform and CLI resource arrays replace rather than lose extras', t => {
  const root = fixture(t);
  writeFileSync(join(root, 'src-tauri/tauri.windows.conf.json'), JSON.stringify({ bundle: { resources: ['binaries/windows-runtime/**/*', ...release] } }));
  assert.deepEqual(devResourceConfig(root, 'x86_64-pc-windows-msvc'), { bundle: { resources: ['binaries/windows-runtime/**/*'] } });
  const override = JSON.stringify({ bundle: { resources: ['custom-resource', ...release] } });
  assert.deepEqual(devResourceConfig(root, 'x86_64-pc-windows-msvc', ['--config', override]), { bundle: { resources: ['custom-resource'] } });
});
test('explicitly empty or mapped resource overrides are respected', t => {
  const root = fixture(t);
  assert.equal(devResourceConfig(root, 'aarch64-apple-darwin', ['--config={"bundle":{"resources":[]}}']), null);
  const resources = { 'custom-resource': 'keep', [release[0]]: 'optional' };
  assert.deepEqual(devResourceConfig(root, 'aarch64-apple-darwin', ['--config', JSON.stringify({ bundle: { resources } })]), { bundle: { resources: { 'custom-resource': 'keep' } } });
});
