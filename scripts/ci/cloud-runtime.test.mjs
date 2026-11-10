import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const read = path => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');

test('runtime image defaults to a non-root user; only the fixed exporter overrides it', () => {
  assert.match(read('runtime-images/ubuntu24/Dockerfile'), /^USER 1000:1000$/m);
  assert.match(read('scripts/runtime/build-image.sh'), /docker run --rm --user 0 .*export\.py:\/export\.py:ro/);
});

test('both new and resumed provisioning install the cloud desktop runtime', () => {
  const source = read('src-tauri/src/services/orchestration.rs');
  assert.equal((source.match(/provision_desktop_runtime\(&app, &context, &remote, &target_user\)/g) || []).length, 2);
  assert.match(source, /runtime_manager::enable\(remote, target_user\)/);
});

test('the client launch library has no runtime enable or release-signing panel', () => {
  const source = read('src/features/launch-library/LaunchLibraryModal.tsx');
  assert.doesNotMatch(source, /RuntimePanel|runtime\.unconfigured|runtime\.enable/);
});

test('cloud installer validates downloads and does not replace host graphics or libc', () => {
  const source = read('scripts/runtime/install-cloud-runtime.sh');
  assert.match(source, /sha256sum --check --status/);
  assert.match(source, /sudo -n -H -u "\$user" \/usr\/local\/bin\/noland-runtime create-shortcut/);
  assert.match(source, /chroot "\$image_stage" .*apt-get install/);
  assert.doesNotMatch(source, /NOLAND_RUNTIME_SIGNING|NOLAND_RUNTIME_PUBLIC_KEY|--insecure|--allow-unauthenticated|xhost \+/);
  assert.doesNotMatch(source, /systemctl restart|apt-get.*install.*nvidia-driver/);
});

test('launcher source is embedded in the app instead of depending on external release settings', () => {
  const source = read('src-tauri/src/services/runtime_manager.rs');
  for (const file of ['gui.py', 'Cargo.lock', 'install-cloud-runtime.sh', 'packages.txt']) assert.ok(source.includes(file));
  assert.match(source, /ssh_with_stdin/);
  assert.doesNotMatch(source, /option_env!|NOLAND_RUNTIME_AGENT_URL|release_settings/);
});

test('provisioned images contain the Wayland server ABI needed by NVIDIA EGL', () => {
  assert.match(read('runtime-images/ubuntu24/packages.txt'), /^libwayland-server0$/m);
});

test('host config overlay targets are materialized before activating a read-only image', () => {
  const source = read('scripts/runtime/install-cloud-runtime.sh');
  assert.match(source, /for name in hosts resolv\.conf localtime; do/);
  assert.match(source, /mv -f -- "\$image_stage\/etc\/\$name\.noland-new" "\$image_stage\/etc\/\$name"/);
  assert.ok(source.indexOf('for name in hosts resolv.conf localtime; do') < source.indexOf('mv -- "$image_stage" "$image"'));
});
