import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';

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
  && ['quitWindow', 'deleteAllAndQuit'].includes(node.name?.text)).map((node) => node.getText(appSource)).join('\n');

test('close listener reads current rental state without needing re-registration', async () => {
  const effect = app.body.statements.find((node) => ts.isExpressionStatement(node)
    && ts.isCallExpression(node.expression) && node.expression.expression.getText(appSource) === 'useEffect'
    && node.getText(appSource).includes('currentWindow.onCloseRequested'));
  const callback = effect.expression.arguments[0].getText(appSource);
  let listener; let opened = false; let prevented = 0; let removed = false;
  const current = { loading: false, rentedInstances: [] };
  const { register } = compile(`exports.register = ${callback};`, {}, {
    windowLabelResolved: true, windowLabel: 'main', isRunningInTauri: () => true,
    getCurrentWindow: () => ({ onCloseRequested: async (handler) => {
      listener = handler; return () => { removed = true; };
    } }),
    useAppStore: { getState: () => current }, closeActionRunningRef: { current: false },
    setCloseGuardError() {}, setCloseGuardOpen: (value) => { opened = value; },
  });
  const cleanup = register();
  const event = { preventDefault: () => { prevented++; } };
  listener(event);
  assert.equal(prevented, 0);
  current.rentedInstances = [{ instanceId: 42 }];
  listener(event);
  assert.equal(opened, true);
  assert.equal(prevented, 1);
  cleanup();
  await Promise.resolve();
  assert.equal(removed, true);
});

test('failed quit after deletion resets popup state so the user can retry', async () => {
  let deleting = false; let opened = true; let error = null;
  const running = { current: false };
  const destroyed = [];
  const handlers = compile(`${closeFunctions}\nexports.remove = deleteAllAndQuit;`, {}, {
    closeActionRunningRef: running,
    useAppStore: { getState: () => ({ rentedInstances: [{ instanceId: 42 }] }) },
    destroyInstance: async (id) => destroyed.push(id),
    exitApp: async () => { throw new Error('Exit refused'); },
    setDeletingBeforeClose: (value) => { deleting = value; },
    setCloseGuardOpen: (value) => { opened = value; },
    setCloseGuardError: (value) => { error = value; },
  });
  await handlers.remove();
  assert.deepEqual(destroyed, [42]);
  assert.equal(deleting, false);
  assert.equal(opened, true);
  assert.equal(running.current, false);
  assert.match(error, /Exit refused/);
});

test('failed rental destruction leaves the app open and unlocks the popup', async () => {
  let deleting = false; let exits = 0; let error;
  const running = { current: false };
  const handlers = compile(`${closeFunctions}\nexports.remove = deleteAllAndQuit;`, {}, {
    closeActionRunningRef: running,
    useAppStore: { getState: () => ({ rentedInstances: [{ instanceId: 42 }] }) },
    destroyInstance: async () => { throw new Error('Provider refused'); },
    exitApp: async () => { exits++; },
    setDeletingBeforeClose: (value) => { deleting = value; },
    setCloseGuardOpen() {}, setCloseGuardError: (value) => { error = value; },
  });
  await handlers.remove();
  assert.equal(exits, 0);
  assert.equal(deleting, false);
  assert.equal(running.current, false);
  assert.match(error, /Provider refused/);
});
