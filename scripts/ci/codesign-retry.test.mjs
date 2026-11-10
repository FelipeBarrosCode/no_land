import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, join } from 'node:path';
import { test } from 'node:test';
import { codesignRetryEnv, isTransientTimestampFailure, runCodesignWithRetry } from '../apple-codesign-retry.mjs';

const argumentsForSigning = ['--force', '--sign', 'Developer ID Application: Fixture', '--timestamp', '--options', 'runtime', '--entitlements', '/test/sidecar.entitlements', '/test/Noland Connect.app/lib.dylib'];
const unavailable = 'A timestamp was expected but was not found.';
const logger = { log() {}, warn() {} };

test('Apple timestamp signing retries preserve every argument and mandatory trust setting', () => {
  const calls = [];
  const delays = [];
  runCodesignWithRetry(argumentsForSigning, {
    logger,
    sleep: delay => delays.push(delay),
    spawn(command, args, options) {
      calls.push({ command, args: [...args] });
      assert.equal(options.timeout, 120_000);
      assert.equal(options.killSignal, 'SIGKILL');
      return calls.length < 3 ? { status: 1, stderr: unavailable } : { status: 0 };
    },
  });
  assert.equal(calls.length, 3);
  assert.deepEqual(delays, [15_000, 30_000]);
  for (const { command, args } of calls) {
    assert.equal(command, '/usr/bin/codesign', 'the retry never invokes its own PATH shim');
    assert.deepEqual(args, argumentsForSigning);
    assert.ok(!args.includes('--timestamp=none'));
  }
});

test('a persistent Apple outage fails after exactly three attempts', () => {
  let calls = 0;
  assert.throws(() => runCodesignWithRetry(argumentsForSigning, {
    logger, sleep() {}, spawn() { calls++; return { status: 1, stderr: unavailable }; },
  }), /after 3\/3 attempts/);
  assert.equal(calls, 3);
});

for (const failure of ['no identity found', 'certificate revoked', 'invalid entitlements', 'unsigned code', 'errSecInternalComponent', 'The specified item could not be found in the keychain']) {
  test(`a timestamp message cannot override a permanent signing error: ${failure}`, () => {
    let calls = 0;
    assert.throws(() => runCodesignWithRetry(argumentsForSigning, {
      logger,
      sleep() { assert.fail('permanent failures do not sleep'); },
      spawn() { calls++; return { status: 1, stderr: `${failure}\n${unavailable}` }; },
    }), /after 1\/3 attempts/);
    assert.equal(calls, 1);
  });
}

for (const result of [{ status: null, signal: 'SIGTERM' }, { status: null, error: { code: 'ETIMEDOUT' } }]) {
  test(`interruption or timeout fails without retry: ${result.signal ?? result.error.code}`, () => {
    let calls = 0;
    assert.throws(() => runCodesignWithRetry(argumentsForSigning, {
      logger, sleep() { assert.fail('interrupted commands do not sleep'); },
      spawn() { calls++; return { ...result, stderr: unavailable }; },
    }));
    assert.equal(calls, 1);
  });
}

test('verification and ad-hoc signing never use the timestamp retry lane', () => {
  for (const args of [['--verify', '/app'], ['--force', '--sign', '-', '--timestamp=none', '/app'], ['--sign', 'identity', '--timestamp=none', '/app']]) {
    let calls = 0;
    assert.throws(() => runCodesignWithRetry(args, {
      logger, sleep() { assert.fail('verification must fail immediately'); },
      spawn() { calls++; return { status: 1, stderr: unavailable }; },
    }), /after 1\/1 attempts/);
    assert.equal(calls, 1);
  }
});

test('unknown errors do not retry, while recognized Apple transport diagnostics do', () => {
  assert.equal(isTransientTimestampFailure('The timestamp service is not available.'), true);
  assert.equal(isTransientTimestampFailure('Could not resolve host'), true);
  assert.equal(isTransientTimestampFailure('An unknown signing error occurred'), false);
});

test('diagnostics and terminal errors redact Apple credentials', () => {
  const output = [];
  const env = { APPLE_PASSWORD: 'fixture-password', APPLE_ID: 'fixture-account', APPLE_TEAM_ID: 'fixture-team' };
  assert.throws(() => runCodesignWithRetry(argumentsForSigning, {
    env, logger: { log: text => output.push(text), warn() {} }, sleep() {},
    spawn() { return { status: 1, stderr: 'invalid credentials fixture-password fixture-account fixture-team' }; },
  }), error => !Object.values(env).some(value => error.message.includes(value)));
  for (const value of Object.values(env)) assert.ok(!output.join('\n').includes(value));
});

test('the PATH shim preserves whitespace/apostrophes and does not mutate or duplicate PATH', t => {
  const root = mkdtempSync(join(tmpdir(), "noland codesign 'fixture-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const helper = join(root, "offline helper's.mjs");
  writeFileSync(helper, 'export function codesignCli(args) { console.log(JSON.stringify(args)); }\n');
  const original = { PATH: process.env.PATH, KEEP: 'unchanged' };
  const env = codesignRetryEnv(root, original, { helper });
  const shim = join(root, 'src-tauri', '.native-deps', 'codesign-retry-tools', 'codesign');
  assert.equal(original.PATH, process.env.PATH);
  assert.equal(env.KEEP, original.KEEP);
  assert.equal(env.PATH.split(delimiter)[0], shim.slice(0, -'/codesign'.length));
  assert.equal(codesignRetryEnv(root, env, { helper }).PATH, env.PATH);
  const args = ['--sign', "Developer ID: Test's Name", '/application with spaces.app'];
  assert.deepEqual(JSON.parse(execFileSync(process.execPath, [shim, ...args], { encoding: 'utf8', env })), args);
});

test('the real macOS Tauri build installs the retry shim before Tauri and post-build signing', () => {
  const wrapper = readFileSync(new URL('../tauri-with-mic-sidecar.mjs', import.meta.url), 'utf8');
  const macosEnv = wrapper.split("if (targetTriple.endsWith('apple-darwin')) {")[1].split("if (targetTriple.includes('linux'))")[0];
  assert.match(macosEnv, /mode === 'build' && process\.platform === 'darwin' \? codesignRetryEnv\(repoRoot, env\)/);
  assert.match(wrapper, /env: tauriEnv/);
  assert.match(wrapper, /fix-macos-bundle-deps\.mjs[\s\S]*?env: nativeEnv/);
});

test('release extraction tools are installed in the consuming package validation job', () => {
  const release = readFileSync(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8');
  const validation = release.split('\n  validation:\n')[1].split('\n  security:\n')[0];
  const packaging = release.split('\n  package-validation:\n')[1].split('\n  release:\n')[0];
  assert.doesNotMatch(validation, /squashfs-tools/);
  assert.match(packaging, /apt-get install -y --no-install-recommends squashfs-tools zstd/);
  assert.ok(packaging.indexOf('Install package extraction tools') < packaging.indexOf('Extract scannable package filesystems'));
});
