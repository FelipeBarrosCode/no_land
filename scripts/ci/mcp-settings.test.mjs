import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../../src/lib/mcpSetup.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText;
const { buildMcpSetupPrompt } = await import(`data:text/javascript;base64,${Buffer.from(js).toString('base64')}`);
const token = `nlc_${'a'.repeat(64)}`;

test('one setup prompt includes a complete authenticated local connection', () => {
  const prompt = buildMcpSetupPrompt('http://127.0.0.1:47831/mcp', token);
  const config = JSON.parse(prompt.split('Connection details:\n')[1]);
  assert.equal(config.url, 'http://127.0.0.1:47831/mcp');
  assert.equal(config.oauth, false);
  assert.equal(config.headers.Authorization, `Bearer ${token}`);
  assert.match(prompt, /including PCs registered later/);
  assert.match(prompt, /Do not start a stream/);
  assert.match(prompt, /human confirmation/);
});

test('setup copy refuses non-loopback URLs and malformed tokens', () => {
  for (const url of ['https://example.com/mcp', 'http://0.0.0.0:47831/mcp',
    'http://127.0.0.1.evil/mcp', 'http://user:secret@127.0.0.1:47831/mcp',
    'http://127.0.0.1:47831/mcp?secret=oops']) {
    assert.throws(() => buildMcpSetupPrompt(url, token));
  }
  assert.throws(() => buildMcpSetupPrompt('http://127.0.0.1:47831/mcp', 'fingerprint'));
});

test('simple UI has one setup copy flow and no permission or token-management form', () => {
  const ui = readFileSync(new URL('../../src/features/settings/McpSettings.tsx', import.meta.url), 'utf8');
  assert.match(ui, /buildMcpSetupPrompt\(status.endpoint, token\)/);
  assert.match(ui, /mcp\.copyPort/);
  assert.match(ui, /mcpDisable\(\)/);
  assert.doesNotMatch(ui, /mcpRotateToken|mcpRevokeToken|fieldset|textarea|useAppStore/);
});
