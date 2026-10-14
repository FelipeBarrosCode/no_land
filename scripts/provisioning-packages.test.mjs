import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';

function packages(prebuilt) {
  return execFileSync('bash', ['src-tauri/scripts/bootstrap_provisioning_packages.sh'], {
    encoding: 'utf8',
    env: { ...process.env, NOLAND_PACKAGE_PLAN_ONLY: '1', NOLAND_PREBUILT_AGENTS: String(prebuilt) },
  }).trim().split('\n');
}
test('prebuilt provisioning installs only runtime prerequisites without compilers', () => {
  const plan = packages(1);
  for (const required of ['wireguard-tools', 'pipewire', 'wireplumber', 'rtkit', 'x11-utils', 'libelf1', 'gstreamer1.0-plugins-good']) {
    assert.ok(plan.includes(required));
  }
  assert.ok(!plan.includes('clang'));
  assert.ok(!plan.includes('build-essential'));
  assert.equal(plan.length, new Set(plan).size);
});
test('source fallback includes the BPF and microphone build prerequisites', () => {
  const plan = packages(0);
  for (const required of ['clang', 'llvm', 'libelf-dev', 'libgstreamer1.0-dev', 'build-essential']) {
    assert.ok(plan.includes(required));
  }
  assert.equal(plan.length, new Set(plan).size);
});
test('both provisioning paths install xclip for clipboard sync', () => {
  for (const prebuilt of [0, 1]) {
    assert.ok(packages(prebuilt).includes('xclip'));
  }
});
