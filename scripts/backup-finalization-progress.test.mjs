import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const english = JSON.parse(read('src/locales/en-US.json'));
const translate = (key, values = {}) => (english[key] ?? key).replace(/\{(\w+)\}/g, (_, key) => String(values[key]));
const code = ts.transpileModule(read('src/lib/backupProgress.ts'), { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText;
const { backupProgressPresentation: present, BACKUP_FINALIZATION_LABELS: labels } =
  await import(`data:text/javascript;base64,${Buffer.from(code).toString('base64')}`);
const pipeline = {
  selected_files: 186664, source_total_bytes: 33.4 * 1024 ** 3, reused_files: 186664,
  reused_bytes: 33.4 * 1024 ** 3, capture_total_bytes: 0, durable_bytes: 0, completed_files: 0,
  prepared_bytes: 0, active_readers: 0, active_codecs: 0, inflight_uploads: 0,
  upload_limit: 4, waiting_on: 'manifest_commit', capture_complete: true,
};
const event = extra => ({
  operationId: 'backup-1', instanceId: 17, kind: 'backup', state: 'COMMITTING',
  phase: 'publishing_manifest', message: 'Remote text must not override the stage',
  fraction: 1, totalObjects: 0, completedObjects: 0, transferredBytes: 0,
  backupPipeline: pipeline, cancellable: true, cancelRequested: false, ...extra,
});
const render = value => present(value, translate, bytes => `${bytes} B`);

test('all seven finalization stages use owned localized labels and stay indeterminate', () => {
  assert.equal(Object.keys(labels).length, 7);
  for (const [stage, key] of Object.entries(labels)) {
    const view = render(event({ phase: stage }));
    assert.equal(view.label, english[key]);
    assert.equal(view.indeterminate, true);
    assert.match(view.detail, /0 B game data uploaded/);
    assert.match(view.detail, /Metadata may still be read or uploaded/);
    assert.match(view.detail, /No new game-data packs required/);
    assert.doesNotMatch(view.detail, /readers|packers|Waiting:|0\/4 uploads|Remote text/);
    const retained = render(event({ phase: 'reconnecting',
      backupFinalization: { version: 1, stage, started_at: '2026-11-15T22:55:00Z', stages: {} } }));
    assert.equal(retained.label, view.label);
  }
});

test('no-new-packs needs explicit EOF, a known zero pack total and zero prepared bytes', () => {
  for (const change of [
    { backupPipeline: { ...pipeline, capture_complete: undefined } },
    { backupPipeline: { ...pipeline, capture_complete: false } },
    { totalObjects: undefined }, { totalObjects: null }, { totalObjects: 1 },
    { backupPipeline: { ...pipeline, prepared_bytes: 1 } },
    { backupPipeline: { ...pipeline, prepared_bytes: undefined } },
  ]) {
    assert.doesNotMatch(render(event(change)).detail, /No new game-data packs required/);
  }
  assert.match(render(event({ backupPipeline: { ...pipeline, capture_complete: false } })).detail, /0 readers/);
  assert.match(render(event({ backupPipeline: { ...pipeline, active_readers: 2 } })).detail, /2 readers/);
  assert.doesNotMatch(render(event({ transferredBytes: undefined })).detail, /0 B game data uploaded/);
});

test('unknown/legacy finalization is safe, retains pack bytes and never renders remote text as a label', () => {
  for (const stage of ['unknown_phase', '__proto__', 'constructor', 'storage.delete.phase.deleting']) {
    const view = render(event({ phase: stage, backupPipeline: null, transferredBytes: 1234,
      backupFinalization: { version: 1, stage, stages: { [stage]: {
        elapsed_ms: 123300, outcome: 'completed',
        storage_io: { uploaded_bytes: 28470000 }, metadata_reads: { bytes: 169750000 },
      } } } }));
    assert.equal(view.label, english['storage.backup.inProgress']);
    assert.equal(view.indeterminate, true);
    assert.match(view.detail, /1234 B game data uploaded/);
    assert.doesNotMatch(view.detail, /Remote text|28470000|169750000/);
  }
  assert.equal(render(event({ phase: 'COMMITTING', backupFinalization: undefined,
    backupPipeline: null })).indeterminate, true);
  const capture = render(event({ state: 'CAPTURING_AND_UPLOADING', phase: 'capture_and_upload',
    backupPipeline: { ...pipeline, capture_complete: false, active_readers: 2, active_codecs: 3 } }));
  assert.equal(capture.label, null);
  assert.match(capture.detail, /2 readers · 3 packers/);
  assert.match(capture.detail, /186664\/186664 files captured or reused/);
  for (const kind of ['restore', 'delete']) assert.equal(render(event({ kind })), null);
  const warning = 'Operation completed; repository claim cleanup is required';
  const done = render(event({ state: 'COMPLETED', phase: 'COMPLETED', message: warning }));
  assert.equal(done.label, null);
  assert.ok(done.detail.includes(warning));
});

test('all nine locale catalogs contain the same owned finalization keys and placeholders', () => {
  const keys = [...Object.values(labels), 'storage.backup.gameDataUploaded',
    'storage.backup.noNewGameDataPacks', 'storage.backup.finalization.metadataWork'];
  const locales = readdirSync(new URL('../src/locales/', import.meta.url)).filter(name => name.endsWith('.json'));
  assert.equal(locales.length, 9);
  for (const name of locales) {
    const locale = JSON.parse(read(`src/locales/${name}`));
    for (const key of keys) {
      assert.ok(locale[key]?.trim(), `${name}: ${key}`);
      assert.deepEqual([...locale[key].matchAll(/\{(\w+)\}/g)].map(match => match[1]),
        [...english[key].matchAll(/\{(\w+)\}/g)].map(match => match[1]), `${name}: ${key}`);
    }
  }
});

function fixture() {
  let resolve;
  const transfer = new Promise(yes => { resolve = yes; });
  const listeners = {}, calls = [];
  const backend = {
    saveInstanceToSharedStorageSelected: () => transfer,
    cancelSharedStorageOperation: async (...args) => { calls.push(args); },
  };
  for (const name of ['subscribeProvisioningEvents', 'subscribeSharedStorageProgress',
    'subscribeSharedStorageRestoreCompleted', 'subscribeDirectUploadProgress']) {
    backend[name] = async callback => { listeners[name] = callback; return () => {}; };
  }
  const evaluate = path => {
    const module = { exports: {} };
    const code = ts.transpileModule(read(path), { compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020,
    } }).outputText;
    runInNewContext(code, {
      module, exports: module.exports, crypto: { randomUUID }, console,
      require(name) {
        if (name === 'zustand') return { create(factory) {
          let state;
          const get = () => state;
          const set = update => { state = { ...state, ...(typeof update === 'function' ? update(state) : update) }; };
          state = factory(set, get);
          return { getState: get, setState: set };
        } };
        if (name.endsWith('/backend')) return backend;
        if (name.endsWith('/backupProgress')) return evaluate('src/lib/backupProgress.ts');
        if (name.endsWith('/sharedStorageProgress')) return evaluate('src/lib/sharedStorageProgress.ts');
        if (name.endsWith('/storageDeletionProgress')) return evaluate('src/lib/storageDeletionProgress.ts');
        if (name.endsWith('/i18n')) return { translate, translateSource: value => value };
        if (name.endsWith('/notificationPreferences')) return { isNotificationEnabled: () => false };
        return {};
      },
    }, { filename: path });
    return module.exports;
  };
  const store = evaluate('src/store/appStore.ts').useAppStore;
  const action = () => store.getState()[Object.hasOwn(store.getState(), 'backgroundStorageAction')
    ? 'backgroundStorageAction' : 'blockingAction'];
  return { store, listeners, calls, resolve, action };
}

test('real appStore applies stages without early completion or changing operation/cancel correlation', async () => {
  const f = fixture();
  await f.store.getState().bindEvents();
  await f.store.getState().saveInstanceStorageSelected(17, ['/apps/game']);
  const startedAt = f.action().startedAt;
  for (const [phase, key] of Object.entries(labels)) {
    f.listeners.subscribeSharedStorageProgress(event({ phase, readyToLaunch: true }));
    assert.equal(f.action().label, english[key]);
    assert.equal(f.action().mode, 'indeterminate');
    assert.equal(f.action().progress, null);
    assert.equal(f.action().operationId, 'backup-1');
    assert.equal(f.action().startedAt, startedAt);
    assert.doesNotMatch(f.action().detail, /readers|packers|Remote text/);
    const previous = f.action();
    f.listeners.subscribeSharedStorageProgress(event({ operationId: 'unrelated' }));
    f.listeners.subscribeSharedStorageProgress(event({ instanceId: 18 }));
    assert.equal(f.action(), previous);
  }
  await f.store.getState().cancelSharedStorageOperation(17);
  assert.deepEqual(f.calls, [[17, 'backup-1']]);
  assert.equal(f.action().cancelRequested, true);
  const warning = 'Operation completed; repository claim cleanup is required';
  f.listeners.subscribeSharedStorageProgress(event({ state: 'COMPLETED', phase: 'COMPLETED',
    message: warning, cancellable: false }));
  assert.ok(f.action().detail.includes(warning));
  assert.ok(f.action(), 'only the authoritative request result settles the active batch');
  f.resolve('confirmed completion after repository release');
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.action(), null);
});
