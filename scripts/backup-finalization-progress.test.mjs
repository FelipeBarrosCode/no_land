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

test('unavailable status overrides retained phases with honest localized uncertainty', () => {
  for (const state of ['RECONNECTING', 'MONITORING_PAUSED']) {
    for (const phase of ['reconnecting', 'waiting_for_status', 'reconciling', ...Object.keys(labels)]) {
      const value = event({ state, phase, transferredBytes: 1234, completedObjects: 2,
        message: 'Reconnecting: network disconnected, agent definitely busy',
        backupFinalization: { version: 1, stage: 'verifying_backup_history', stages: {} } });
      const before = JSON.stringify(value);
      const view = render(value);
      assert.equal(view.label, english[state === 'RECONNECTING'
        ? 'storage.waitingForStatus' : 'storage.monitoringPaused']);
      assert.equal(view.indeterminate, true);
      assert.match(view.detail, /remote work may still be running/i);
      assert.match(view.detail, /1234 B game data uploaded/);
      assert.match(view.detail, /2 packs confirmed/);
      assert.match(view.detail, /186664\/186664 files captured or reused/);
      assert.doesNotMatch(view.detail, /Reconnecting|network disconnected|definitely busy|Waiting:/);
      if (state === 'RECONNECTING') assert.match(view.detail, /last confirmed/);
      assert.equal(JSON.stringify(value), before, 'presentation must not alter confirmed counters');
    }
  }
  const first = render(event({ state: 'RECONNECTING', phase: 'waiting_for_status',
    backupPipeline: null, backupFinalization: undefined,
    completedObjects: undefined, transferredBytes: undefined }));
  assert.equal(first.label, english['storage.waitingForStatus']);
  assert.equal(first.indeterminate, true);
  assert.doesNotMatch(first.detail, /100%|completed|files captured|packs confirmed/i);
  const legacy = render(event({ state: 'RECONNECTING', phase: 'reconciling',
    backupPipeline: null, backupFinalization: undefined, completedUnits: 72192,
    totalUnits: 186735, unit: 'files', completedBytes: 1234, totalBytes: 5000 }));
  assert.match(legacy.detail, /72192\/186735 files/);
  assert.match(legacy.detail, /1234 B\/5000 B ready/);
  const failure = render(event({ state: 'FAILED', phase: 'FAILED',
    message: 'Authoritative manifest publication failure',
    backupFinalization: { version: 1, stage: 'publishing_manifest', stages: {} } }));
  assert.equal(failure.label, null);
  assert.match(failure.detail, /Authoritative manifest publication failure/);
  assert.doesNotMatch(failure.detail, /No new status received/);
});

test('all nine locale catalogs contain the same owned finalization keys and placeholders', () => {
  const keys = [...Object.values(labels), 'storage.backup.gameDataUploaded',
    'storage.backup.noNewGameDataPacks', 'storage.backup.finalization.metadataWork',
    'storage.waitingForStatus', 'storage.waitingForStatusDetail', 'storage.monitoringPausedDetail'];
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
  let resolve, reject;
  const transfer = new Promise((yes, no) => { resolve = yes; reject = no; });
  const listeners = {}, calls = [];
  const backend = {
    saveInstanceToSharedStorageSelected: () => transfer,
    cancelSharedStorageOperation: async (...args) => { calls.push(args); },
    exportDiagnosticReport: async () => null,
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
  return { store, listeners, calls, resolve, reject, action };
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

test('real appStore preserves backup identity, clock and counters through missing status and recovery', async () => {
  const f = fixture();
  await f.store.getState().bindEvents();
  await f.store.getState().saveInstanceStorageSelected(17, ['/apps/game']);
  const startedAt = f.action().startedAt;
  const requestId = f.action().requestId;
  f.listeners.subscribeSharedStorageProgress(event({ phase: 'verifying_backup_history' }));
  for (const state of ['RECONNECTING', 'RECONNECTING', 'MONITORING_PAUSED']) {
    f.listeners.subscribeSharedStorageProgress(event({ state, phase: 'verifying_backup_history',
      message: 'Reconnecting to your PC',
      backupFinalization: { version: 1, stage: 'verifying_backup_history', stages: {} } }));
    assert.equal(f.action().label, english[state === 'RECONNECTING'
      ? 'storage.waitingForStatus' : 'storage.monitoringPaused']);
    assert.equal(f.action().stage, state);
    assert.equal(f.action().mode, 'indeterminate');
    assert.equal(f.action().progress, null);
    assert.equal(f.action().operationId, 'backup-1');
    assert.equal(f.action().requestId, requestId);
    assert.equal(f.action().startedAt, startedAt);
    assert.match(f.action().detail, /remote work may still be running/i);
    assert.match(f.action().detail, /186664\/186664 files captured or reused/);
    assert.doesNotMatch(f.action().detail, /Reconnecting to your PC/);
    assert.equal(f.calls.length, 0, 'status uncertainty cannot cancel or resubmit');
  }
  f.listeners.subscribeSharedStorageProgress(event({ phase: 'releasing_repository' }));
  assert.equal(f.action().label, english[labels.releasing_repository]);
  assert.equal(f.action().startedAt, startedAt);
  assert.doesNotMatch(f.action().detail, /No new status received/);
  f.listeners.subscribeSharedStorageProgress(event({ state: 'FAILED', phase: 'FAILED',
    message: 'Authoritative source validation failure' }));
  assert.equal(f.action().stage, 'FAILED');
  assert.match(f.action().detail, /Authoritative source validation failure/);
  assert.doesNotMatch(f.action().detail, /No new status received/);
  assert.equal(f.calls.length, 0);
  f.reject({ message: 'Authoritative source validation failure' });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.action(), null, 'the authoritative request error can settle the activity');
  assert.equal(f.store.getState().error, 'Authoritative source validation failure');
});
