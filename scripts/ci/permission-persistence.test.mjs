import assert from 'node:assert/strict';
import { readFileSync, existsSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, sep } from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';

// The same no-app-build suite can validate the Android/iOS checkouts without
// installing dependencies or copying their native code into this repository.
const root = process.env.NOLAND_PERMISSION_TEST_ROOT
  ? pathToFileURL(resolve(process.env.NOLAND_PERMISSION_TEST_ROOT) + sep)
  : new URL('../../', import.meta.url);
const read = (path) => readFileSync(new URL(path, root), 'utf8');
const requestedKey = 'noland.notificationPermissionRequested.v1';
const preferencesKey = 'noland.notificationPreferences';
function storage(initial = {}) {
  const values = new Map(Object.entries(initial));
  return {
    values, failReads: false, failWrites: false,
    getItem(key) {
      if (this.failReads) throw new Error('Storage unavailable');
      return values.get(key) ?? null;
    },
    setItem(key, value) {
      if (this.failWrites) throw new Error('Storage unavailable');
      values.set(key, value);
    },
  };
}
function load(path, window, dependencies = {}) {
  const exports = {};
  const js = ts.transpileModule(read(path), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  vm.runInNewContext(js, { exports, window, require: (id) => {
    assert.ok(['@tauri-apps/plugin-notification', '@tauri-apps/api/core'].includes(id), id);
    return dependencies;
  } });
  return exports;
}
function gate(options = {}) {
  const localStorage = options.storage ?? storage();
  const Notification = { permission: options.permission ?? 'default' };
  const stats = { requests: 0, checks: 0, nativeState: options.state ?? null };
  const window = { localStorage, Notification };
  const dependencies = {
    invoke: async (command) => {
      assert.equal(command, 'plugin:notification|is_permission_granted');
      stats.checks++;
      return stats.nativeState;
    },
    requestPermission: async () => {
      stats.requests++;
      return options.request ? options.request() : 'denied';
    },
  };
  return {
    ...load('src/lib/notificationPermission.ts', window, dependencies),
    localStorage, Notification, stats,
  };
}

test('existing OS grants never open a prompt, and stored bits cannot grant access', async () => {
  const granted = gate({ state: true, permission: 'denied' });
  assert.equal(await granted.ensureNotificationPermission(), true);
  assert.equal(await granted.ensureNotificationPermission(), true);
  assert.equal(granted.stats.requests, 0);
  assert.equal(granted.stats.checks, 2);
  const notGranted = gate({ storage: storage({ [requestedKey]: 'true' }) });
  assert.equal(await notGranted.ensureNotificationPermission(), false);
  assert.equal(notGranted.stats.requests, 0);
});

test('simultaneous notification sources share a single permission request', async () => {
  let finish;
  let announce;
  const started = new Promise((resolve) => { announce = resolve; });
  const current = gate({ request: () => {
    announce();
    return new Promise((resolve) => { finish = resolve; });
  } });
  const promises = Array.from({ length: 12 }, () => current.ensureNotificationPermission());
  assert.ok(promises.every((promise) => promise === promises[0]));
  await started;
  finish('default');
  assert.ok((await Promise.all(promises)).every((value) => value === false));
  assert.equal(current.stats.requests, 1);
  assert.equal(await current.ensureNotificationPermission(), false);
  assert.equal(current.stats.requests, 1);
});

test('denial and dismissal survive module reload without asking on each event', async () => {
  for (const result of ['denied', 'default']) {
    const saved = storage();
    const first = gate({ storage: saved, request: async () => result });
    assert.equal(await first.ensureNotificationPermission(), false);
    assert.equal(first.stats.requests, 1);
    const restarted = gate({ storage: saved });
    assert.equal(await restarted.ensureNotificationPermission(), false);
    assert.equal(restarted.stats.requests, 0);
  }
});

test('OS revocation takes effect, and an external settings grant is noticed', async () => {
  const current = gate({ permission: 'granted', request: async () => 'granted' });
  assert.equal(await current.ensureNotificationPermission(), true);
  current.stats.nativeState = false;
  assert.equal(await current.ensureNotificationPermission(), false);
  assert.equal(current.stats.requests, 1, 'a cached grant never overrides OS revocation');
  current.stats.nativeState = true;
  assert.equal(await current.ensureNotificationPermission(), true);
  assert.equal(current.stats.requests, 1);
});

test('only an explicit settings action retries a dismissed request, never an OS denial', async () => {
  const current = gate({ storage: storage({ [requestedKey]: 'true' }), request: async () => 'granted' });
  assert.equal(await current.ensureNotificationPermission(), false);
  assert.equal(await current.ensureNotificationPermission(true), true);
  assert.equal(current.stats.requests, 1);
  current.stats.nativeState = false;
  assert.equal(await current.ensureNotificationPermission(true), false);
  assert.equal(current.stats.requests, 1);
});

test('native denial never prompts, even with a stale granted webview cache', async () => {
  const current = gate({ state: false, permission: 'granted' });
  assert.equal(await current.ensureNotificationPermission(), false);
  assert.equal(await current.ensureNotificationPermission(true), false);
  assert.equal(current.stats.requests, 0);
});

test('storage failure and request errors do not create an automatic prompt loop', async () => {
  const unavailable = storage();
  unavailable.failReads = unavailable.failWrites = true;
  const current = gate({ storage: unavailable });
  assert.equal(await current.ensureNotificationPermission(), false);
  assert.equal(await current.ensureNotificationPermission(), false);
  assert.equal(current.stats.requests, 1);
  const failed = gate({ request: async () => { throw new Error('Request interrupted'); } });
  await assert.rejects(failed.ensureNotificationPermission(), /Request interrupted/);
  assert.equal(await failed.ensureNotificationPermission(), false);
  assert.equal(failed.stats.requests, 1);
  const restarted = gate({ storage: failed.localStorage });
  assert.equal(await restarted.ensureNotificationPermission(), false);
  assert.equal(restarted.stats.requests, 0);
});

test('notification denials are persisted exactly and returned objects cannot change them', () => {
  const saved = storage();
  const first = load('src/lib/notificationPreferences.ts', { localStorage: saved });
  for (const kind of Object.keys(first.getNotificationPreferences())) {
    first.setNotificationPreference(kind, false);
  }
  const restarted = load('src/lib/notificationPreferences.ts', { localStorage: saved });
  const preferences = restarted.getNotificationPreferences();
  assert.ok(Object.values(preferences).every((value) => value === false));
  preferences.network = true;
  assert.equal(restarted.isNotificationEnabled('network'), false);
});

test('failed preference saves cannot re-enable disabled categories from stale storage', () => {
  const saved = storage({ [preferencesKey]: '{"network":true,"storage":true}' });
  const current = load('src/lib/notificationPreferences.ts', { localStorage: saved });
  saved.failWrites = true;
  current.setNotificationPreference('network', false);
  assert.equal(current.isNotificationEnabled('network'), false);
  saved.failReads = true;
  assert.equal(current.isNotificationEnabled('network'), false);
  saved.failReads = saved.failWrites = false;
  current.setNotificationPreference('storage', false);
  const restarted = load('src/lib/notificationPreferences.ts', { localStorage: saved });
  assert.equal(restarted.isNotificationEnabled('network'), false);
  assert.equal(restarted.isNotificationEnabled('storage'), false);
});

test('setter results cannot mutate a prior denial while storage is unavailable', () => {
  const saved = storage({ [preferencesKey]: '{"network":true,"budget":false,"storage":false}' });
  const current = load('src/lib/notificationPreferences.ts', { localStorage: saved });
  const deniedKind = 'budget' in current.getNotificationPreferences() ? 'budget' : 'storage';
  assert.equal(current.isNotificationEnabled(deniedKind), false);
  saved.failReads = saved.failWrites = true;
  const returned = current.setNotificationPreference('network', false);
  returned[deniedKind] = true;
  assert.equal(current.isNotificationEnabled(deniedKind), false);
  assert.equal(current.isNotificationEnabled('network'), false);
});

test('malformed stored preferences cannot supply truthy non-boolean choices', () => {
  const saved = storage({ [preferencesKey]: '{"network":false}' });
  const current = load('src/lib/notificationPreferences.ts', { localStorage: saved });
  assert.equal(current.isNotificationEnabled('network'), false);
  saved.values.set(preferencesKey, '{"network":"true","storage":42}');
  assert.equal(current.isNotificationEnabled('network'), false);
  assert.equal(typeof current.isNotificationEnabled('storage'), 'boolean');
});

test('all notification paths use the shared gate, with retry reserved for settings', () => {
  const paths = ['src/lib/networkNotifications.ts', 'src/lib/provisioningNotifications.ts',
    'src/lib/instanceNotifications.ts', 'src/store/appStore.ts'];
  if (existsSync(new URL('src/lib/spendNotifications.ts', root))) paths.push('src/lib/spendNotifications.ts');
  for (const path of paths) {
    assert.match(read(path), /await ensureNotificationPermission\(\)/, path);
    assert.doesNotMatch(read(path), /\brequestPermission\b|\bisPermissionGranted\b/, path);
  }
  assert.match(read('src/features/settings/NotificationSettings.tsx'), /if \(enabled\)[\s\S]*ensureNotificationPermission\(true\)/);
});

test('MCP startup uses the exact persisted grant even while maintenance owns the lock', {
  skip: !existsSync(new URL('src-tauri/src/mcp/mod.rs', root)),
}, () => {
  const initialize = read('src-tauri/src/mcp/mod.rs').split('pub fn initialize(')[1].split('impl McpManager')[0];
  assert.match(initialize, /config: Config/);
  assert.doesNotMatch(initialize, /try_read|unwrap_or_default|for_local_agents/);
  const startup = read('src-tauri/src/main.rs');
  assert.match(startup, /let mcp_config = initial_state\.mcp\.clone\(\);/);
  assert.match(startup, /mcp::initialize\(app\.handle\(\), context\.clone\(\), mcp_config\);/);
});

function method(source, name) {
  const start = source.indexOf(`    pub async fn ${name}`);
  assert.ok(start >= 0, name);
  const body = source.indexOf('\n    {', start) >= 0 && name === 'update_state'
    ? source.indexOf('\n    {', start) + 5 : source.indexOf('{', start);
  let depth = 1;
  let end = body + 1;
  for (; depth > 0; end++) {
    if (source[end] === '{') depth++;
    else if (source[end] === '}') depth--;
  }
  return source.slice(start, end);
}

test('the actual Rust reload excludes concurrent saves and preserves state after load errors', () => {
  const source = read('src-tauri/src/services/app_context.rs');
  const methods = ['update_state', 'load_state', 'reload_state_from_disk'].map((name) => method(source, name)).join('\n');
  // Compile only these production methods against inert dependencies. This does
  // not build or launch the native app, access OS permissions, or read app data.
  const harness = `
use std::{future::Future, pin::pin, sync::{Arc, Barrier, Mutex, mpsc}, task::{Context, Poll, Wake, Waker}};
type AppResult<T> = Result<T, &'static str>;
#[derive(Clone, Default)]
struct PersistedAppState { granted: bool, feedback_program: () }
impl PersistedAppState { fn normalize_feedback_identifiers(&mut self) {} }
struct Lock<T>(Mutex<T>);
impl<T> Lock<T> {
    async fn write(&self) -> std::sync::MutexGuard<'_, T> { self.0.lock().unwrap() }
    async fn read(&self) -> std::sync::MutexGuard<'_, T> { self.0.lock().unwrap() }
}
#[derive(Default)]
struct Feedback;
impl Feedback {
    fn publish_local_permission(&self, _: &()) {}
    fn reconcile_state(&self, _: &()) {}
    async fn observe_state(&self, _: &PersistedAppState, _: &PersistedAppState) {}
    async fn state_reloaded(&self, _: &context::AppContext) {}
}
mod feedback { pub async fn observe_persisted_state(_: super::context::AppContext, _: super::PersistedAppState) {} }
struct Store {
    value: Mutex<PersistedAppState>,
    pause: Mutex<Option<(Arc<Barrier>, mpsc::Receiver<()>)>>,
    fail: bool,
}
impl Store {
    async fn load_state(&self) -> AppResult<PersistedAppState> {
        let snapshot = self.value.lock().unwrap().clone();
        if let Some((started, release)) = self.pause.lock().unwrap().take() { started.wait(); release.recv().unwrap(); }
        if self.fail { return Err("load failed"); }
        // Production load migrates and saves, so a stale read can erase disk too.
        *self.value.lock().unwrap() = snapshot.clone();
        Ok(snapshot)
    }
    async fn save_state(&self, state: &PersistedAppState) -> AppResult<()> {
        *self.value.lock().unwrap() = state.clone(); Ok(())
    }
}
mod context {
    use super::*;
    #[derive(Clone)]
    pub struct AppContext { pub(super) state: Arc<Lock<PersistedAppState>>, pub(super) state_store: Arc<Store>, pub(super) feedback: Arc<Feedback> }
    impl AppContext { ${methods} }
}
struct Noop;
impl Wake for Noop { fn wake(self: Arc<Self>) {} }
fn run<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(Noop));
    let mut context = Context::from_waker(&waker);
    loop { if let Poll::Ready(value) = future.as_mut().poll(&mut context) { return value; } std::thread::yield_now(); }
}
fn context(store: Store, granted: bool) -> context::AppContext {
    context::AppContext { state: Arc::new(Lock(Mutex::new(PersistedAppState { granted, feedback_program: () }))),
        state_store: Arc::new(store), feedback: Arc::new(Feedback) }
}
#[test]
fn reload_and_save_are_serialized() {
    let started = Arc::new(Barrier::new(2));
    let (release, receiver) = mpsc::channel();
    let context = context(Store { value: Mutex::new(PersistedAppState::default()),
        pause: Mutex::new(Some((started.clone(), receiver))), fail: false }, false);
    let reloader = context.clone();
    let reload = std::thread::spawn(move || run(reloader.reload_state_from_disk()).unwrap());
    started.wait();
    let reload_holds_state = context.state.0.try_lock().is_err();
    let writer = context.clone();
    let save = std::thread::spawn(move || run(writer.update_state(|state| state.granted = true)).unwrap());
    release.send(()).unwrap();
    reload.join().unwrap(); save.join().unwrap();
    assert!(reload_holds_state, "reload must lock state BEFORE loading/saving disk");
    assert!(run(context.load_state()).granted, "concurrent saved choice lost from memory");
    assert!(run(context.state_store.load_state()).unwrap().granted, "saved choice lost across restart");
}
#[test]
fn failed_reload_keeps_choice_and_releases_lock() {
    let context = context(Store { value: Mutex::new(PersistedAppState::default()), pause: Mutex::new(None), fail: true }, true);
    assert!(run(context.reload_state_from_disk()).is_err());
    assert!(run(context.load_state()).granted);
    run(context.update_state(|state| state.granted = false)).unwrap();
    assert!(!run(context.load_state()).granted);
}`;
  const directory = mkdtempSync(join(tmpdir(), 'noland-permission-test-'));
  try {
    const path = join(directory, 'reload.rs');
    const binary = join(directory, process.platform === 'win32' ? 'reload.exe' : 'reload');
    writeFileSync(path, harness);
    const compiled = spawnSync('rustc', ['--edition=2021', '--test', '-A', 'dead_code', path, '-o', binary], { encoding: 'utf8', timeout: 30000 });
    assert.equal(compiled.status, 0, compiled.error?.message ?? compiled.stderr);
    const result = spawnSync(binary, [], { encoding: 'utf8', timeout: 10000 });
    assert.equal(result.status, 0, result.error?.message ?? `${result.stdout}\n${result.stderr}`);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
