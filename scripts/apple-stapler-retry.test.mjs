import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';
import { isTransientAppleNetworkFailure, runStaplerWithRetry } from './apple-stapler-retry.mjs';

const scripts = dirname(fileURLToPath(import.meta.url));
const fixture = join(scripts, 'apple-stapler-retry.fixture.mjs');
const lostConnection = 'error is Error Domain=NSURLErrorDomain Code=-1005 "The network connection was lost." api.apple-cloudkit.com\nThe validate action failed! Error 68.';
const transient = { status: 68, stderr: lostConnection };
const credentials = { APPLE_ID: 'fixture-apple@example.invalid', APPLE_PASSWORD: 'fixture-password-never-print', APPLE_TEAM_ID: 'FIXTURETEAM' };

function withFixture(plan, fn) {
  const root = mkdtempSync(join(tmpdir(), 'noland-apple-retry-test-'));
  const planPath = join(root, 'plan.json');
  const statePath = join(root, 'state.json');
  writeFileSync(planPath, JSON.stringify(plan));
  writeFileSync(statePath, '[]');
  const env = { ...process.env, ...credentials, APPLE_RETRY_FIXTURE_PLAN: planPath, APPLE_RETRY_FIXTURE_STATE: statePath };
  const logs = [];
  const delays = [];
  const calls = [];
  const spawn = (command, args, options) => {
    calls.push({ command, args, options });
    return spawnSync(process.execPath, [fixture, command, ...args], { ...options, env });
  };
  try {
    fn({ root, env, spawn, calls, logs, delays,
      history: () => JSON.parse(readFileSync(statePath, 'utf8')),
      options: { spawn, env, sleep: (ms) => delays.push(ms), logger: { log: (s) => logs.push(s), warn: (s) => logs.push(s) } },
    });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

for (const action of ['staple', 'validate']) {
  for (const extension of ['app', 'dmg']) {
    const artifact = `/fake/Fixture with spaces.${extension}`;
    const key = `xcrun stapler ${action} ${extension}`;
    test(`${action} ${extension}: transient failures retry and eventually succeed`, () => {
      withFixture({ [key]: [transient, transient, { status: 0, stdout: 'The action worked!' }] }, ({ options, calls, logs, delays }) => {
        runStaplerWithRetry(action, artifact, options);
        assert.equal(calls.length, 3);
        assert.deepEqual(delays, [15_000, 30_000]);
        assert.match(logs.join('\n'), /The action worked!/u);
        for (const call of calls) {
          assert.equal(call.command, 'xcrun');
          assert.deepEqual(call.args, ['stapler', action, '-v', artifact]);
          assert.equal(call.options.timeout, 120_000);
          assert.equal(call.options.killSignal, 'SIGKILL');
          assert.deepEqual(call.options.stdio, ['ignore', 'pipe', 'pipe']);
        }
      });
    });
    test(`${action} ${extension}: repeated transient failures exhaust the bound`, () => {
      withFixture({ [key]: [transient] }, ({ options, calls, delays }) => {
        assert.throws(() => runStaplerWithRetry(action, artifact, options), /exit 68.*after 3\/3 attempts/u);
        assert.equal(calls.length, 3);
        assert.deepEqual(delays, [15_000, 30_000]);
      });
    });
    test(`${action} ${extension}: permanent errors, even alongside network errors, never retry`, () => {
      withFixture({ [key]: [{ status: 65, stderr: `The signature is invalid.\n${lostConnection}` }, { status: 0 }] }, ({ options, calls, delays }) => {
        assert.throws(() => runStaplerWithRetry(action, artifact, options), /exit 65.*after 1\/3 attempts/u);
        assert.equal(calls.length, 1);
        assert.deepEqual(delays, []);
      });
    });
  }
}

test('successful first attempt does not sleep or retry', () => {
  withFixture({}, ({ options, calls, delays }) => {
    runStaplerWithRetry('validate', '/fake/Fixture.app', options);
    assert.equal(calls.length, 1);
    assert.deepEqual(delays, []);
  });
});

test('transient diagnostics on stdout also retry', () => {
  withFixture({ 'xcrun stapler validate app': [{ status: 68, stdout: lostConnection }, { status: 0 }] }, ({ options, calls, delays }) => {
    runStaplerWithRetry('validate', '/fake/Fixture.app', options);
    assert.equal(calls.length, 2);
    assert.deepEqual(delays, [15_000]);
  });
});

test('only explicit transient transport failures qualify; exit 68 alone is not sufficient', () => {
  for (const output of [lostConnection, 'NSURLErrorDomain Code=-1001', 'NSURLErrorDomain Code=-1003',
    'NSURLErrorDomain Code=-1004', 'NSURLErrorDomain Code=-1006', 'NSURLErrorDomain Code=-1009',
    'Connection reset by peer', 'HTTP status code: 503', 'HTTP/1.1 502 Bad Gateway', 'HTTP 429']) {
    assert.equal(isTransientAppleNetworkFailure(output), true, output);
  }
  for (const output of ['', 'The validate action failed! Error 68.', 'CloudKit query failed',
    'NSURLErrorDomain Code=-1200 secure connection failed', 'NSURLErrorDomain Code=-1202 untrusted certificate',
    'NSURLErrorDomain Code=-1011 HTTP 403', 'Ticket not found', 'No stapled ticket', 'not notarized',
    'Authentication failed; connection lost', `{"status":"Invalid"}\n${lostConnection}`, `Rejected\n${lostConnection}`]) {
    assert.equal(isTransientAppleNetworkFailure(output), false, output);
  }
});

test('unclassified code 68 fails immediately with no retry', () => {
  withFixture({ 'xcrun stapler validate app': [{ status: 68, stderr: 'The validate action failed! Error 68.' }] }, ({ options, calls, delays }) => {
    assert.throws(() => runStaplerWithRetry('validate', '/fake/Fixture.app', options), /after 1\/3 attempts/u);
    assert.equal(calls.length, 1);
    assert.deepEqual(delays, []);
  });
});

test('launch errors, signals and process timeouts fail closed even with transient-looking output', () => {
  for (const failure of [{ error: { code: 'ENOENT' } }, { error: { code: 'ETIMEDOUT' }, signal: 'SIGKILL' }, { signal: 'SIGTERM' }]) {
    let calls = 0;
    assert.throws(() => runStaplerWithRetry('validate', '/fake/Fixture.app', {
      spawn: () => { calls++; return { status: null, stderr: lostConnection, ...failure }; },
      logger: { log() {}, warn() {} },
      sleep: () => assert.fail('Must not sleep'),
    }), /after 1\/3 attempts/u);
    assert.equal(calls, 1);
  }
});

test('stdout/stderr and thrown failures never expose Apple credentials or command arguments', () => {
  const secretText = Object.values(credentials).join(' ');
  for (const status of [0, 68, 65]) {
    const outcome = { status, stdout: secretText, stderr: `${secretText}\n${status === 65 ? 'invalid signature' : lostConnection}` };
    withFixture({ 'xcrun stapler validate app': [outcome] }, ({ options, logs }) => {
      let errorText = '';
      try { runStaplerWithRetry('validate', '/fake/Fixture.app', options); } catch (error) { errorText = error.stack; }
      const output = `${logs.join('\n')}\n${errorText}`;
      for (const secret of Object.values(credentials)) assert.ok(!output.includes(secret));
      assert.match(output, /\[REDACTED\]/u);
      assert.ok(!output.includes('/fake/Fixture.app'));
    });
  }
});

test('shared CLI retries validation and returns nonzero on exhaustion or permanent failure', () => {
  for (const outcomes of [[transient, { status: 0 }], [transient], [{ status: 65, stderr: 'Invalid signature' }]]) {
    withFixture({ 'xcrun stapler validate app': outcomes }, ({ env, history }) => {
      const helper = join(scripts, 'apple-stapler-retry.mjs');
      const bootstrap = `
        import childProcess from 'node:child_process';
        import { syncBuiltinESMExports } from 'node:module';
        const realSpawn = childProcess.spawnSync;
        childProcess.spawnSync = (command, args, options) => realSpawn(process.execPath,
          [${JSON.stringify(fixture)}, command, ...args], options);
        syncBuiltinESMExports();
        Atomics.wait = () => 'timed-out';
        process.argv = [process.execPath, ${JSON.stringify(helper)}, 'validate', '/fake/Fixture.app'];
        await import(${JSON.stringify(pathToFileURL(helper).href)});
      `;
      const result = spawnSync(process.execPath, ['--input-type=module', '-e', bootstrap], { env, encoding: 'utf8', timeout: 10_000 });
      assert.equal(result.status, outcomes.length === 2 ? 0 : 1, result.stderr);
      assert.equal(history().length, outcomes.length === 2 ? 2 : outcomes[0].status === 68 ? 3 : 1);
    });
  }
});

function runNotarizationFixture(context) {
  const { root, env } = context;
  const sandboxScripts = join(root, 'scripts');
  mkdirSync(sandboxScripts);
  mkdirSync(join(root, 'src-tauri'));
  for (const name of ['notarize-macos-bundle.mjs', 'apple-stapler-retry.mjs']) {
    copyFileSync(join(scripts, name), join(sandboxScripts, name));
  }
  writeFileSync(join(root, 'src-tauri', 'tauri.conf.json'), JSON.stringify({ productName: 'Fixture', version: '1.0.0' }));
  const app = join(root, 'src-tauri', 'target', 'x86_64-apple-darwin', 'release', 'bundle', 'macos', 'Fixture.app');
  mkdirSync(join(app, 'Contents', 'MacOS'), { recursive: true });
  writeFileSync(join(app, 'Contents', 'MacOS', 'noland-connect'), 'fixture, not executable');
  // Route EVERY child process to the offline fixture, including codesign/notarytool.
  // Force the macOS branch on all test hosts; no production platform bypass is added.
  const bootstrap = `
    import childProcess from 'node:child_process';
    import { syncBuiltinESMExports } from 'node:module';
    const realSpawn = childProcess.spawnSync;
    childProcess.spawnSync = (command, args, options) => realSpawn(process.execPath,
      [${JSON.stringify(fixture)}, command, ...args], options);
    syncBuiltinESMExports();
    Object.defineProperty(process, 'platform', { value: 'darwin' });
    Atomics.wait = () => 'timed-out';
    process.argv = [process.execPath, ${JSON.stringify(join(sandboxScripts, 'notarize-macos-bundle.mjs'))}, 'x86_64-apple-darwin'];
    await import(${JSON.stringify(pathToFileURL(join(sandboxScripts, 'notarize-macos-bundle.mjs')).href)});
  `;
  return spawnSync(process.execPath, ['--input-type=module', '-e', bootstrap], {
    env: { ...env, APPLE_RETRY_FIXTURE_APP: app }, encoding: 'utf8', timeout: 20_000,
  });
}

test('notarization pipeline retries both app/DMG staple and validate before completing all trust checks', () => {
  const plan = {};
  for (const kind of ['app', 'dmg']) {
    for (const action of ['staple', 'validate']) plan[`xcrun stapler ${action} ${kind}`] = [transient, { status: 0 }];
  }
  withFixture(plan, (context) => {
    const result = runNotarizationFixture(context);
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    assert.match(result.stdout, /Notarization complete/u);
    const history = context.history();
    for (const key of Object.keys(plan)) assert.equal(history.filter((entry) => entry.key === key).length, 2, key);
    assert.equal(history.filter((entry) => entry.command === 'spctl').length, 3);
    assert.equal(history.filter((entry) => entry.command === 'codesign').length, 2);
    assert.equal(history.filter((entry) => entry.args[0] === 'notarytool' && entry.args[1] === 'submit').length, 2);
  });
});

for (const outcome of [transient, { status: 65, stderr: `Invalid signature\n${lostConnection}` }]) {
  test(`notarization pipeline stops before DMG submission on ${outcome.status === 68 ? 'exhausted' : 'permanent'} app validation failure`, () => {
    withFixture({ 'xcrun stapler validate app': [outcome] }, (context) => {
      const result = runNotarizationFixture(context);
      assert.notEqual(result.status, 0);
      const history = context.history();
      assert.equal(history.filter((entry) => entry.key === 'xcrun stapler validate app').length, outcome.status === 68 ? 3 : 1);
      assert.equal(history.filter((entry) => entry.args[0] === 'notarytool' && entry.args[1] === 'submit').length, 1);
      assert.equal(history.filter((entry) => entry.command === 'spctl').length, 0);
    });
  });
}

for (const outcome of [
  { status: 0, notaryStatus: 'Invalid' },
  { status: 1, stderr: `Invalid credentials\n${lostConnection}` },
  { status: 1, stdout: '{"status":"Invalid","id":"fixture-rejected"}', stderr: lostConnection },
]) {
  test(`notary ${outcome.notaryStatus ?? (outcome.stdout ? 'nonzero Invalid response' : 'authentication failure')} aborts without retry or stapling`, () => {
    withFixture({ 'xcrun notarytool submit app': [outcome] }, (context) => {
      const result = runNotarizationFixture(context);
      assert.notEqual(result.status, 0);
      const history = context.history();
      assert.equal(history.filter((entry) => entry.args[0] === 'notarytool' && entry.args[1] === 'submit').length, 1);
      assert.equal(history.filter((entry) => entry.args[0] === 'stapler').length, 0);
    });
  });
}

test('captured and normally streamed command diagnostics redact credentials without echoing argv', () => {
  withFixture({
    'ditto -c -k app': [{ status: 0, stdout: Object.values(credentials).join(' '), stderr: Object.values(credentials).join(' ') }],
    'xcrun notarytool submit app': [{ status: 1, stderr: `Invalid credentials ${Object.values(credentials).join(' ')}` }],
  }, (context) => {
    const result = runNotarizationFixture(context);
    assert.notEqual(result.status, 0);
    const output = `${result.stdout}\n${result.stderr}`;
    for (const secret of Object.values(credentials)) assert.ok(!output.includes(secret));
    assert.match(output, /\[REDACTED\]/u);
    assert.ok(!output.includes('--password'));
  });
});
