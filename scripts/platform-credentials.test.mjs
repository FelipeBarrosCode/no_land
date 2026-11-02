import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { execFileSync } from 'node:child_process';

const source = readFileSync(new URL('../src/lib/platformCredentials.ts', import.meta.url), 'utf8');
const module = { exports: {} };
vm.runInNewContext(ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS },
}).outputText, { exports: module.exports });
const { platformUsernameError: usernameError, platformPasswordError: passwordError } = module.exports;

test('both production bootstrap commands preserve safe punctuation through both shell layers', () => {
  const username = 'Player_1.test-name';
  const password = 'Ab9._~!@#%^*+=,:?/-';
  for (const file of ['sunshine.rs', 'post_wireguard_setup.rs']) {
    const rust = readFileSync(new URL(`../src-tauri/src/services/${file}`, import.meta.url), 'utf8');
    const template = rust.match(/r"(sudo -u [^\n]+sunshine --creds[^\n]+)"/);
    assert.ok(template, `${file} preserves shell escapes with a Rust raw string`);
    const command = template[1]
      .replace(/\{(?:target_user|sunshine_user)\}/g, 'noland')
      .replace(/\{(?:escaped_)?username\}/g, username)
      .replace(/\{(?:escaped_)?password\}/g, password);
    // Stub sudo/Sunshine, but run the actual production nested-shell command.
    // Suppress login profiles so the test never executes local startup files.
    const script = `sudo() { shift 2; shift; command bash --noprofile --norc "$@"; }; sunshine() { printf '%s\\n' "$@"; }; export -f sunshine; ${command}`;
    const output = execFileSync('bash', ['--noprofile', '--norc', '-c', script], { encoding: 'utf8' });
    assert.equal(output, `--creds\n${username}\n${password}\n`, file);
  }
});

test('accepts supported credentials and safe password punctuation', () => {
  assert.equal(usernameError('Player_1.test-name'), '');
  assert.equal(passwordError('Ab9._~!@#%^*+=,:?/-'), '');
});

test('enforces length, leading characters and exact values without trimming', () => {
  for (const value of ['', 'ab', '-user', '_user', ' user', 'user ', 'user\n']) {
    assert.ok(usernameError(value));
  }
  for (const value of ['', 'short', '-secret', ' secret', 'secret ']) {
    assert.ok(passwordError(value));
  }
});

test('checks every ASCII character against the command-safe allowlists', () => {
  const usernameAllowed = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-';
  const passwordAllowed = usernameAllowed + '~!@#%^*+=,:?/';
  for (let code = 0; code < 128; code++) {
    const char = String.fromCharCode(code);
    assert.equal(usernameError(`user${char}name`) === '', usernameAllowed.includes(char), `username byte ${code}`);
    assert.equal(passwordError(`secret${char}123`) === '', passwordAllowed.includes(char), `password byte ${code}`);
  }
});

test('rejects non-ASCII, invisible Unicode, shell substitutions and multiline payloads', () => {
  for (const value of ['usérname', 'user\u200bname', 'user\u00a0name', 'user😀', "user'name", '$(whoami)', '`whoami`', 'user\nname']) {
    assert.ok(usernameError(value));
    assert.ok(passwordError(value));
  }
});
