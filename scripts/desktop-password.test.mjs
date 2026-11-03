import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const script = read('src-tauri/scripts/set_desktop_password.sh');
const native = read('src-tauri/src/services/desktop_password.rs');
const secret = 'Ab9._~!@#%^*+=,:?/-';

function runFixture(overrides = {}) {
  const directory = mkdtempSync(join(tmpdir(), 'noland-password-test-'));
  const capture = join(directory, 'stdin');
  try {
    const commands = {
      id: '#!/bin/sh\n[ "${ID_FAIL:-0}" = 0 ] || exit 1\nprintf "%s\\n" "${TARGET_UID:-1000}"\n',
      chpasswd: '#!/bin/sh\ncat > "$CAPTURE"\nif [ "${PAM_FAIL:-0}" = 1 ]; then cat "$CAPTURE"; cat "$CAPTURE" >&2; exit 1; fi\n',
      passwd: '#!/bin/sh\nprintf "user %s 2026-11-03 0 99999 7 -1\\n" "${PASSWORD_STATUS:-P}"\n',
    };
    for (const [name, source] of Object.entries(commands)) {
      writeFileSync(join(directory, name), source, { mode: 0o700 });
    }
    // Only the mock chpasswd runs; no real account/password is ever changed.
    const command = `sh -c '${script.replaceAll("'", "'\\''")}' sh user`;
    const result = spawnSync('sh', ['-c', command], {
      input: `user:${secret}\n`, encoding: 'utf8', timeout: 15000,
      env: { ...process.env, PATH: `${directory}:/usr/bin:/bin`, CAPTURE: capture, ...overrides },
    });
    return { ...result, input: existsSync(capture) ? readFileSync(capture, 'utf8') : null };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test('actual nested-shell script passes exactly one unchanged password record through stdin', () => {
  const result = runFixture();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.input, `user:${secret}\n`);
  assert.equal(result.stdout, 'NOLAND_DESKTOP_PASSWORD_SET\n');
  assert.equal(result.stderr, '');
});

test('PAM rejection cannot echo the secret or claim success', () => {
  const result = runFixture({ PAM_FAIL: '1' });
  assert.equal(result.status, 43);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, '');
});

test('missing accounts and UID-zero aliases fail before any password write', () => {
  for (const settings of [{ ID_FAIL: '1' }, { TARGET_UID: '0' }]) {
    const result = runFixture(settings);
    assert.equal(result.status, 41);
    assert.equal(result.input, null);
  }
});

test('locked and passwordless accounts do not pass post-write verification', () => {
  for (const status of ['L', 'NP']) {
    const result = runFixture({ PASSWORD_STATUS: status });
    assert.equal(result.status, 44);
    assert.equal(result.stdout, '');
  }
});

test('initial setup runs on both paths; retries and legacy usable servers do not auto-rotate', () => {
  const orchestration = read('src-tauri/src/services/orchestration.rs');
  assert.equal((orchestration.match(/desktop_password::provision\(/g) ?? []).length, 2);
  assert.match(native, /desktop_password_configured\s*\|\| server\.steps\.sunshine_configured\s*\|\| server\.steps\.post_provision_completed/);
  assert.ok(native.indexOf('server.steps.desktop_password_configured = true') > native.indexOf('if result.status_code != 0'));
  assert.match(native, /validate_platform_password\(&password\)/);
  assert.match(native, /ssh_with_stdin\(&command, input,/);
  assert.doesNotMatch(native, /result\.stderr|format!\([^\n]*password/);
  assert.match(read('src-tauri/src/models/app_state.rs'), /#\[serde\(default\)\]\s*pub desktop_password_configured: bool/);
});

test('existing-server update is an explicit registered action using the saved password', () => {
  const commands = read('src-tauri/src/commands/mod.rs');
  const handler = commands.split('pub async fn apply_instance_desktop_password(')[1]?.split('\n}\n')[0];
  assert.ok(handler);
  assert.match(handler, /orchestration_guard\.lock\(\)\.await/);
  assert.match(handler, /InstanceLifecycleService::acquire_lock/);
  assert.match(handler, /InstanceLifecycleService::release_lock/);
  assert.match(handler, /build_remote_exec_for_instance\(context\.inner\(\), instance_id\)/);
  const entry = ['src-tauri/src/main.rs', 'src-tauri/src/lib.rs']
    .filter((path) => existsSync(new URL(`../${path}`, import.meta.url)))
    .map(read).join('\n');
  assert.match(entry, /apply_instance_desktop_password,/);
  assert.match(read('src/lib/backend.ts'), /"apply_instance_desktop_password", \{ instanceId \}/);
  const ui = read('src/features/settings/RemoteDesktopPasswordSettings.tsx');
  assert.match(ui, /applyInstanceDesktopPassword\(instanceId\)/);
  assert.match(ui, /busy \|\| hasUnsavedPassword \|\| pendingId !== null/);
  assert.match(ui, /\[instanceId\]: \{ ok: false/);
});
