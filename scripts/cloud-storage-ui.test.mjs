import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const english = JSON.parse(read('src/locales/en-US.json'));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const flush = () => new Promise(resolve => setImmediate(resolve));

// Exercise the real store with in-memory backends. No native/remote calls run.
function fixture() {
  const transfer = deferred(), stream = deferred();
  const listeners = {}, calls = [];
  const backend = {
    startPlayExistingInstance: () => stream.promise,
    getAppState: async () => ({ orchestrationState: 'Ready' }),
    getInstanceMoonlightPipelineStatus: async () => null,
    syncInstanceFromSharedStorageSelected: () => transfer.promise,
    saveInstanceToSharedStorageSelected: () => transfer.promise,
    uploadPathsToInstance: () => transfer.promise,
    cancelSharedStorageOperation: async (...args) => { calls.push(['cancel', ...args]); },
    confirmStorageDeletion: async (...args) => { calls.push(['delete', ...args]); return transfer.promise; },
    exportDiagnosticReport: async () => null,
  };
  for (const name of ['subscribeProvisioningEvents', 'subscribeSharedStorageProgress',
    'subscribeSharedStorageRestoreCompleted', 'subscribeDirectUploadProgress']) {
    backend[name] = async callback => { listeners[name] = callback; return () => {}; };
  }
  const evaluate = path => {
    const module = { exports: {} };
    const code = ts.transpileModule(read(path), { compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
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
        if (name.endsWith('/sharedStorageProgress')) return evaluate('src/lib/sharedStorageProgress.ts');
        if (name.endsWith('/storageDeletionProgress')) return evaluate('src/lib/storageDeletionProgress.ts');
        if (name.endsWith('/i18n')) return {
          translate: (key, values = {}) => (english[key] ?? key).replace(/\{(\w+)\}/g, (_, k) => String(values[k] ?? `{${k}}`)),
          translateSource: value => value,
        };
        if (name.endsWith('/constants')) return { PROVISIONING_ORDER: ['Idle', 'Ready'] };
        if (name.endsWith('/notificationPreferences')) return { isNotificationEnabled: () => false };
        return {};
      },
    }, { filename: path });
    return module.exports;
  };
  const store = evaluate('src/store/appStore.ts').useAppStore;
  return { store, transfer, stream, listeners, calls };
}

const progress = (kind, extra = {}) => ({
  instanceId: 17, operationId: 'transfer-1', kind, state: 'RUNNING',
  phase: 'DOWNLOADING_PACKS', fraction: 0.25, cancellable: true, ...extra,
});

for (const kind of ['download', 'upload', 'direct']) {
  test(`${kind}: stream startup and completion cannot replace background storage progress`, async () => {
    const f = fixture();
    await f.store.getState().bindEvents();
    const launch = kind === 'download' ? () => f.store.getState().syncInstanceStorage(17, ['/catalog/app/bundle'])
      : kind === 'upload' ? () => f.store.getState().saveInstanceStorageSelected(17, ['/apps/game'])
        : () => f.store.getState().uploadPathsToRemoteInstance(17, ['/fixture'], '/tmp/fixture');
    await launch();
    const request = f.store.getState().backgroundStorageAction.requestId;
    const starting = f.store.getState().startPlayExisting(17);
    assert.equal(f.store.getState().isBlocking, true);
    assert.equal(f.store.getState().blockingAction.key, 'provisioning.flow');
    assert.equal(f.store.getState().backgroundStorageAction.requestId, request);
    if (kind === 'direct') {
      f.listeners.subscribeDirectUploadProgress({ instanceId: 17, operationId: 'transfer-1',
        completedObjects: 1, totalObjects: 4, completedBytes: 10, totalBytes: 40, fraction: 0.25 });
    } else {
      f.listeners.subscribeSharedStorageProgress(progress(kind === 'upload' ? 'backup' : 'restore'));
      f.listeners.subscribeSharedStorageProgress(progress(kind === 'upload' ? 'backup' : 'restore', { instanceId: 18, fraction: 0.99 }));
    }
    assert.equal(f.store.getState().backgroundStorageAction.progress, 25);
    f.stream.resolve('embedded');
    await starting;
    assert.equal(f.store.getState().blockingAction, null);
    assert.equal(f.store.getState().backgroundStorageAction.requestId, request);
    if (kind === 'direct') f.transfer.resolve({ uploadedObjects: 1, destination: '/tmp/fixture' });
    else f.transfer.resolve('completed');
    await flush();
    assert.equal(f.store.getState().backgroundStorageAction, null);
  });
}

test('background completion leaves a concurrent foreground stream startup untouched', async () => {
  const f = fixture();
  await f.store.getState().syncInstanceStorage(17, ['/catalog/app/bundle']);
  const starting = f.store.getState().startPlayExisting(17);
  f.transfer.resolve('completed');
  await flush();
  assert.equal(f.store.getState().backgroundStorageAction, null);
  assert.equal(f.store.getState().blockingAction.key, 'provisioning.flow');
  assert.equal(f.store.getState().busy, true);
  f.stream.resolve('embedded');
  await starting;
});

test('cancellation retains exact operation identity while the foreground stream starts', async () => {
  const f = fixture();
  await f.store.getState().bindEvents();
  await f.store.getState().syncInstanceStorage(17, ['/catalog/app/bundle']);
  f.listeners.subscribeSharedStorageProgress(progress('restore'));
  const starting = f.store.getState().startPlayExisting(17);
  await f.store.getState().cancelSharedStorageOperation(17);
  assert.deepEqual(f.calls[0], ['cancel', 17, 'transfer-1']);
  assert.equal(f.store.getState().backgroundStorageAction.cancelRequested, true);
  assert.equal(f.store.getState().blockingAction.key, 'provisioning.flow');
  f.transfer.resolve('completed'); f.stream.resolve('embedded');
  await starting; await flush();
});

test('confirmed deletion runs in the background and binds progress to its frozen plan', async () => {
  const f = fixture();
  await f.store.getState().bindEvents();
  const preview = { planId: 'confirmed-plan', target: 'cloud', fullDeletion: true, names: ['fixture'] };
  await f.store.getState().startStorageDeletion(17, preview);
  assert.equal(f.store.getState().busy, false);
  assert.equal(f.store.getState().isBlocking, false);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0][1], 17);
  assert.equal(f.calls[0][2], preview);
  const event = progress('delete', { operationId: preview.planId,
    deletion: { phase: 'deleting', completed_objects: 2, total_objects: 4,
      completed_packs: 1, total_packs: 2, verified_absent_bytes: 1024 } });
  for (const change of [{ operationId: 'other-plan' }, { instanceId: 18 }, { kind: 'restore' }]) {
    f.listeners.subscribeSharedStorageProgress({ ...event, ...change });
    assert.equal(f.store.getState().backgroundStorageAction.progress, null);
  }
  f.listeners.subscribeSharedStorageProgress(event);
  assert.equal(f.store.getState().backgroundStorageAction.progress, 50);
  assert.equal(f.store.getState().backgroundStorageAction.cancellable, false);
  const starting = f.store.getState().startPlayExisting(17);
  assert.equal(f.store.getState().backgroundStorageAction.operationId, preview.planId);
  f.transfer.resolve('completed'); f.stream.resolve('embedded');
  await starting; await flush();
  assert.equal(f.store.getState().backgroundStorageAction, null);
  assert.equal(f.calls.length, 1, 'no new preview, deletion plan or automatic retry');
});

test('uncertain deletion failures retain their handle without retrying or claiming success', async () => {
  const f = fixture();
  await f.store.getState().startStorageDeletion(17, { planId: 'confirmed-plan', target: 'cloud' });
  f.transfer.reject({ message: 'Response lost: cloud outcome is unknown' });
  await flush();
  const failed = f.store.getState().backgroundStorageAction;
  assert.equal(failed.stage, 'FAILED');
  assert.equal(failed.operationId, 'confirmed-plan');
  assert.equal(f.calls.length, 1);
  f.store.getState().clearBackgroundStorageAction('unrelated');
  assert.equal(f.store.getState().backgroundStorageAction, failed);
  f.store.getState().clearBackgroundStorageAction('confirmed-plan');
  assert.equal(f.store.getState().backgroundStorageAction, null);
});

test('paused deletion monitoring retains confirmed counts, age and exact plan without retry', async () => {
  const f = fixture();
  await f.store.getState().bindEvents();
  await f.store.getState().startStorageDeletion(17, { planId: 'confirmed-plan', target: 'cloud' });
  f.listeners.subscribeSharedStorageProgress(progress('delete', {
    operationId: 'confirmed-plan', state: 'MONITORING_PAUSED',
    deletion: { phase: 'deleting', completed_objects: 2, total_objects: 4,
      completed_packs: 1, total_packs: 2, verified_absent_bytes: 1024,
      last_success_at: '2026-11-15T00:00:00Z' },
  }));
  f.transfer.reject({ message: 'Monitoring paused' });
  await flush();
  const action = f.store.getState().backgroundStorageAction;
  assert.equal(action.stage, 'MONITORING_PAUSED');
  assert.equal(action.label, english['storage.monitoringPaused']);
  assert.equal(action.progress, 50);
  assert.equal(action.lastProgressAt, '2026-11-15T00:00:00Z');
  assert.equal(action.pendingStorageRequest, false);
  assert.equal(f.calls.length, 1);
  assert.equal(f.store.getState().error, null);
});

test('cancellation cannot target another PC, an unknown operation or a deletion', async () => {
  const f = fixture();
  await f.store.getState().syncInstanceStorage(17, ['/catalog/app/bundle']);
  await f.store.getState().cancelSharedStorageOperation(18);
  await f.store.getState().cancelSharedStorageOperation(17);
  assert.equal(f.calls.length, 0);
  f.transfer.resolve('completed'); await flush();
});

test('storage status is outside route/foreground guards and deletion closes only after background acceptance', () => {
  assert.match(read('src/app/App.tsx'), /<CloudStorageActivity \/>/);
  const activity = read('src/features/shared-storage-manager/CloudStorageActivity.tsx');
  assert.match(activity, /state\.backgroundStorageAction/);
  assert.doesNotMatch(activity, /isBlocking|embeddedMoonlightStatus|videoSessionActive/);
  const deletion = read('src/features/shared-storage-manager/StorageDeleteAction.tsx');
  assert.match(deletion, /<ModalFrame nonBlocking/);
  assert.match(deletion, /await startDeletion\(preview\.instanceId, preview\);[\s\S]*?onStarted\(\)/);
  assert.match(deletion, /acknowledgedDependents/);
  assert.match(deletion, /preview\.fullDeletion !== true/);
  assert.match(read('src/components/ui/ModalFrame.tsx'), /if \(nonBlocking\) return;/);
});

test('server configuration owns EDID, preserves reliability policy, and removes first-open cards', () => {
  const settings = read('src/features/settings/SettingsScreen.tsx');
  const server = settings.split('const serverPanel =')[1].split('const clientPanel =')[0];
  const client = settings.split('const clientPanel =')[1];
  assert.match(server, /onRegenerateEdid/);
  assert.doesNotMatch(client, /onRegenerateEdid|generated\.9a12d65bf2f96b18/);
  assert.doesNotMatch(settings, /feedback\.vast\.minimum|serverForm\.minReliability/);
  assert.match(server, /minReliability: appState\.serverPreferences\.minReliability/);
  assert.doesNotMatch(read('src/features/dashboard/DashboardScreen.tsx'), /showDashboardGuidance|APP_PROMPTS\.(moonlightCard|wireguardCard|setServerCard)/);
});

test('game actions and branding are localized without renaming settings Save or the MCP protocol', () => {
  assert.equal(english['storage.games.upload'], 'Upload games');
  assert.equal(english['storage.games.download'], 'Download games');
  assert.equal(english['settings.storage'], 'Cloud Storage');
  assert.equal(english['mcp.label'], 'AI automation');
  assert.equal(english['generated.1509f561f2416598'], 'Save');
  for (const name of readdirSync(new URL('../src/locales/', import.meta.url)).filter(n => n.endsWith('.json'))) {
    const locale = JSON.parse(read(`src/locales/${name}`));
    for (const key of ['storage.games.upload', 'storage.games.download', 'storage.operation.running',
      'storage.delete.preparing', 'storage.delete.failed']) assert.ok(locale[key], `${name}: ${key}`);
    assert.notEqual(locale['mcp.label'], 'MCP');
  }
  assert.match(read('src/lib/mcpSetup.ts'), /MCP server named noland/);
});

const generatedKey = text => `generated.${createHash('sha256').update(text).digest('hex').slice(0, 16)}`;

test('generated UI IDs bind the current English copy, including every renamed Cloud Storage label', () => {
  for (const [key, text] of Object.entries(english)) {
    if (!key.startsWith('generated.')) continue;
    assert.equal(key, generatedKey(text), `Generated ID must match its English UI text: ${key}`);
  }
  for (const text of [
    'Set up cloud storage',
    'Cloud storage',
    '· Installed software and titles tracked in Cloud Storage',
    'Cloud Storage',
    'How to use cloud storage',
    'Uploading games...',
    'Downloading games...',
    'Upload games to Cloud Storage',
    'Download games from Cloud Storage',
    'No files found in cloud storage.',
    'Cloud Storage Overview',
    'Loading cloud storage settings',
    'Saving cloud storage settings',
    'Testing cloud storage connection',
    'Use the cloud storage upload flow to choose specific applications instead of uploading the whole filesystem.',
    'Whole-instance cloud-storage upload has been removed. Use Upload games from the dashboard/cloud storage UI.',
    'Loading cloud storage tree',
    'Reading the application catalog from Cloud Storage.',
    'Copying your selected games from cloud storage to the instance.',
    'Unable to retrieve files from cloud storage.',
    'Noland Cloud Storage',
    'No cloud storage operation is running.',
  ]) {
    assert.equal(english[generatedKey(text)], text);
  }
});

test('locale catalogs have unique keys and preserve the merged Cloud Storage translation in every language', () => {
  const translations = {
    'de.json': 'Cloud-Speicher',
    'en-US.json': 'Cloud Storage',
    'es.json': 'Almacenamiento en la nube',
    'fr.json': 'Stockage cloud',
    'it.json': 'Archiviazione cloud',
    'ja.json': 'クラウドストレージ',
    'ko.json': '클라우드 저장소',
    'pt-BR.json': 'Armazenamento na nuvem',
    'zh-CN.json': '云存储',
  };
  const names = readdirSync(new URL('../src/locales/', import.meta.url)).filter(name => name.endsWith('.json')).sort();
  assert.deepEqual(names, Object.keys(translations).sort());
  const expectedKeys = Object.keys(english).sort();
  const cloudKey = generatedKey('Cloud Storage');
  for (const name of names) {
    const source = read(`src/locales/${name}`);
    const locale = JSON.parse(source);
    // Inspect raw properties as well: JSON.parse alone silently overwrites duplicates.
    const object = ts.parseJsonText(name, source).statements[0]?.expression;
    assert.ok(object && ts.isObjectLiteralExpression(object), `${name}: locale object`);
    const keys = object.properties.map(property => property.name.text);
    assert.equal(new Set(keys).size, keys.length, `${name}: duplicate locale keys`);
    assert.deepEqual(keys.sort(), expectedKeys, `${name}: canonical locale key set`);
    assert.equal(keys.filter(key => key === cloudKey).length, 1, `${name}: merged Cloud Storage key`);
    assert.equal(locale[cloudKey], translations[name], `${name}: preserve the existing translation`);
  }
});

test('generated translation references in UI source and tests cannot retain removed IDs', () => {
  const sourceFiles = directory => readdirSync(new URL(`../${directory}/`, import.meta.url), { withFileTypes: true })
    .flatMap(entry => entry.isDirectory() ? sourceFiles(`${directory}/${entry.name}`) : [`${directory}/${entry.name}`]);
  let references = 0;
  for (const path of [...sourceFiles('src'), ...sourceFiles('scripts')].filter(path => /\.[cm]?[jt]sx?$/.test(path))) {
    const pattern = path.startsWith('src/')
      ? /["'](generated\.[0-9a-f]{16})["']/g
      : /(?:\btranslate\s*\(\s*|\benglish\s*\[\s*)["'](generated\.[0-9a-f]{16})["']/g;
    for (const [, key] of read(path).matchAll(pattern)) {
      references++;
      assert.ok(Object.hasOwn(english, key), `${path}: removed or unknown generated UI ID ${key}`);
    }
  }
  assert.ok(references > 0, 'The regression must inspect real generated UI references');
});
