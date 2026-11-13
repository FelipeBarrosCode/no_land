import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const source = read('src/lib/storageDeletionProgress.ts');
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText;
const { deletionErrorMessage, deletionProgressFor, secondsSinceDeletionProgress } = await import(`data:text/javascript;base64,${Buffer.from(js).toString('base64')}`);
const preview = { instanceId: 42, planId: 'confirmed-plan', target: 'cloud' };
const event = extra => ({ operationId: 'confirmed-plan', instanceId: 42, kind: 'delete',
  state: 'RUNNING', deletion: { completed_packs: 37, total_packs: 115 }, ...extra });

test('deletion progress binds the frozen plan, PC and operation kind', () => {
  const sample = event();
  assert.equal(deletionProgressFor(preview, sample), sample);
  for (const change of [{ instanceId: 43 }, { operationId: 'earlier-plan' }, { kind: 'restore' }, { kind: 'backup' }, { deletion: null }]) {
    assert.equal(deletionProgressFor(preview, event(change)), null);
  }
  assert.equal(deletionProgressFor({ ...preview, target: 'remote_pc' }, sample), null);
});

test('reconnection retains confirmed counts without implying completion', () => {
  for (const state of ['RECONNECTING', 'MONITORING_PAUSED', 'FAILED']) {
    const result = deletionProgressFor(preview, event({ state }));
    assert.equal(result.deletion.completed_packs, 37);
    assert.equal(result.state, state);
  }
  const component = read('src/features/shared-storage-manager/StorageDeleteAction.tsx');
  assert.match(component, /await confirmStorageDeletion\(preview\.instanceId, preview\);\s*setCompleted\(true\)/u);
  assert.match(component, /deletionProgressFor\(preview, event\)/u);
  assert.match(component, /else unlisten\(\)/u, 'late subscription setup must not leak a listener');
});

test('last confirmed progress age is honest for unknown, stale and future timestamps', () => {
  const now = Date.parse('2026-11-13T21:00:00Z');
  assert.equal(secondsSinceDeletionProgress(null, now), null);
  assert.equal(secondsSinceDeletionProgress('invalid', now), null);
  assert.equal(secondsSinceDeletionProgress('2026-11-13T20:59:30Z', now), 30);
  assert.equal(secondsSinceDeletionProgress('2026-11-13T21:00:30Z', now), 0);
});

test('native timeout/error details remain actionable instead of rendering object Object', () => {
  assert.equal(deletionErrorMessage({ message: 'Operation failed', details: 'Resume only the same confirmed plan' }),
    'Resume only the same confirmed plan');
  assert.equal(deletionErrorMessage({ message: 'Permission denied', details: null }), 'Permission denied');
  assert.equal(deletionErrorMessage(new Error('Fixture error')), 'Fixture error');
  assert.equal(deletionErrorMessage('Fixture error'), 'Fixture error');
});

test('operation diagnostics are read-only and do not acquire the destructive action lock', () => {
  const command = read('src-tauri/src/commands/mod.rs').split('pub async fn get_storage_operation_status(')[1].split('#[tauri::command]')[0];
  assert.match(command, /"GetOperationStatus"/u);
  assert.match(command, /"projection"\s*:\s*"progress"/u);
  assert.match(command, /Uuid::parse_str/u);
  assert.doesNotMatch(command, /begin_action|ensure_state_agent|ensure_rclone|StartDeletion|RetryOperation|mint_ephemeral/u);
});

test('desktop MCP diagnostics stay within the read-only instance grant', {
  skip: !existsSync(new URL('../src-tauri/src/mcp/catalog.rs', import.meta.url)),
}, () => {
  const catalog = read('src-tauri/src/mcp/catalog.rs');
  assert.match(catalog, /tool\("storage_operation_status",[\s\S]*?,\s*Read,\s*true,\s*false,\s*true,/u);
  const adapter = read('src-tauri/src/mcp/adapter.rs');
  const guarded = adapter.split('let _action = if matches!(')[1].split('if e.cancelled.is_cancelled()')[0];
  assert.ok(!guarded.includes('storage_operation_status'));
});

test('verified deletion progress strings and placeholders exist in every locale', () => {
  const english = JSON.parse(read('src/locales/en-US.json'));
  const keys = Object.keys(english).filter(key => /^storage\.delete\.(progress|quotaNote|lastProgress|phase\.)/u.test(key));
  assert.equal(keys.length, 10);
  for (const name of readdirSync(new URL('../src/locales/', import.meta.url)).filter(name => name.endsWith('.json'))) {
    const locale = JSON.parse(read(`src/locales/${name}`));
    for (const key of keys) {
      assert.ok(locale[key], `${name}: ${key}`);
      assert.deepEqual([...locale[key].matchAll(/\{\w+\}/gu)].map(match => match[0]).sort(),
        [...english[key].matchAll(/\{\w+\}/gu)].map(match => match[0]).sort(), `${name}: ${key} placeholders`);
    }
  }
});
