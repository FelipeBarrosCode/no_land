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
  for (const file of ['gui.py', 'Cargo.lock', 'install-cloud-runtime.sh', 'packages.txt', 'elf.rs', 'maintenance.rs', 'maintain-runtime.py', 'prerequisites.json']) assert.ok(source.includes(file));
  assert.match(source, /ssh_with_stdin/);
  assert.doesNotMatch(source, /option_env!|NOLAND_RUNTIME_AGENT_URL|release_settings/);
});

test('runtime maintenance is restricted to isolated Ubuntu images and approved packages', () => {
  const helper = read('scripts/runtime/maintain-runtime.py');
  assert.match(helper, /allowed = set\(catalog\['libraries'\]\.values\(\)\)/);
  assert.match(helper, /'\/usr\/sbin\/chroot', str\(stage\), '\/usr\/bin\/apt-get'/);
  assert.doesNotMatch(helper, /--allow-unauthenticated|--allow-insecure-repositories|shell=True/);
  const gui = read('runtime-agent/src/gui.py');
  assert.match(gui, /Update Runtime/);
  assert.match(gui, /"--expected-plan", plan\["approvalDigest"\]/);
  assert.match(gui, /Install app prerequisites\?/);
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

test('runtime does not introduce CPU/GPU quotas, pinning or rendering/thread caps', () => {
  const engine = read('runtime-agent/src/engine.rs');
  const launch = engine.slice(engine.indexOf('fn bwrap('), engine.indexOf('fn check('));
  assert.doesNotMatch(launch, /"(?:taskset|nice|renice|cpulimit|systemd-run)"|CPUQuota=|CPUWeight=|CPUAffinity=|--cpus|--cpu-quota|--cpuset-cpus/);
  assert.doesNotMatch(launch, /"(?:OMP_NUM_THREADS|OPENBLAS_NUM_THREADS|GOMP_CPU_AFFINITY|CUDA_VISIBLE_DEVICES|CUDA_MPS_ACTIVE_THREAD_PERCENTAGE|LIBGL_ALWAYS_SOFTWARE|MESA_LOADER_DRIVER_OVERRIDE|DXVK_FRAME_RATE|__GL_MaxFramesAllowed)"/);
  assert.match(launch, /"--clearenv"/);
  assert.match(launch, /bind_gpu_devices\(&mut c, &gpu.devices\)/);
  assert.match(engine, /"cpu-access"/);
  for (const path of ['scripts/runtime/install-cloud-runtime.sh', 'scripts/runtime/maintain-runtime.py', 'src-tauri/src/services/runtime_manager.rs']) {
    assert.doesNotMatch(read(path), /CPUQuota=|CPUWeight=|CPUAffinity=|--cpu-quota|--cpuset-cpus|nvidia-smi\s+(?:-pl|--power-limit|-lgc|--lock-gpu-clocks)/);
  }
});
