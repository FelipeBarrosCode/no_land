import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  createStorageHelperPreparation, startStorageBrowse, storageBrowseError, storageBrowseErrorKey,
} from "../src/lib/storageBrowse.ts";

const secret = "DO_NOT_RENDER_token_provider_stderr";
const cached = () => ({ entries: [{ path: "/catalog/game/current" }], scope: "scope-a", observedAt: "2026-11-15T00:00:00Z", freshness: "cached" });
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const codes = {
  storage_helper_missing: "helper_missing",
  storage_agent_unavailable: "agent_unavailable",
  storage_agent_incompatible: "agent_incompatible",
  storage_agent_updating: "agent_updating",
  storage_provider_unavailable: "provider_unavailable",
  storage_profile_changed: "scope_changed",
  storage_refresh_timeout: "refresh_timeout",
  storage_helper_install_failed: "helper_install_failed",
};

test("only allowlisted structured codes select owned copy; raw details never enter browse state", async () => {
  for (const [code, expected] of Object.entries(codes)) {
    const rows = cached(), updates = [];
    await startStorageBrowse(async refresh => {
      if (refresh) throw { code, message: secret, details: { stderr: secret, access_token: secret } };
      return rows;
    }, view => updates.push(view)).finished;
    const last = updates.at(-1);
    assert.equal(last.error, expected);
    assert.equal(last.snapshot, expected === "scope_changed" ? null : rows);
    assert.ok(storageBrowseErrorKey(last.error).startsWith("storage.browse."));
    assert.equal(JSON.stringify(updates).includes(secret), false);
  }
});

test("old errors, forged detail codes, inherited codes and JSON strings use the generic fallback", () => {
  for (const reason of [
    null, undefined, secret, JSON.stringify({ code: "storage_helper_missing", details: secret }),
    new Error(secret), { message: "storage_helper_missing", details: secret },
    { details: { code: "storage_helper_missing" } }, { code: "__proto__" },
    { code: "storage_helper_missing\n" }, { code: "storage_admin_command" },
    Object.create({ code: "storage_helper_missing" }),
  ]) assert.equal(storageBrowseError(reason), "refresh_failed");
  assert.equal(storageBrowseErrorKey("__proto__"), "storage.browse.failed");
});

test("forged accessors and proxy traps cannot leak strings or execute a code getter", () => {
  let accessed = false;
  const getter = { get code() { accessed = true; throw new Error(secret); } };
  assert.equal(storageBrowseError(getter), "refresh_failed");
  assert.equal(accessed, false);
  assert.equal(storageBrowseError(new Proxy({}, { getOwnPropertyDescriptor() { throw new Error(secret); } })), "refresh_failed");
});

test("a new PC without cached rows reports the helper reason without inventing saved data", async () => {
  const updates = [];
  await startStorageBrowse(async refresh => {
    if (refresh) throw { code: "storage_helper_missing", details: secret };
    return { ...cached(), entries: [], observedAt: null, freshness: "unavailable" };
  }, view => updates.push(view)).finished;
  assert.equal(updates.at(-1).snapshot, null);
  assert.equal(updates.at(-1).error, "helper_missing");
  const en = JSON.parse(readFileSync(new URL("../src/locales/en-US.json", import.meta.url)));
  assert.doesNotMatch(en["storage.browse.failed"], /saved|retained/i);
});

test("failed forced refresh downgrades a recent observation without discarding its rows or timestamp", async () => {
  const rows = { ...cached(), freshness: "fresh" }, updates = [];
  await startStorageBrowse(async refresh => {
    if (refresh) throw { code: "storage_provider_unavailable", details: secret };
    return rows;
  }, view => updates.push(view), true).finished;
  const last = updates.at(-1);
  assert.equal(last.snapshot.entries, rows.entries);
  assert.equal(last.snapshot.observedAt, rows.observedAt);
  assert.equal(last.snapshot.freshness, "cached");
  assert.equal(last.error, "provider_unavailable");
});

test("preparation is click-only and duplicate clicks share one authorized invocation", async () => {
  const install = deferred(), updates = [];
  let calls = 0, reloads = 0;
  const control = createStorageHelperPreparation(() => { calls++; return install.promise; }, value => updates.push(value), () => reloads++, () => true);
  assert.equal(calls, 0);
  assert.deepEqual(updates, []);
  const clicked = control.run();
  await control.run();
  assert.equal(calls, 1);
  assert.deepEqual(updates, [{ preparing: true, error: null }]);
  install.resolve();
  await clicked;
  assert.equal(reloads, 1);
  assert.deepEqual(updates.at(-1), { preparing: false, error: null });
});

test("helper failures are always locally sanitized and never trigger refresh or automatic retry", async () => {
  let calls = 0, reloads = 0;
  const updates = [];
  const control = createStorageHelperPreparation(async () => { calls++; throw { code: "storage_helper_missing", details: secret }; }, value => updates.push(value), () => reloads++, () => true);
  await control.run();
  assert.equal(calls, 1);
  assert.equal(reloads, 0);
  assert.deepEqual(updates.at(-1), { preparing: false, error: "helper_install_failed" });
  assert.equal(JSON.stringify(updates).includes(secret), false);
});

for (const failure of [false, true]) {
  test(`closing a view ignores late helper ${failure ? "failure" : "success"} without cancelling authorized remote work`, async () => {
    const install = deferred(), updates = [];
    let reloads = 0;
    const control = createStorageHelperPreparation(() => install.promise, value => updates.push(value), () => reloads++, () => true);
    const clicked = control.run();
    control.cancel();
    const count = updates.length;
    if (failure) install.reject(new Error(secret));
    else install.resolve();
    await clicked;
    assert.equal(updates.length, count);
    assert.equal(reloads, 0);
  });
}

for (const replacement of ["another PC", "another profile", "a reopened view of the same PC"]) {
  test(`a helper result cannot update ${replacement}`, async () => {
    const install = deferred(), updates = [], selected = {};
    let current = selected, calls = 0, reloads = 0;
    const control = createStorageHelperPreparation(() => { calls++; return install.promise; }, value => updates.push(value), () => reloads++, () => current === selected);
    const clicked = control.run();
    current = {};
    await control.run();
    install.resolve();
    await clicked;
    assert.equal(calls, 1);
    assert.equal(updates.length, 1);
    assert.equal(reloads, 0);
  });
}

test("stale click callbacks cannot start helper work and a closing state callback cannot trigger refresh", async () => {
  let current = false, calls = 0, reloads = 0;
  const control = createStorageHelperPreparation(async () => { calls++; }, value => { if (!value.preparing) current = false; }, () => reloads++, () => current);
  await control.run();
  assert.equal(calls, 0);
  current = true;
  await control.run();
  assert.equal(calls, 1);
  assert.equal(reloads, 0);
});

test("preparation success followed by refresh failure retains cached data with a safe reason", async () => {
  const rows = cached(), updates = [], calls = [];
  let request;
  const control = createStorageHelperPreparation(async () => {}, () => {}, () => {
    request = startStorageBrowse(async refresh => {
      calls.push(refresh);
      if (refresh) throw { code: "storage_refresh_timeout", details: secret };
      return rows;
    }, value => updates.push(value), true);
  }, () => true);
  assert.equal(request, undefined);
  await control.run();
  await request.finished;
  assert.deepEqual(calls, [false, true]);
  assert.equal(updates.at(-1).snapshot, rows);
  assert.equal(updates.at(-1).error, "refresh_timeout");
});
