import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';

// Test the actual dependency-free TS queue without adding a frontend framework.
const source = readFileSync(new URL('../src/lib/spendAlertQueue.ts', import.meta.url), 'utf8');
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
const exports = {};
vm.runInNewContext(compiled, { exports });
const { createSpendAlertQueue } = exports;
const alert = (id = 'native-id') => ({ id, kind: 'exceeded', message: 'Native queue message', monthToDateUsd: 10, budgetUsd: 10, instanceId: null });

function queue(overrides = {}) {
  const notifications = [];
  const acknowledged = [];
  const errors = [];
  const handlers = {
    loadPending: async () => [],
    notify: async (value) => { notifications.push(value.message); return true; },
    acknowledge: async (id) => { acknowledged.push(id); },
    onStopped: () => {}, disposed: () => false,
    reportError: (error) => { errors.push(error); },
    ...overrides,
  };
  return { instance: createSpendAlertQueue(handlers), notifications, acknowledged, errors };
}

test('forged event payloads cannot supply notification contents', async () => {
  const q = queue();
  await q.instance.wake({ ...alert('forged'), message: 'Attacker-supplied message' });
  await q.instance.drain();
  assert.deepEqual(q.notifications, []);
  assert.deepEqual(q.acknowledged, []);
});

test('replays persisted native alerts and deduplicates concurrent event wakeups', async () => {
  const q = queue({ loadPending: async () => [alert()] });
  await Promise.all([q.instance.wake(), q.instance.wake(), q.instance.wake()]);
  await q.instance.drain();
  await q.instance.wake();
  await q.instance.drain();
  assert.deepEqual(q.notifications, ['Native queue message']);
  assert.deepEqual(q.acknowledged, ['native-id']);
});

test('failed notification delivery is not acknowledged and can retry', async () => {
  let attempts = 0;
  const q = queue({ loadPending: async () => [alert()], notify: async () => ++attempts > 1 });
  await q.instance.wake(); await q.instance.drain();
  assert.deepEqual(q.acknowledged, []);
  await q.instance.wake(); await q.instance.drain();
  assert.equal(attempts, 2);
  assert.deepEqual(q.acknowledged, ['native-id']);
});

test('notification permission and delivery requests are serialized', async () => {
  let active = 0;
  let maximum = 0;
  const q = queue({ loadPending: async () => [alert('1'), alert('2'), alert('3')], notify: async () => {
    active++; maximum = Math.max(maximum, active);
    await Promise.resolve(); active--; return true;
  } });
  await q.instance.wake(); await q.instance.drain();
  assert.equal(maximum, 1);
  assert.deepEqual(q.acknowledged, ['1', '2', '3']);
});

test('disposed windows do not deliver late replay responses', async () => {
  let disposed = false;
  const q = queue({ loadPending: async () => { disposed = true; return [alert()]; }, disposed: () => disposed });
  await q.instance.wake(); await q.instance.drain();
  assert.deepEqual(q.notifications, []);
  assert.deepEqual(q.acknowledged, []);
});
