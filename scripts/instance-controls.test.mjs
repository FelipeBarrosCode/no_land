import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';
import { Window as TauriWindow } from '@tauri-apps/api/window';

const require = createRequire(import.meta.url);
const read = (file) => readFileSync(new URL(`../${file}`, import.meta.url), 'utf8');
function compile(source, mocks = {}, globals = {}) {
  const exports = {};
  const code = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } }).outputText;
  vm.runInNewContext(code, { exports, require: (name) => mocks[name] ?? require(name), ...globals });
  return exports;
}

const storage = compile(read('src/lib/storageAllocation.ts'));
test('storage allocation uses the selected card capacity without rounding above it', () => {
  assert.equal(storage.storageAllocationLimit(80.9), 80);
  assert.equal(storage.storageAllocationLimit(20000), 10000);
  for (const value of [NaN, Infinity, -1]) assert.equal(storage.storageAllocationLimit(value), 0);
  for (const input of ['', ' ', 'NaN', 'Infinity', '-1', '0', '29', '80.5', '81']) {
    assert.equal(storage.isValidStorageAllocation(input, 80), false, input);
  }
  for (const input of ['30', '50', '80']) assert.equal(storage.isValidStorageAllocation(input, 80), true);
  assert.equal(storage.isValidStorageAllocation('30', 29), false);
});

function terminalHarness(open) {
  const slots = [];
  let cursor = 0;
  let pending = [];
  const hooks = {
    useState(initial) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = initial;
      return [slots[index], (next) => { slots[index] = typeof next === 'function' ? next(slots[index]) : next; }];
    },
    useRef(initial) { return hooks.useState({ current: initial })[0]; },
    useEffect(effect, dependencies) {
      const index = cursor++;
      const previous = slots[index];
      if (!previous || dependencies.some((value, i) => !Object.is(value, previous.dependencies[i]))) {
        pending.push(() => {
          previous?.cleanup?.();
          slots[index] = { dependencies, cleanup: effect() };
        });
      }
    },
  };
  const listeners = new Map();
  const closed = [];
  const output = [];
  let opens = 0;
  class Terminal {
    rows = 32; cols = 120;
    loadAddon() {} open() {} focus() {} dispose() {}
    onData() { return { dispose() {} }; }
    write(data) { output.push(data); }
    writeln(data) { output.push(data); }
  }
  const emit = (name, payload) => listeners.get(name)?.({ payload });
  const { InstanceTerminalModal } = compile(read('src/features/dashboard/InstanceTerminalModal.tsx'), {
    react: hooks,
    '@tauri-apps/api/event': { listen: async (name, callback) => {
      listeners.set(name, callback);
      return () => listeners.delete(name);
    } },
    '@xterm/xterm': { Terminal },
    '@xterm/addon-fit': { FitAddon: class { fit() {} } },
    '@xterm/xterm/css/xterm.css': {},
    '../../components/ui/Button': { Button: 'button' },
    '../../components/ui/ModalFrame': { ModalBody: 'div', ModalFrame: 'div' },
    '../../lib/i18n': { useLocalization: () => ({ t: (key) => key }), translate: (key) => key },
    '../../lib/backend': {
      openRemoteTerminal: async (id) => { opens++; return open ? open(id, emit) : { sessionId: 'session' }; },
      closeRemoteTerminal: async (id) => { closed.push(id); },
      resizeRemoteTerminal: async () => {}, writeRemoteTerminal: async () => {},
    },
  }, { ResizeObserver: class { observe() {} disconnect() {} } });
  let instance = { instanceId: 7, sshHost: 'host', sshPort: 22, label: 'Server' };
  function render() {
    cursor = 0;
    InstanceTerminalModal({ instance, onClose() {} });
    // Attach the terminal host ref before running effects, as React does.
    slots[0].current = {};
    for (const effect of pending.splice(0)) effect();
  }
  render();
  return {
    emit, closed, output, listeners,
    get opens() { return opens; },
    get status() { return slots[1]; },
    refresh(overrides = {}) { instance = { ...instance, ...overrides }; render(); },
    async flush() { for (let i = 0; i < 15; i++) await Promise.resolve(); },
    dispose() { for (const slot of slots) slot?.cleanup?.(); },
  };
}

test('instance polling does not reopen SSH, but an endpoint change does', async () => {
  const terminal = terminalHarness();
  await terminal.flush();
  terminal.emit('remote-terminal-output', { sessionId: 'session', data: 'ready' });
  assert.equal(terminal.status, 'connected');
  terminal.refresh({ label: 'Updated server label' });
  await terminal.flush();
  assert.equal(terminal.opens, 1);
  assert.deepEqual(terminal.closed, []);
  terminal.refresh({ sshPort: 2222 });
  await terminal.flush();
  assert.equal(terminal.opens, 2);
  assert.deepEqual(terminal.closed, ['session']);
  terminal.dispose();
});

test('early SSH output and failure are captured before the invoke response', async () => {
  const terminal = terminalHarness(async (_id, emit) => {
    emit('remote-terminal-output', { sessionId: 'session', data: 'Permission denied' });
    emit('remote-terminal-closed', { sessionId: 'session', exitCode: 255 });
    return { sessionId: 'session' };
  });
  await terminal.flush();
  assert.equal(terminal.status, 'error');
  assert.ok(terminal.output.includes('Permission denied'));
  terminal.refresh();
  await terminal.flush();
  assert.equal(terminal.opens, 1);
  assert.equal(terminal.status, 'error');
  terminal.dispose();
  assert.equal(terminal.listeners.size, 0);
});

test('closing while SSH is opening closes the late session and removes listeners', async () => {
  let resolve;
  const terminal = terminalHarness(() => new Promise((done) => { resolve = done; }));
  await terminal.flush();
  terminal.dispose();
  resolve({ sessionId: 'late-session' });
  await terminal.flush();
  assert.deepEqual(terminal.closed, ['late-session']);
  assert.equal(terminal.listeners.size, 0);
});

// Execute the actual close handlers without loading the entire app/native runtime.
const appSource = ts.createSourceFile('App.tsx', read('src/app/App.tsx'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const app = appSource.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === 'App');
const closeFunctions = app.body.statements.filter((node) => ts.isFunctionDeclaration(node)
  && ['requestQuit', 'quitWindow', 'cancelQuit', 'manageInstancesFromCloseGuard'].includes(node.name?.text)).map((node) => node.getText(appSource)).join('\n');

test('native close requests use one replayable UI dialog even with empty or unknown rentals', async () => {
  const effect = app.body.statements.find((node) => ts.isExpressionStatement(node)
    && ts.isCallExpression(node.expression) && node.expression.expression.getText(appSource) === 'useEffect'
    && node.getText(appSource).includes('"app:quit-requested"'));
  const callback = effect.expression.arguments[0].getText(appSource);
  const listeners = new Map();
  let opened = false; let removed = 0; let exits = 0; let pending = false;
  const { register } = compile(`${closeFunctions}\nexports.register = ${callback};`, {}, {
    windowLabelResolved: true, windowLabel: 'main', isRunningInTauri: () => true,
    getCurrentWindow: () => ({ listen: async (event, handler) => {
      listeners.set(event, handler); return () => { removed++; };
    } }),
    closeActionRunningRef: { current: false },
    setCloseGuardError() {}, setCloseGuardOpen: (value) => { opened = value; },
    setDeletingBeforeClose() {}, exitApp: async () => { exits++; },
    takeAppQuitRequest: async () => { const value = pending; pending = false; return value; },
  });
  const flush = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
  const cleanup = register();
  await flush();
  assert.equal(opened, false);
  pending = true;
  listeners.get('app:quit-requested')();
  await flush();
  assert.equal(opened, true);
  listeners.get('app:quit-dismissed')();
  assert.equal(opened, false);
  listeners.get('app:quit-requested')();
  await flush();
  assert.equal(opened, false, 'already consumed requests cannot create a second dialog');
  pending = true;
  listeners.get('app:quit-requested')();
  await flush();
  assert.equal(opened, true);
  assert.equal(exits, 0, 'no empty-rentals shortcut can silently quit');
  cleanup();
  await flush();
  assert.equal(removed, 2);
});

test('native main-window close and global Quit are intercepted before React', () => {
  const native = read('src-tauri/src/main.rs');
  const coordinator = read('src-tauri/src/services/app_exit.rs');
  assert.match(native, /window\.label\(\) == "main"[\s\S]*?api\.prevent_close\(\);[\s\S]*?request_app_quit\(window\.app_handle\(\)\)/);
  assert.match(native, /RunEvent::ExitRequested[\s\S]*?api\.prevent_exit\(\);[\s\S]*?request_app_quit\(app\)/);
  assert.match(coordinator, /claim_fallback/);
  assert.match(coordinator, /app_quit_dialog_ready/);
  assert.doesNotMatch(closeFunctions, /destroyInstance|deleteAllAndQuit|rentedInstances\.length/);
});

test('real Tauri close listener can destroy only the authorized main window', async () => {
  const capability = JSON.parse(read('src-tauri/capabilities/main-close.json'));
  assert.deepEqual(capability.windows, ['main']);
  assert.ok(capability.permissions.includes('core:window:allow-destroy'));
  let callback; let destroyed = 0; let guard = false;
  const window = {
    listen: async (_event, listener) => { callback = listener; return () => {}; },
    destroy: async () => {
      assert.ok(capability.permissions.includes('core:window:allow-destroy'));
      destroyed++;
    },
  };
  // Execute the installed SDK wrapper, including its implicit destroy() call.
  await TauriWindow.prototype.onCloseRequested.call(window, (event) => {
    if (guard) event.preventDefault();
  });
  await callback({ event: 'tauri://close-requested', id: 1, payload: null });
  assert.equal(destroyed, 1);
  guard = true;
  await callback({ event: 'tauri://close-requested', id: 2, payload: null });
  assert.equal(destroyed, 1, 'pending confirmation must keep the window open');
});

test('loading screen still renders the quit confirmation', () => {
  const guard = app.body.statements.find((node) => ts.isVariableStatement(node)
    && node.declarationList.declarations.some((item) => item.name.getText(appSource) === 'closeGuard'));
  const loadingBranch = app.body.statements.find((node) => ts.isIfStatement(node)
    && node.expression.getText(appSource) === 'loading');
  const { render } = compile(`exports.render = () => { ${guard.getText(appSource)} ${loadingBranch.getText(appSource)} return closeGuard; };`, {}, {
    closeGuardOpen: true, loading: true, instancesStatus: 'loading', quitActiveJobs: 2, rentedInstances: [], deletingBeforeClose: false, closeGuardError: null,
    CloseWithInstancesModal: 'quit-dialog', BootScreen: 'boot-screen',
    setCloseGuardOpen() {}, quitWindow() {}, manageInstancesFromCloseGuard() {},
  });
  const children = render().props.children;
  assert.equal(children[0].type, 'boot-screen');
  assert.equal(children[1].type, 'quit-dialog');
  assert.equal(children[1].props.instancesLoading, true);
});

test('failed quit resets popup state so the user can retry without deleting anything', async () => {
  let deleting = false; let opened = true; let error = null;
  const running = { current: false };
  const destroyed = [];
  const handlers = compile(`${closeFunctions}\nexports.quit = quitWindow;`, {}, {
    closeActionRunningRef: running,
    useAppStore: { getState: () => ({ rentedInstances: [{ instanceId: 42 }] }) },
    destroyInstance: async (id) => destroyed.push(id),
    exitApp: async () => { throw new Error('Exit refused'); },
    setDeletingBeforeClose: (value) => { deleting = value; },
    setCloseGuardOpen: (value) => { opened = value; },
    setCloseGuardError: (value) => { error = value; },
  });
  await handlers.quit();
  assert.deepEqual(destroyed, []);
  assert.equal(deleting, false);
  assert.equal(opened, true);
  assert.equal(running.current, false);
  assert.match(error, /Exit refused/);
});

test('Manage cloud PCs cancels the pending native quit and navigates without exiting', async () => {
  let opened = true; let cancelled = 0;
  const window = { location: { hash: '' } };
  const handlers = compile(`${closeFunctions}\nexports.manage = manageInstancesFromCloseGuard;`, {}, {
    window,
    cancelAppQuit: async () => { cancelled++; },
    setCloseGuardOpen: (value) => { opened = value; },
  });
  await handlers.manage();
  assert.equal(cancelled, 1);
  assert.equal(opened, false);
  assert.equal(window.location.hash, '#/');
});
