import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const module = { exports: {} };
runInNewContext(ts.transpileModule(read('src/lib/storageBrowse.ts'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText, { module, exports: module.exports });
const { startStorageBrowse } = module.exports;
const snapshot = (freshness = 'cached', scope = 'repository-a', entries = [{ path: '/catalog/game/current' }]) => ({
  scope, entries, freshness, observedAt: '2026-11-15T00:00:00Z',
});
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const flush = () => new Promise(resolve => setImmediate(resolve));

test('cached rows display before the fresh request completes', async () => {
  const fresh = deferred(), updates = [], calls = [];
  const cached = snapshot();
  const request = startStorageBrowse(refresh => {
    calls.push(refresh);
    return refresh ? fresh.promise : Promise.resolve(cached);
  }, view => updates.push(view));
  await flush();
  assert.deepEqual(calls, [false, true]);
  assert.equal(updates.at(-1).snapshot, cached);
  assert.equal(updates.at(-1).refreshing, true);
  fresh.resolve(snapshot('fresh'));
  await request.finished;
  assert.equal(updates.at(-1).snapshot.freshness, 'fresh');
  assert.equal(updates.at(-1).refreshing, false);
});

test('refresh errors retain cached rows without claiming they are current or empty', async () => {
  const cached = snapshot(), updates = [];
  await startStorageBrowse(refresh => refresh ? Promise.reject(new Error('offline')) : Promise.resolve(cached),
    view => updates.push(view)).finished;
  assert.equal(updates.at(-1).snapshot, cached);
  assert.equal(updates.at(-1).snapshot.freshness, 'cached');
  assert.equal(updates.at(-1).error, 'refresh_failed');
});

test('a repository/profile mismatch clears old rows instead of adopting another scope', async () => {
  const updates = [];
  await startStorageBrowse(async refresh => snapshot(refresh ? 'fresh' : 'cached', refresh ? 'repository-b' : 'repository-a'),
    view => updates.push(view)).finished;
  assert.equal(updates.at(-1).snapshot, null);
  assert.equal(updates.at(-1).error, 'scope_changed');
});

test('missing cache falls through to authoritative refresh and preserves verified empty results', async () => {
  const updates = [];
  await startStorageBrowse(async refresh => refresh ? snapshot('fresh', 'repository-a', []) : {
    ...snapshot('unavailable'), observedAt: null, entries: [],
  }, view => updates.push(view)).finished;
  assert.equal(updates.at(-1).snapshot.entries.length, 0);
  assert.equal(updates.at(-1).snapshot.freshness, 'fresh');
  assert.equal(updates.at(-1).error, null);
});

test('a broken disposable cache does not prevent a fresh fetch', async () => {
  const updates = [];
  await startStorageBrowse(refresh => refresh ? Promise.resolve(snapshot('fresh')) : Promise.reject(new Error('bad cache')),
    view => updates.push(view)).finished;
  assert.equal(updates.at(-1).snapshot.freshness, 'fresh');
  assert.equal(updates.at(-1).error, null);
});

test('a failed refresh cannot relabel a cached or unavailable response as fresh', async () => {
  for (const response of [snapshot(), { ...snapshot('unavailable'), observedAt: null }]) {
    const cached = snapshot(), updates = [];
    await startStorageBrowse(async refresh => refresh ? response : cached,
      view => updates.push(view)).finished;
    assert.equal(updates.at(-1).snapshot, cached);
    assert.equal(updates.at(-1).snapshot.freshness, 'cached');
    assert.equal(updates.at(-1).error, 'refresh_failed');
  }
});

test('a still-fresh native observation opens immediately without another provider request', async () => {
  const calls = [], updates = [], fresh = snapshot('fresh');
  await startStorageBrowse(async refresh => {
    calls.push(refresh);
    return fresh;
  }, view => updates.push(view)).finished;
  assert.deepEqual(calls, [false]);
  assert.equal(updates.at(-1).snapshot, fresh);
  assert.equal(updates.at(-1).refreshing, false);
});

test('explicit Refresh bypasses the native freshness window', async () => {
  const calls = [], updates = [];
  await startStorageBrowse(async refresh => {
    calls.push(refresh);
    return snapshot('fresh');
  }, view => updates.push(view), true).finished;
  assert.deepEqual(calls, [false, true]);
  assert.equal(updates.at(-1).refreshing, false);
});

test('an already-running refresh marked cached is joined rather than treated as fresh', async () => {
  const calls = [];
  await startStorageBrowse(async refresh => {
    calls.push(refresh);
    return snapshot(refresh ? 'fresh' : 'cached');
  }, () => {}).finished;
  assert.deepEqual(calls, [false, true]);
});

for (const beforeFresh of [true, false]) {
  test(`closing the view rejects late responses (${beforeFresh ? 'cache' : 'refresh'} pending)`, async () => {
    const cache = deferred(), fresh = deferred(), updates = [], calls = [];
    const request = startStorageBrowse(refresh => {
      calls.push(refresh);
      return refresh ? fresh.promise : cache.promise;
    }, view => updates.push(view));
    if (!beforeFresh) {
      cache.resolve(snapshot());
      await flush();
    }
    request.cancel();
    const count = updates.length;
    cache.resolve(snapshot());
    fresh.resolve(snapshot('fresh'));
    await request.finished;
    assert.equal(updates.length, count);
    if (beforeFresh) assert.deepEqual(calls, [false]);
  });
}

test('selectors retain cached trees, allow closing during refresh and gate mutations on fresh results', () => {
  for (const name of ['SharedStorageSyncModal', 'SharedStorageExportModal']) {
    const source = read(`src/features/shared-storage-manager/${name}.tsx`);
    assert.match(source, /useStorageBrowse\(open, instanceId/);
    assert.match(source, /<StorageBrowseStatus/);
    assert.match(source, /!browse\.snapshot && browse\.refreshing/);
    assert.match(source, /browse\.snapshot\?\.freshness !== "fresh"/);
    assert.match(source, /onClick=\{onClose\} disabled=\{busy \|\| loading\}/);
    assert.doesNotMatch(source, /600000/);
  }
});
