import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import test from "node:test";
import ts from "typescript";
import * as browse from "../src/lib/storageBrowse.ts";

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const secret = "RAW_STDERR_ACCESS_TOKEN_MUST_NOT_RENDER";
const en = JSON.parse(read("src/locales/en-US.json"));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const flush = () => new Promise(resolve => setImmediate(resolve));
function compile(source, fileName, mocks = {}, globals = {}) {
  const module = { exports: {} };
  const js = ts.transpileModule(source, { fileName, compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } }).outputText;
  runInNewContext(js, { module, exports: module.exports, require(name) {
    assert.ok(Object.hasOwn(mocks, name), `Unexpected import: ${name}`);
    return mocks[name];
  }, ...globals });
  return module.exports;
}
const jsx = (type, props) => ({ type, props });
const { StorageBrowseStatus } = compile(
  read("src/features/shared-storage-manager/StorageBrowseStatus.tsx"), "StorageBrowseStatus.tsx",
  {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "../../components/ui/Button": { Button: "button" },
    "../../lib/i18n": { translate: key => en[key] ?? key },
    "../../lib/storageBrowse": browse,
  },
);
function elements(node) {
  if (Array.isArray(node)) return node.flatMap(elements);
  if (!node || typeof node !== "object") return [];
  return [node, ...elements(node.props?.children)];
}
const props = extra => ({
  snapshot: null, refreshing: false, error: null, slow: false,
  preparingHelper: false, onRefresh() {}, onPrepareHelper() {}, ...extra,
});

test("helper buttons are click-only, disabled while working, and unknown reasons never render", () => {
  let clicks = 0;
  const tree = StorageBrowseStatus(props({ error: "helper_missing", onPrepareHelper: () => clicks++ }));
  assert.equal(clicks, 0);
  const button = elements(tree).find(node => node.type === "button" && node.props.children === en["storage.browse.prepareHelper"]);
  assert.ok(button);
  assert.equal(button.props.disabled, false);
  button.props.onClick();
  assert.equal(clicks, 1);
  const working = elements(StorageBrowseStatus(props({ error: "helper_missing", preparingHelper: true }))).filter(node => node.type === "button");
  assert.ok(working.every(node => node.props.disabled));
  const retry = StorageBrowseStatus(props({ error: "helper_install_failed" }));
  assert.ok(elements(retry).some(node => node.props.children === en["storage.browse.prepareHelper"]));
  const forged = JSON.stringify(StorageBrowseStatus(props({ error: secret })));
  assert.equal(forged.includes(secret), false);
  assert.ok(forged.includes(en["storage.browse.failed"]));
  assert.doesNotMatch(en["storage.browse.failed"], /saved|retained/i);
});

test("both real native adapters preserve explicit instance, cache intent and structured errors", async () => {
  const source = ts.createSourceFile("backend.ts", read("src/lib/backend.ts"), ts.ScriptTarget.Latest, true);
  const names = new Set(["getInstanceStorageBrowseSnapshot", "prepareInstanceStorageHelper"]);
  const code = source.statements.filter(node => ts.isFunctionDeclaration(node) && names.has(node.name?.text)).map(node => node.getText(source)).join("\n");
  assert.equal([...code.matchAll(/export async function /g)].length, 2);
  const calls = [];
  let failure;
  const adapter = compile(code, "backend.ts", {}, { invokeSafe: async (command, args) => {
    calls.push([command, JSON.parse(JSON.stringify(args))]);
    if (failure) throw failure;
  } });
  assert.deepEqual(calls, []);
  await adapter.getInstanceStorageBrowseSnapshot(60812509, "cloud");
  await adapter.prepareInstanceStorageHelper(60812509);
  assert.deepEqual(calls, [
    ["get_instance_storage_browse_snapshot", { instanceId: 60812509, target: "cloud", refresh: false }],
    ["prepare_instance_storage_helper", { instanceId: 60812509 }],
  ]);
  failure = { code: "storage_helper_install_failed", message: secret, details: secret };
  await assert.rejects(adapter.prepareInstanceStorageHelper(42), error => error === failure);
  assert.equal(calls.length, 3, "there is no automatic retry, scan or fallback command");
});

// A small synchronous hook host: imports/timers/native work are mocked, while
// the actual hook and preparation controller run unchanged.
function mountHook() {
  const slots = [], pending = [];
  let cursor = 0, writes = 0;
  const control = { profiles: [{ id: "profile-a", active: true }], installed: false, freshCache: false };
  const install = deferred(), calls = { browse: [], prepare: [] };
  const rows = [{ path: "/catalog/game/current", name: "Game", parentPath: "/catalog", isDir: false }];
  const same = (a, b) => a?.length === b.length && b.every((value, index) => Object.is(value, a[index]));
  const react = {
    useRef(initial) { const index = cursor++; return slots[index] ??= { current: initial }; },
    useState(initial) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = initial;
      return [slots[index], value => { writes++; slots[index] = typeof value === "function" ? value(slots[index]) : value; }];
    },
    useCallback(callback, deps) {
      const index = cursor++, old = slots[index];
      if (!old || !same(old.deps, deps)) slots[index] = { deps, callback };
      return slots[index].callback;
    },
    useEffect(effect, deps) {
      const index = cursor++, old = slots[index];
      if (!old || !same(old.deps, deps)) {
        const next = { deps, cleanup: old?.cleanup };
        slots[index] = next;
        pending.push(() => { old?.cleanup?.(); next.cleanup = effect(); });
      }
    },
  };
  const { useStorageBrowse } = compile(read("src/features/shared-storage-manager/useStorageBrowse.ts"), "useStorageBrowse.ts", {
    react,
    "../../lib/backend": {
      async getInstanceStorageBrowseSnapshot(id, target, refresh) {
        calls.browse.push([id, target, refresh]);
        if (refresh) throw { code: control.installed ? "storage_provider_unavailable" : "storage_helper_missing", details: secret };
        return { entries: rows, scope: `pc-${id}`, observedAt: "2026-11-15T00:00:00Z", freshness: control.freshCache ? "fresh" : "cached" };
      },
      prepareInstanceStorageHelper(id) { calls.prepare.push(id); return install.promise; },
    },
    "../../lib/storageBrowse": browse,
    "../../store/appStore": { useAppStore: selector => selector({ sharedStorageProfiles: control.profiles }) },
  }, { window: { setTimeout: () => 1, clearTimeout() {} } });
  const effects = () => { while (pending.length) pending.shift()(); };
  return {
    control, install, calls, rows, effects, writes: () => writes,
    render(open = true, id = 60812509, flushEffects = true) {
      cursor = 0;
      const view = useStorageBrowse(open, id, "cloud");
      if (flushEffects) effects();
      return view;
    },
  };
}

test("actual hook never installs on open/error/refresh; a human success forces refresh and retains rows on failure", async () => {
  const h = mountHook();
  h.render();
  await flush();
  let view = h.render();
  assert.equal(view.error, "helper_missing");
  assert.deepEqual(h.calls.prepare, []);
  view.reload();
  h.render();
  await flush();
  view = h.render();
  assert.deepEqual(h.calls.prepare, [], "Refresh is still read-only");
  view.prepareHelper();
  view.prepareHelper();
  assert.deepEqual(h.calls.prepare, [60812509]);
  assert.equal(h.render().preparingHelper, true);
  h.control.installed = true;
  h.control.freshCache = true;
  h.install.resolve();
  await flush();
  h.render();
  await flush();
  view = h.render();
  assert.deepEqual(h.calls.browse.slice(-2), [[60812509, "cloud", false], [60812509, "cloud", true]]);
  assert.equal(view.preparingHelper, false);
  assert.equal(view.error, "provider_unavailable");
  assert.equal(view.snapshot.entries, h.rows);
  assert.equal(view.snapshot.freshness, "cached");
  assert.equal(JSON.stringify(view).includes(secret), false);
});

for (const replacement of ["close", "PC", "profile"]) {
  test(`actual hook ignores helper completion after ${replacement} even before effect cleanup`, async () => {
    const h = mountHook();
    h.render(); await flush();
    const original = h.render();
    original.prepareHelper();
    if (replacement === "profile") h.control.profiles = [{ id: "profile-b", active: true }];
    h.render(replacement !== "close", replacement === "PC" ? 42 : 60812509, false);
    const count = h.writes(), reads = h.calls.browse.length;
    original.prepareHelper();
    h.install.resolve();
    await flush();
    assert.equal(h.writes(), count);
    assert.equal(h.calls.browse.length, reads);
    assert.deepEqual(h.calls.prepare, [60812509]);
    h.effects();
  });
}

test("all allowlisted reasons and helper actions have locally-owned translations in every locale", () => {
  const keys = Object.keys(en).filter(key => key.startsWith("storage.browse."));
  assert.equal(keys.length, 17);
  for (const locale of ["en-US", "pt-BR", "es", "fr", "de", "it", "ja", "ko", "zh-CN"]) {
    const dictionary = JSON.parse(read(`src/locales/${locale}.json`));
    for (const key of keys) {
      assert.ok(dictionary[key]?.trim(), `${locale}: ${key}`);
      if (locale !== "en-US") assert.notEqual(dictionary[key], en[key], `${locale}: ${key}`);
    }
  }
});
