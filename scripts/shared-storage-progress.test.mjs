import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/lib/sharedStorageProgress.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText;
const { canClearStorageCompletion, correlateStorageProgress, settleBackgroundRequest } = await import(`data:text/javascript;base64,${Buffer.from(js).toString('base64')}`);
const event = (operationId, extra = {}) => ({ operationId, instanceId: 17, kind: 'restore', ...extra });
const request = () => ({ key: 'instance.storage.sync', instanceId: 17, pendingStorageRequest: true });

test('a selected-app request progresses through multiple operations and rejects late earlier events', () => {
  let action = request();
  action = { ...action, ...correlateStorageProgress(action, event('first')), stage: 'APPLYING' };
  assert.equal(correlateStorageProgress(action, event('second')), null);
  action.stage = 'COMPLETED';
  const next = correlateStorageProgress(action, event('second'));
  assert.deepEqual(next.storageOperationIds, ['first', 'second']);
  action = { ...action, ...next, stage: 'APPLYING' };
  assert.equal(correlateStorageProgress(action, event('first')), null);
  action.stage = 'COMPLETED';
  assert.equal(correlateStorageProgress(action, event('first')), null);
  assert.equal(correlateStorageProgress(action, event('third')).operationId, 'third');
});

test('instance and operation kind are bound before the first progress event', () => {
  assert.equal(correlateStorageProgress(request(), event('other', { instanceId: 18 })), null);
  assert.equal(correlateStorageProgress(request(), event('other', { kind: 'backup' })), null);
  assert.equal(correlateStorageProgress({ key: 'instance.files.upload' }, event('other')), null);
});

test('paused, failed and settled requests cannot accidentally attach to a new operation', () => {
  const action = { ...request(), operationId: 'retained' };
  for (const stage of ['MONITORING_PAUSED', 'RECONNECTING', 'FAILED', 'CANCELLED']) {
    assert.equal(correlateStorageProgress({ ...action, stage }, event('unrelated')), null);
    assert.equal(correlateStorageProgress({ ...action, stage }, event('retained')).operationId, 'retained');
  }
  assert.equal(correlateStorageProgress({ ...action, stage: 'COMPLETED', pendingStorageRequest: false }, event('unrelated')), null);
});

test('per-app completion cannot clear a running batch and stale request cleanup cannot clear its successor', () => {
  const action = { ...request(), requestId: 'batch', operationId: 'first', stage: 'COMPLETED' };
  assert.equal(canClearStorageCompletion(action, 'first'), false);
  const next = { ...action, ...correlateStorageProgress(action, event('second')), stage: 'APPLYING' };
  assert.equal(canClearStorageCompletion(next, 'first'), false);
  assert.equal(settleBackgroundRequest(next, 'older-batch'), next);
  assert.equal(settleBackgroundRequest(next, 'batch'), null);
});

test('a paused request retains its exact handle until authoritative completion arrives', () => {
  const paused = { ...request(), requestId: 'batch', operationId: 'first', stage: 'MONITORING_PAUSED' };
  const retained = settleBackgroundRequest(paused, 'batch');
  assert.equal(retained.operationId, 'first');
  assert.equal(retained.pendingStorageRequest, false);
  assert.equal(canClearStorageCompletion(retained, 'unrelated'), false);
  assert.equal(canClearStorageCompletion(retained, 'first'), true);
});
