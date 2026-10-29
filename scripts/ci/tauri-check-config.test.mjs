import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const workflow = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');
const shipping = JSON.parse(readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));

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

test('shipping bundles still require their real resources and remote agents', () => {
  assert.ok(Array.isArray(shipping.bundle.resources));
  assert.ok(shipping.bundle.resources.length > 0);
  assert.ok(shipping.bundle.resources.some((resource) => resource.includes('/prebuilt/')));
});
