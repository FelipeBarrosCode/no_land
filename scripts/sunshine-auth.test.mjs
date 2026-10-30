import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { promisify } from 'node:util';

// Exercise the actual remote curl template without compiling Rust or contacting a host.
const run = promisify(execFile);
const sunshine = readFileSync(new URL('../src-tauri/src/services/sunshine.rs', import.meta.url), 'utf8');
const setup = readFileSync(new URL('../src-tauri/src/services/post_wireguard_setup.rs', import.meta.url), 'utf8');
const bootstrap = sunshine.split('async fn bootstrap_web_credentials(')[1]?.split('async fn verify_web_credentials(')[0];
const verification = sunshine.split('async fn verify_web_credentials(')[1]?.split('async fn detect_gpu_output(')[0];
assert.ok(bootstrap, 'Sunshine credential bootstrap exists');
assert.ok(verification, 'Sunshine credential verification exists');
const template = verification.match(/let verify_command = format!\(\s*("(?:\\.|[^"\\])*")/);
assert.ok(template, 'read the production curl command');
// Decode Rust's apostrophe escape before parsing the JSON-compatible escapes.
const curlTemplate = JSON.parse(template[1].replaceAll("\\'", "'")).replaceAll('{{', '{').replaceAll('}}', '}');
const escapeCredential = (value) => value.replaceAll("'", "'\"'\"'");

async function verify(endpoint, username = 'test-user', password = 'test-password') {
  const command = curlTemplate
    .replace('https://localhost:47990/api/config', endpoint)
    .replace('{escaped_username}', () => escapeCredential(username))
    .replace('{escaped_password}', () => escapeCredential(password));
  try {
    const result = await run('sh', ['-c', command], {
      timeout: 5000,
      env: { ...process.env, http_proxy: 'http://127.0.0.1:1', no_proxy: '', NO_PROXY: '' },
    });
    return { ...result, statusCode: 0 };
  } catch (error) {
    if (typeof error.code !== 'number') throw error;
    return { stdout: error.stdout, stderr: error.stderr, statusCode: error.code };
  }
}

async function fixture(t, handler) {
  const server = createServer(handler);
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  t.after(() => new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve())));
  return `http://127.0.0.1:${server.address().port}/api/config`;
}

function setupFunction(name) {
  const start = setup.indexOf(`async fn ${name}(`);
  assert.ok(start >= 0, `${name} exists`);
  const end = setup.indexOf('\n}', start);
  assert.ok(end > start, `${name} has a body`);
  return setup.slice(start, end + 2);
}

test('credential writes precede service startup, and verification follows startup', () => {
  const write = sunshine.indexOf('self.bootstrap_web_credentials(');
  const restart = sunshine.indexOf('let restart_service_cmd =', write);
  const restartCheck = sunshine.indexOf('.stdout.contains("SERVICE_RESTARTED")', restart);
  const verify = sunshine.indexOf('self.verify_web_credentials(', restartCheck);
  assert.ok(write >= 0 && restart > write && restartCheck > restart && verify > restartCheck);
  assert.match(bootstrap, /sunshine --creds/);
  assert.doesNotMatch(bootstrap, /let verify_command/);
});

test('production verification requires curl success and exactly HTTP 200', () => {
  assert.match(verification, /if verify\.status_code != 0 \|\| verify\.stdout\.trim\(\) != "200"\s*\{/);
  assert.match(curlTemplate, /--output \/dev\/null --write-out '%\{http_code\}'/);
  assert.match(curlTemplate, /--connect-timeout 10 --max-time 20/);
  assert.match(curlTemplate, /--noproxy '\*'/);
  assert.doesNotMatch(curlTemplate, /(?:^|\s)(?:-L|--location)(?:\s|$)|CREDS_OK|CREDS_FAIL/);
});

test('real curl rejects redirects, missing auth, and server errors', async (t) => {
  let status = 200;
  let requests = [];
  const endpoint = await fixture(t, (request, response) => {
    requests.push(request.url);
    // Following a redirect would return a successful HTML welcome page.
    response.writeHead(request.url === '/api/config' ? status : 200,
      status >= 300 && status < 400 ? { Location: '/welcome' } : {});
    response.end(request.url === '/api/config' ? '{"status":true}' : '<html>Welcome</html>');
  });
  for (const code of [200, 204, 301, 302, 303, 307, 308, 401, 403, 404, 500, 503]) {
    await t.test(`HTTP ${code}`, async () => {
      status = code;
      requests = [];
      const result = await verify(endpoint);
      assert.equal(result.stdout.trim(), String(code));
      assert.equal(result.statusCode, code >= 400 ? 22 : 0);
      assert.equal(result.statusCode === 0 && result.stdout.trim() === '200', code === 200);
      assert.deepEqual(requests, ['/api/config'], 'never follows the welcome redirect');
    });
  }
});

test('real curl passes shell-sensitive credentials unchanged', async (t) => {
  const username = "test'user";
  const password = "p'a\"ss $HOME; $(false):\\word";
  let authorization;
  const endpoint = await fixture(t, (request, response) => {
    authorization = request.headers.authorization;
    response.end('{"status":true}');
  });
  const result = await verify(endpoint, username, password);
  assert.equal(result.statusCode, 0, result.stderr);
  assert.equal(result.stdout.trim(), '200');
  assert.equal(authorization, `Basic ${Buffer.from(`${username}:${password}`).toString('base64')}`);
});

test('real curl transport failure cannot report auth success', async (t) => {
  const endpoint = await fixture(t, (request) => request.socket.destroy());
  const result = await verify(endpoint);
  assert.notEqual(result.statusCode, 0);
  assert.equal(result.stdout.trim(), '000');
});

test('repair validates credentials first, including best-effort repairs', () => {
  const repair = setupFunction('repair_sunshine_auth_state');
  assert.match(repair, /\) -> AppResult<\(\)> \{\s*ensure_sunshine_credentials\(context, sunshine_username, sunshine_password\)\.await\?;/);
  assert.ok(repair.indexOf('ensure_sunshine_credentials(') < repair.indexOf('bootstrap_sunshine_credentials_over_ssh('));
  assert.ok(repair.indexOf('ensure_sunshine_credentials(') < repair.indexOf('restart_sunshine_service_over_ssh('));
});

test('shared credential guard rejects empty or whitespace-only fields and preserves setup errors', () => {
  const guard = setupFunction('ensure_sunshine_credentials');
  assert.match(guard, /if username\.trim\(\)\.is_empty\(\) \|\| password\.trim\(\)\.is_empty\(\)/);
  assert.match(guard, /set_setup_failure\(/);
  assert.match(guard, /"missing_platform_credentials"/);
  assert.match(guard, /return Err\(AppError::InvalidInput\(/);
  assert.match(guard, /Ok\(\(\)\)\s*\}$/);
  assert.match(setupFunction('verify_sunshine_api'), /ensure_sunshine_credentials\(context, &username, &password\)\.await\?;/);
});
