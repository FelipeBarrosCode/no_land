import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

// Reuse the same regressions against each client's actual embedded installer.
const root = process.env.NOLAND_RCLONE_TEST_ROOT || fileURLToPath(new URL('../', import.meta.url));
const read = name => readFileSync(resolve(root, 'scripts', name), 'utf8');

test('Shared Storage rclone installer: real offline Python compatibility, integrity and admission regressions', () => {
  const result = spawnSync('python3', [fileURLToPath(new URL('./test_shared_storage_rclone.py', import.meta.url))], {
    encoding: 'utf8', timeout: 60_000, env: { ...process.env, PYTHONDONTWRITEBYTECODE: '1' },
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stderr, /Ran 27 tests/u);
});

test('agent sessions verify rclone capabilities before minting storage credentials', () => {
  const manager = read('../src-tauri/src/services/shared_storage/shared_storage_manager.rs');
  const ensure = manager.split('pub(crate) async fn ensure_rclone_installed')[1].split('/// Write the rclone filter')[0];
  assert.match(ensure, /include_str!\("\.\.\/\.\.\/\.\.\/scripts\/ensure_shared_storage_rclone\.py"\)/u);
  assert.match(ensure, /prefix, shell_escape\(script\)/u);
  assert.match(ensure, /remote\.ssh_with_stdin\(&command, input,/u);
  assert.doesNotMatch(ensure, /sudo_prefix\(\)/u, 'sudo passwords must not be interpolated into logged SSH command arguments');
  assert.match(ensure, /install\.status_code != 0/u);
  assert.match(ensure, /line == "RCLONE_READY"/u);
  assert.doesNotMatch(ensure, /apt-get install|install\.sh|RCLONE_SUDO_OK/u);
  const handoff = read('../src-tauri/src/services/shared_storage/agent_handoff.rs').split('pub async fn prepare_agent_operation')[1];
  assert.ok(handoff.indexOf('ensure_rclone_installed(remote).await?') < handoff.indexOf('mint_ephemeral_session('));
});

test('the real remote-command template keeps the multiline installer in one Python argument', { skip: process.platform === 'win32' }, () => {
  const manager = read('../src-tauri/src/services/shared_storage/shared_storage_manager.rs');
  const template = manager.match(/let command = format!\("([^"\n]+)", prefix, shell_escape\(script\)\);/u)?.[1];
  assert.ok(template, 'use the production command template, not a separately quoted test command');
  const escapeBody = manager.match(/fn shell_escape\(input: &str\) -> String \{([^}]+)\}/u)?.[1].trim();
  assert.equal(escapeBody, String.raw`input.replace('\'', "'\"'\"'")`);
  const script = read('../src-tauri/scripts/ensure_shared_storage_rclone.py')
    .replace('if __name__ == "__main__":', 'if False:') + '\nprint("RCLONE_TRANSPORT_OK")\n';
  const escaped = script.replaceAll("'", "'\"'\"'");
  for (const prefix of ['', "sudo -S -p '' "]) {
    const command = template.replace('{}', prefix).replace('{}', () => escaped);
    const sudo = 'sudo() { if [ "$1" = -S ]; then shift; shift 2; fi; "$@"; };\n';
    const result = spawnSync('sh', ['-c', sudo + command], {
      encoding: 'utf8', input: 'fixture-password\n', timeout: 10_000,
      env: { ...process.env, PYTHONDONTWRITEBYTECODE: '1' },
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), 'RCLONE_TRANSPORT_OK');
  }
});

test('rclone downloads are pinned for both remote CPU architectures and never restart services or alter cloud data', () => {
  const source = read('../src-tauri/scripts/ensure_shared_storage_rclone.py');
  assert.match(source, /VERSION = "1\.76\.0"/u);
  for (const checksum of ['b00f00b3266c3764413afe599d3ec316f6f43b3e0ea8a0bc1d186d2133e46df7',
    'ee169016675e23a69d77240565133ff7dbc9b33d16fa8f7053d4af39f1eaeb78']) assert.ok(source.includes(checksum));
  assert.ok(source.indexOf('hashlib.sha256(archive.read_bytes())') < source.indexOf('zipfile.ZipFile(archive)'));
  assert.doesNotMatch(source, /systemctl|killall|pkill|apt-get|state\.json|rclone\.conf|"(?:delete|purge|sync|copyto)"/u);
});
