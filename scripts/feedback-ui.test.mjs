import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

// Same dependency-free node:test / transpileModule approach as the existing
// spend-alert queue tests. No tsc, application build, or native compilation.
const root = fileURLToPath(new URL('../', import.meta.url));
const require = createRequire(import.meta.url);
const plain = (value) => JSON.parse(JSON.stringify(value));
function loader(mocks = {}, globals = {}) {
  const modules = new Map();
  function load(relative) {
    const filename = path.resolve(root, relative);
    if (mocks[relative]) return mocks[relative];
    if (modules.has(filename)) return modules.get(filename).exports;
    const source = readFileSync(filename, 'utf8');
    const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    const module = { exports: {} };
    modules.set(filename, module);
    const localRequire = (name) => {
      if (mocks[name]) return mocks[name];
      if (!name.startsWith('.')) return require(name);
      const base = path.resolve(path.dirname(filename), name);
      const resolved = ['.ts', '.tsx', '.js'].map((ext) => base + ext).find(existsSync);
      assert.ok(resolved, `Cannot resolve ${name}`);
      return load(path.relative(root, resolved));
    };
    vm.runInNewContext(compiled, { module, exports: module.exports, require: localRequire,
      URL, setTimeout, clearTimeout, console, ...globals }, { filename });
    return module.exports;
  }
  return load;
}
const model = loader()('src/lib/feedbackModel.ts');
const dataModule = loader()('src/lib/feedbackData.ts');
const idA = '00000000-0000-4000-8000-000000000001';
const idB = '00000000-0000-4000-8000-000000000002';
const reference = `secure-store://feedback/${idB}`;
const active = (overrides = {}) => ({ ...plain(model.defaultFeedbackProgram()), enabled: true,
  enrollmentStatus: 'enrolled', installationId: idA, userId: idB, credentialRef: reference,
  deletionCodeRef: reference, consentEpoch: 1, privacyVersion: 'privacy-1', termsVersion: 'terms-1',
  consentedAt: '2026-10-25T00:00:00Z', configured: true, supported: true,
  statusMessage: null, pendingSessions: 0, ...overrides });
const summary = (providerMachineId = '42', overrides = {}) => ({ providerMachineId,
  reliability: { source: 'no_land', ratio: 0, eligibleSessions: 2, successfulSessions: 0, failedSessions: 2,
    basis: 'ready_and_paired_before_destroy', scope: 'retained_history' },
  jitter: null, reviews: { commentCount: 0, ratingCount: 0, averageScore: null }, asOf: '2026-10-25T00:00:00Z', ...overrides });
const deferred = () => { let resolve; let reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const emptyPage = () => ({ reviews: [], nextCursor: null });
function reader(transport = {}, now) {
  const calls = [];
  const client = dataModule.createFeedbackDataClient({
    summaries: async (ids) => { calls.push(['summaries', plain(ids)]); return { summaries: ids.map((id) => summary(id)) }; },
    reviews: async (id, cursor) => { calls.push(['reviews', id, cursor]); return emptyPage(); }, ...transport,
  }, now);
  return { client, calls };
}
function storeHarness(overrides = {}) {
  const calls = [];
  const backend = {
    feedbackProgramStatus: async () => { calls.push('status'); return model.normalizeFeedbackStatus(null); },
    feedbackProgramEnroll: async () => active(),
    feedbackProgramConsent: async (enabled, optionalContextEnabled) => active({ enabled, optionalContextEnabled }),
    feedbackProgramDelete: async () => active({ enabled: false, enrollmentStatus: 'deleted' }),
    feedbackMachineSummaries: async (ids) => { calls.push('summaries'); return { summaries: ids.map((id) => summary(id)) }; },
    feedbackMachineReviews: async () => { calls.push('reviews'); return emptyPage(); },
    feedbackReviewReport: async () => { calls.push('report'); },
    feedbackReviewBlockAuthor: async () => { calls.push('block'); }, ...overrides,
  };
  const store = loader({ 'src/lib/feedbackBackend.ts': backend })('src/features/feedback/feedbackStore.ts').useFeedbackStore;
  return { store, calls, backend };
}

test('absent, malformed, future, unsupported, or credential-less consent fails closed', () => {
  for (const raw of [null, {}, { enabled: true }, active({ schemaVersion: 2 }), active({ credentialRef: 'plaintext' }),
    active({ userId: 'not-a-uuid' }), active({ deletionCodeRef: null }), active({ optionalContextEnabled: 'true' }),
    active({ consentedAt: 'invalid' }), active({ pendingControlAction: 'delete' }), active({ supported: false }),
    active({ configured: false }), active({ collectionPaused: true }), active({ collectionPaused: 'false' })]) {
    assert.equal(model.normalizeFeedbackStatus(raw).enabled, false);
  }
  assert.equal(model.normalizeFeedbackStatus(active()).enabled, true);
  assert.equal(model.normalizeFeedbackStatus({ ...active(), credential: 'never-forward-this' }).credential, undefined);
  assert.equal(model.defaultFeedbackProgram().optionalContextEnabled, false);
});

test('structured pending consent/deletion controls and receipt references are mirrored while off', () => {
  const pending = { kind: 'delete', requestId: idA, expectedEpoch: 1, enabled: false, optionalContextEnabled: false };
  const result = model.normalizeFeedbackStatus(active({ pendingControlAction: pending, receiptId: idB, receiptStatus: 'pending', receiptTokenRef: reference }));
  assert.equal(result.enabled, false);
  assert.deepEqual(plain(result.pendingControlAction), pending);
  assert.equal(result.receiptId, idB);
  assert.equal(result.receiptTokenRef, reference);
  assert.equal(model.feedbackViewerKey(result), null);
});

test('machine IDs are explicit lossless strings; unsafe numeric rental IDs are never stringified', () => {
  for (const value of [null, undefined, 42, 9007199254740992, '0', '-1', '01', ' 42', '42.0', '1e3', '9223372036854775808']) {
    assert.equal(model.feedbackMachineId(value), null);
  }
  assert.equal(model.feedbackMachineId('9223372036854775807'), '9223372036854775807');
  assert.equal(model.feedbackInstanceId(42), '42');
  assert.equal(model.feedbackInstanceId(9007199254740992), null);
});

test('summary IDs deduplicate across offers and every request is capped at 100', async () => {
  const { client, calls } = reader(); client.setViewer('viewer');
  const ids = Array.from({ length: 205 }, (_, i) => String(i + 1));
  const result = await client.summaries([...ids, ...ids, null, 3]);
  assert.deepEqual(calls.map(([, batch]) => batch.length), [100, 100, 5]);
  assert.equal(Object.keys(result.value).length, 205);
});

test('only >=2 eligible outcomes replace Vast and two failures display No Land 0%', () => {
  const one = model.parseFeedbackSummary(summary('42', { reliability: { ...summary().reliability,
    ratio: 0, eligibleSessions: 1, failedSessions: 1 } }));
  assert.equal(model.displayFeedbackReliability(.999, one).source, 'Vast');
  const two = model.parseFeedbackSummary(summary());
  assert.deepEqual(plain(model.displayFeedbackReliability(.999, two)), { ratio: 0, source: 'No Land', sessions: 2 });
  const different = model.parseFeedbackSummary(summary('42', { reliability: { ...summary().reliability, ratio: null } }));
  assert.equal(model.displayFeedbackReliability(.999, different).source, 'Vast');
});

test('measured zero jitter is valid, absent/unsupported/incomplete observations are N/A', () => {
  const jitter = { meanMs: 0, minMs: 0, maxMs: 0, sampleCount: 60, contributingSessions: 1,
    highSampleRatio: 0, severeSampleRatio: 0, metricVersion: 'rtt_jitter_ewma_window60_report1s_v1', scope: 'all_routes' };
  assert.equal(model.parseFeedbackSummary(summary('42', { jitter })).jitter.meanMs, 0);
  for (const invalid of [null, { ...jitter, sampleCount: 0 }, { ...jitter, meanMs: NaN },
    { ...jitter, metricVersion: 'other_metric' }, { ...jitter, completeness: 'partial_window' }]) {
    assert.equal(model.parseFeedbackSummary(summary('42', { jitter: invalid })).jitter, null);
  }
});

test('server policy links must be safe HTTPS and published versions/contact/disclosure gate joining', () => {
  const notices = model.parseFeedbackDocument({ schemaVersion: 1, available: true, collectionPaused: false,
    privacyVersion: 'p1', termsVersion: 't1', privacyUrl: 'https://policy.test/privacy', termsUrl: 'https://policy.test/terms',
    deletionUrl: 'https://policy.test/delete', contact: 'Published operator contact', disclosure: 'Published disclosure', metrics: [] });
  assert.equal(model.canJoinFeedback(model.normalizeFeedbackStatus(null), notices), false);
  assert.equal(model.canJoinFeedback(model.normalizeFeedbackStatus(active({ enabled: false })), notices), true);
  for (const url of ['javascript:alert(1)', 'file:///tmp/secret', 'https://user:secret@policy.test', 'http://policy.test']) assert.equal(model.feedbackPolicyUrl(url), null);
  assert.equal(model.canJoinFeedback(model.normalizeFeedbackStatus(active()), { ...notices, contact: '' }), false);
});

test('review pages bound content, omit private fields, preserve plain text, and coarsen dates', () => {
  const review = { reviewId: idA, score: 8, comment: '<script>alert("x")</script>', publishedDate: '2026-10-25T12:34:56Z',
    contributorId: 'private', sessionId: 'private', providerInstanceId: 'private', hardwareSnapshot: { gpuName: '<GPU>', rentedGpuCount: 1, country: 'CA', region: 'North America' } };
  const page = model.parseFeedbackReviews({ reviews: [review], nextCursor: 'opaque' });
  assert.equal(page.reviews[0].comment, review.comment);
  assert.equal(page.reviews[0].publishedDate, '2026-10-25');
  assert.equal(page.reviews[0].sessionId, undefined);
  assert.equal(page.reviews[0].contributorId, undefined);
  assert.throws(() => model.parseFeedbackReviews({ reviews: Array(21).fill(review), nextCursor: null }), /Invalid/);
  const { FeedbackReviewContent } = loader()('src/features/feedback/FeedbackReviewContent.tsx');
  const markup = renderToStaticMarkup(React.createElement(FeedbackReviewContent, { review: page.reviews[0] }));
  assert.ok(markup.includes('&lt;script&gt;'));
  assert.ok(!markup.includes('<script>'));
  assert.ok(markup.includes('Rental hardware:'));
  assert.ok(!markup.includes('12:34:56'));
});

test('summary and cursor page cache expiry is hard 60 seconds and reads do not extend it', async () => {
  let now = 1000;
  const { client, calls } = reader({}, () => now); client.setViewer('viewer');
  const first = await client.summaries(['42']); await client.reviews('42', null);
  now += 59000;
  assert.equal((await client.summaries(['42'])).expiresAt, first.expiresAt);
  await client.reviews('42', null);
  assert.equal(calls.length, 2);
  now += 1000;
  await client.summaries(['42']); await client.reviews('42', null);
  assert.equal(calls.length, 4);
});

test('viewer changes and explicit moderation invalidation clear both caches', async () => {
  const { client, calls } = reader();
  await assert.rejects(client.summaries(['42']), /permission/);
  assert.equal(calls.length, 0);
  client.setViewer('viewer-A'); await client.summaries(['42']); await client.reviews('42', null);
  client.setViewer('viewer-B'); await client.summaries(['42']); await client.reviews('42', null);
  client.invalidate(); await client.summaries(['42']); await client.reviews('42', null);
  assert.equal(calls.length, 6);
  client.setViewer(null); await assert.rejects(client.reviews('42', null), /permission/);
});

test('read failure drops cached summaries/pages and partial responses fail closed', async () => {
  let fail = false; let requests = 0;
  const { client } = reader({ summaries: async (ids) => { requests++; return { summaries: ids.map((id) => summary(id)) }; },
    reviews: async () => { if (fail) throw new Error('offline'); return emptyPage(); } });
  client.setViewer('viewer'); await client.summaries(['42']); await client.reviews('42', null);
  fail = true; await assert.rejects(client.reviews('42', 'next'), /offline/);
  await client.summaries(['42']); assert.equal(requests, 2);
  const partial = reader({ summaries: async () => ({ summaries: [] }) }).client;
  partial.setViewer('viewer'); await assert.rejects(partial.summaries(['42']), /Incomplete/);
});

test('late responses cannot revive an earlier viewer or invalidate a new viewer cache', async () => {
  const delayed = deferred(); let requests = 0;
  const { client } = reader({ summaries: async (ids) => ++requests === 1 ? delayed.promise : { summaries: ids.map((id) => summary(id)) } });
  client.setViewer('old'); const old = client.summaries(['42']);
  client.setViewer('new'); await client.summaries(['42']);
  delayed.resolve({ summaries: [summary()] }); await assert.rejects(old, /changed/);
  await client.summaries(['42']); assert.equal(requests, 2);
});

test('review validation allows rating-only or comment-only, never defaults a score, and enforces max length', () => {
  assert.equal(model.normalizeFeedbackReview(null, '  '), null);
  assert.deepEqual(plain(model.normalizeFeedbackReview(null, ' hello ')), { score: null, comment: 'hello' });
  assert.deepEqual(plain(model.normalizeFeedbackReview(1, '')), { score: 1, comment: null });
  for (const score of [0, 11, 1.5, NaN]) assert.equal(model.normalizeFeedbackReview(score, 'hello'), null);
  assert.equal(model.normalizeFeedbackReview(10, 'x'.repeat(2001)), null);
  assert.equal(model.normalizeFeedbackReview(10, 'x'.repeat(2000)).comment.length, 2000);
});

test('review draft stages before destroy and storage errors cannot prevent destruction', async () => {
  const order = []; const warnings = [];
  await model.stageFeedbackThenDestroy(async () => { order.push('stage'); throw new Error('disk full'); }, async () => { order.push('destroy'); }, (message) => warnings.push(message));
  assert.deepEqual(order, ['stage', 'destroy']); assert.equal(warnings.length, 1);
  const ok = [];
  await model.stageFeedbackThenDestroy(async () => { ok.push('stage'); }, async () => { ok.push('destroy'); }, () => assert.fail('no warning'));
  assert.deepEqual(ok, ['stage', 'destroy']);
});

test('a hung native stage times out nonblockingly and provider failure is still returned', async () => {
  let destroyed = false; let warning = false;
  await model.stageFeedbackThenDestroy(() => new Promise(() => {}), async () => { destroyed = true; }, () => { warning = true; }, 5);
  assert.equal(destroyed, true); assert.equal(warning, true);
  await assert.rejects(model.stageFeedbackThenDestroy(async () => {}, async () => { throw new Error('provider destroy failed'); }, () => {}), /provider destroy failed/);
});

test('off-state startup refreshes only local status; no summary/review/enrollment request is automatic', async () => {
  const { store, calls } = storeHarness();
  assert.equal(store.getState().status.enabled, false);
  assert.deepEqual(calls, []);
  await store.getState().refreshStatus();
  await assert.rejects(store.getState().loadSummaries(['42']));
  await assert.rejects(store.getState().loadReviews('42', null));
  assert.deepEqual(calls, ['status']);
});

test('withdrawal disables reads immediately, preserves pending control, and rejects stale status responses', async () => {
  const control = deferred(); const delayedStatus = deferred(); let statusCalls = 0;
  const pending = active({ enabled: false, pendingControlAction: { kind: 'consent', requestId: idA,
    expectedEpoch: 1, enabled: false, optionalContextEnabled: false } });
  const { store } = storeHarness({ feedbackProgramStatus: async () => ++statusCalls === 1 ? active() : delayedStatus.promise,
    feedbackProgramConsent: async () => control.promise });
  await store.getState().refreshStatus(); assert.equal(store.getState().status.enabled, true);
  const oldStatus = store.getState().refreshStatus();
  const stop = store.getState().consent(false, false);
  assert.equal(store.getState().status.enabled, false);
  await assert.rejects(store.getState().loadSummaries(['42']));
  control.resolve(pending); await stop;
  delayedStatus.resolve(active()); await oldStatus;
  assert.equal(store.getState().status.enabled, false);
  assert.equal(store.getState().status.pendingControlAction.enabled, false);
});

test('failed withdrawal does not re-enable reads from an old persisted true bit', async () => {
  const { store } = storeHarness({ feedbackProgramStatus: async () => active(), feedbackProgramConsent: async () => { throw new Error('storage error'); } });
  await store.getState().refreshStatus();
  assert.equal(await store.getState().consent(false, false), false);
  await store.getState().refreshStatus();
  assert.equal(store.getState().status.enabled, false);
  assert.ok(store.getState().controlError);
});

test('report/block and read failures reset viewer-specific summary counts and pages', async () => {
  const { store, calls } = storeHarness({ feedbackProgramStatus: async () => active() });
  await store.getState().refreshStatus(); await store.getState().loadSummaries(['42']); await store.getState().loadReviews('42', null);
  await store.getState().block(idA); await store.getState().loadSummaries(['42']); await store.getState().loadReviews('42', null);
  await store.getState().report(idA, 'spam'); await store.getState().loadSummaries(['42']);
  assert.equal(calls.filter((call) => call === 'summaries').length, 3);
  assert.equal(calls.filter((call) => call === 'reviews').length, 2);
});

test('native adapter uses exact command names/arguments without any HTTP transport', async () => {
  const calls = [];
  const backend = loader({ 'src/lib/tauri.ts': { invokeSafe: async (command, args) => {
    calls.push([command, plain(args ?? {})]);
    if (command === 'feedback_review_eligibility') return { eligible: true };
    if (command === 'feedback_program_notices') return publishedProgram();
    return active();
  } } })('src/lib/feedbackBackend.ts');
  await backend.feedbackProgramStatus(); await backend.feedbackProgramNotices();
  await backend.feedbackProgramEnroll({ privacyVersion: 'p', termsVersion: 't', participationAccepted: true, optionalContextEnabled: false, optionalProfile: {} });
  await backend.feedbackProgramConsent(false, false); await backend.feedbackProgramDelete();
  await backend.feedbackMachineSummaries(['42']); await backend.feedbackMachineReviews('42', null);
  await backend.feedbackReviewReport(idA, 'spam'); await backend.feedbackReviewBlockAuthor(idA);
  await backend.feedbackReviewEligibility('7'); await backend.feedbackStageReview('7', null, 'hello');
  assert.deepEqual(calls.map(([name]) => name), ['feedback_program_status', 'feedback_program_notices', 'feedback_program_enroll', 'feedback_program_consent', 'feedback_program_delete',
    'feedback_machine_summaries', 'feedback_machine_reviews', 'feedback_review_report', 'feedback_review_block_author', 'feedback_review_eligibility', 'feedback_stage_review']);
  assert.deepEqual(calls[2][1], { request: { privacyVersion: 'p', termsVersion: 't', participationAccepted: true, optionalContextEnabled: false, optionalProfile: {} } });
  assert.deepEqual(calls[3][1], { enabled: false, optionalContextEnabled: false });
  assert.deepEqual(calls[5][1], { machineIds: ['42'] });
  assert.deepEqual(calls[6][1], { machineId: '42', cursor: null, limit: 20 });
  assert.deepEqual(calls[7][1], { reviewId: idA, reason: 'spam' });
  assert.deepEqual(calls[8][1], { reviewId: idA });
  assert.deepEqual(calls[10][1], { providerInstanceId: '7', score: null, comment: 'hello' });
});

// Lightweight hook/element harness exercises real component handlers using the
// installed React element runtime; it is not a DOM/browser accessibility test.
function componentHarness(relative, exportName, props, mocks, globals = {}) {
  const slots = [];
  let cursor = 0; let dirty = true; let tree; const effects = [];
  const hooks = {
    useState(initial) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = typeof initial === 'function' ? initial() : initial;
      return [slots[index], (next) => { slots[index] = typeof next === 'function' ? next(slots[index]) : next; dirty = true; }];
    },
    useRef(initial) { return hooks.useState(() => ({ current: initial }))[0]; },
    useId() { return hooks.useRef(`test-id-${cursor}`).current; },
    useMemo(factory, dependencies) {
      const index = cursor++;
      const previous = slots[index];
      if (!previous || dependencies.some((value, i) => !Object.is(value, previous.dependencies[i]))) {
        slots[index] = { dependencies, value: factory() };
      }
      return slots[index].value;
    },
    useEffect(effect, dependencies) {
      const index = cursor++;
      const previous = slots[index];
      if (!previous || dependencies.some((value, i) => !Object.is(value, previous.dependencies[i]))) {
        slots[index] = { dependencies, cleanup: previous?.cleanup };
        effects.push(() => { slots[index].cleanup?.(); slots[index].cleanup = effect(); });
      }
    },
  };
  const Component = loader({ react: hooks, ...mocks }, globals)(relative)[exportName];
  function render() {
    cursor = 0; dirty = false; tree = Component(props);
    for (const effect of effects.splice(0)) effect();
    return tree;
  }
  function nodes() {
    const out = [];
    function visit(node) {
      if (Array.isArray(node)) { node.forEach(visit); return; }
      if (!node || typeof node !== 'object' || !node.props) return;
      out.push(node); visit(node.props.children);
    }
    visit(tree); return out;
  }
  const text = (value) => Array.isArray(value) ? value.map(text).join('') : value?.props ? text(value.props.children) : typeof value === 'string' || typeof value === 'number' ? String(value) : '';
  return {
    render,
    nodes,
    text: () => text(tree),
    button(label) { const node = nodes().find((node) => node.props.onClick && text(node.props.children) === label); assert.ok(node, `Missing action: ${label}`); return node; },
    async flush() { for (let i = 0; i < 12; i++) { await Promise.resolve(); if (dirty) render(); } },
    dispose() { for (const slot of slots) slot?.cleanup?.(); },
  };
}
function componentStore(state) {
  const useFeedbackStore = (selector) => selector(state);
  useFeedbackStore.getState = () => state;
  return { useFeedbackStore };
}
const publishedProgram = () => ({ schemaVersion: 1, available: true, collectionPaused: false,
  privacyVersion: 'privacy-1', termsVersion: 'terms-1', privacyUrl: 'https://policy.test/privacy', termsUrl: 'https://policy.test/terms',
  deletionUrl: 'https://policy.test/delete', contact: 'Published operator contact', disclosure: 'Published disclosure', metrics: [] });

test('real enrollment component opens without registering, starts both consents unchecked, and cancels cleanly', async () => {
  const calls = [];
  const state = { status: model.normalizeFeedbackStatus({ ...model.defaultFeedbackProgram(), configured: true, supported: true }),
    statusError: null, controlError: null, controlBusy: false, refreshStatus: async () => { calls.push('local-status'); },
    enroll: async (request) => { calls.push(['enroll', plain(request)]); return true; }, consent: async () => true, deleteContributions: async () => true };
  const modal = componentHarness('src/features/feedback/FeedbackProgramModal.tsx', 'FeedbackProgramModal', { onClose: () => calls.push('close') }, {
    'src/features/feedback/feedbackStore.ts': componentStore(state), '@tauri-apps/plugin-opener': { openUrl: async () => {} },
    'src/lib/feedbackBackend.ts': { feedbackProgramNotices: async () => { calls.push('notices'); return publishedProgram(); } },
  });
  modal.render(); await modal.flush();
  const checkboxes = modal.nodes().filter((node) => node.type === 'input' && node.props.type === 'checkbox');
  assert.equal(checkboxes.length, 2); assert.ok(checkboxes.every((node) => node.props.checked === false));
  assert.equal(modal.button('Join feedback program').props.disabled, true);
  assert.equal(calls.filter(Array.isArray).length, 0);
  modal.button('Not now').props.onClick();
  assert.ok(calls.includes('close')); assert.equal(calls.filter(Array.isArray).length, 0);
  modal.dispose();
});

test('real enrollment accepts blank optional profile without implicitly opting into context', async () => {
  let request;
  const state = { status: model.normalizeFeedbackStatus({ ...model.defaultFeedbackProgram(), configured: true, supported: true }),
    statusError: null, controlError: null, controlBusy: false, refreshStatus: async () => {},
    enroll: async (value) => { request = plain(value); return true; }, consent: async () => true, deleteContributions: async () => true };
  const modal = componentHarness('src/features/feedback/FeedbackProgramModal.tsx', 'FeedbackProgramModal', { onClose: () => {} }, {
    'src/features/feedback/feedbackStore.ts': componentStore(state), '@tauri-apps/plugin-opener': { openUrl: async () => {} },
    'src/lib/feedbackBackend.ts': { feedbackProgramNotices: async () => publishedProgram() },
  });
  modal.render(); await modal.flush();
  const checkboxes = modal.nodes().filter((node) => node.type === 'input' && node.props.type === 'checkbox');
  checkboxes[1].props.onChange({ target: { checked: true } }); modal.render();
  assert.equal(modal.button('Join feedback program').props.disabled, false);
  modal.button('Join feedback program').props.onClick(); await modal.flush();
  assert.deepEqual(request, { privacyVersion: 'privacy-1', termsVersion: 'terms-1', participationAccepted: true,
    optionalContextEnabled: false, optionalProfile: {} });
  modal.dispose();
});

function destroyHarness({ enabled = true, eligible = true, failStage = false, failDestroy = false, instanceId = 7 } = {}) {
  const calls = [];
  const state = { status: model.normalizeFeedbackStatus(active({ enabled })), setWarning: (warning) => calls.push(['warning', warning]), invalidate: () => calls.push(['invalidate']) };
  const component = componentHarness('src/features/feedback/DestroyFeedbackConfirmation.tsx', 'DestroyFeedbackConfirmation', {
    instanceId, disabled: false, onCancel: () => calls.push(['cancel']), onDestroy: async (id) => { calls.push(['destroy', id]); if (failDestroy) throw new Error('provider'); },
  }, {
    'src/features/feedback/feedbackStore.ts': componentStore(state),
    'src/lib/feedbackBackend.ts': {
      feedbackReviewEligibility: async (id) => { calls.push(['eligibility', id]); return eligible; },
      feedbackStageReview: async (...args) => { calls.push(['stage', ...args]); if (failStage) throw new Error('storage'); },
    },
  });
  component.render();
  return { component, calls, state };
}

test('real destroy confirmation has no prompt/native eligibility call off or for unsafe rental IDs', async () => {
  for (const options of [{ enabled: false }, { instanceId: 9007199254740992 }]) {
    const { component, calls } = destroyHarness(options); await component.flush();
    assert.equal(calls.length, 0);
    assert.equal(component.nodes().filter((node) => node.type === 'textarea').length, 0);
    component.button('Confirm destroy').props.onClick(); await component.flush();
    assert.equal(calls.filter(([kind]) => kind === 'stage').length, 0);
    assert.equal(calls.filter(([kind]) => kind === 'destroy').length, 1);
    component.dispose();
  }
});

test('real destroy confirmation needs native frame eligibility, and Cancel stages/destroys nothing', async () => {
  const ineligible = destroyHarness({ eligible: false }); await ineligible.component.flush();
  assert.equal(ineligible.component.nodes().filter((node) => node.props.type === 'radio').length, 0);
  ineligible.component.dispose();
  const { component, calls } = destroyHarness(); await component.flush();
  assert.equal(component.nodes().filter((node) => node.props.type === 'radio').length, 10);
  assert.ok(component.nodes().filter((node) => node.props.type === 'radio').every((node) => !node.props.checked));
  assert.equal(component.button('Submit and destroy').props.disabled, true);
  component.button('Cancel').props.onClick(); await component.flush();
  assert.deepEqual(calls.map(([kind]) => kind), ['eligibility', 'cancel']);
  component.dispose();
});

test('real Submit stages optional review before existing destroy; stage failure remains nonblocking', async () => {
  const { component, calls } = destroyHarness({ failStage: true }); await component.flush();
  component.nodes().find((node) => node.props.type === 'radio' && node.props.value === 10).props.onChange();
  component.nodes().find((node) => node.type === 'textarea').props.onChange({ target: { value: ' Smooth ' } });
  component.render(); assert.equal(component.button('Submit and destroy').props.disabled, false);
  component.button('Submit and destroy').props.onClick(); await component.flush();
  assert.deepEqual(calls.find(([kind]) => kind === 'stage'), ['stage', '7', 10, 'Smooth']);
  assert.deepEqual(calls.map(([kind]) => kind), ['eligibility', 'stage', 'warning', 'destroy', 'cancel']);
  component.dispose();
});

test('real Skip stages a native tombstone and never invents score/comment; retry avoids duplicate staging', async () => {
  const skipped = destroyHarness(); await skipped.component.flush();
  skipped.component.button('Skip and destroy').props.onClick(); await skipped.component.flush();
  assert.deepEqual(skipped.calls.find(([kind]) => kind === 'stage'), ['stage', '7', null, null]);
  skipped.component.dispose();
  const { component, calls } = destroyHarness({ failDestroy: true }); await component.flush();
  component.nodes().find((node) => node.props.type === 'radio' && node.props.value === 8).props.onChange(); component.render();
  component.button('Submit and destroy').props.onClick(); await component.flush();
  component.button('Submit and destroy').props.onClick(); await component.flush();
  assert.equal(calls.filter(([kind]) => kind === 'stage').length, 1);
  assert.equal(calls.filter(([kind]) => kind === 'destroy').length, 2);
  assert.equal(calls.filter(([kind]) => kind === 'cancel').length, 0);
  component.dispose();
});

test('feedback JSX/TS parses and privacy-sensitive UI integration guards stay present', () => {
  const files = ['src/lib/feedbackModel.ts', 'src/lib/feedbackTypes.ts', 'src/lib/feedbackData.ts', 'src/lib/feedbackBackend.ts',
    'src/features/feedback/feedbackStore.ts', 'src/features/feedback/useFeedbackSummaries.ts', 'src/features/feedback/FeedbackDialog.tsx',
    'src/features/feedback/FeedbackProgramEntry.tsx', 'src/features/feedback/FeedbackProgramModal.tsx', 'src/features/feedback/MachineFeedback.tsx',
    'src/features/feedback/MachineReviewsModal.tsx', 'src/features/feedback/FeedbackReviewContent.tsx', 'src/features/feedback/DestroyFeedbackConfirmation.tsx',
    'src/features/servers/ServerPickerModal.tsx', 'src/features/shared-storage-manager/InstanceCardActions.tsx', 'src/features/settings/SettingsScreen.tsx', 'src/store/appStore.ts', 'src/app/App.tsx', 'src/lib/types.ts', 'src/lib/backend.ts'];
  for (const file of files) {
    const source = readFileSync(path.join(root, file), 'utf8');
    const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, file.endsWith('tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
    assert.equal(parsed.parseDiagnostics.length, 0, `${file}: ${parsed.parseDiagnostics.map((d) => d.messageText).join(', ')}`);
    if (file.includes('feedback') || file.includes('feedbackBackend')) {
      assert.doesNotMatch(source, /\bfetch\s*\(|XMLHttpRequest|dangerouslySetInnerHTML|localStorage|sessionStorage|navigator\.(?:userAgent|platform|userAgentData)/);
    }
  }
  const card = readFileSync(path.join(root, 'src/features/feedback/MachineFeedback.tsx'), 'utf8');
  assert.match(card, /aria-label=\{`View machine reviews/);
  assert.match(card, /event\.stopPropagation\(\)/);
  const destroy = readFileSync(path.join(root, 'src/features/feedback/DestroyFeedbackConfirmation.tsx'), 'utf8');
  assert.doesNotMatch(destroy, /embeddedMoonlightPaired|RuntimeConnected|SessionState/);
  assert.match(destroy, /useState<number \| null>\(null\)/);
  assert.match(destroy, /maxLength=\{2000\}/);
});

test('malformed summary counts cannot be mistaken for a confirmed zero', () => {
  for (const invalid of [undefined, -1, 1.5, NaN, '0']) {
    assert.throws(() => model.parseFeedbackSummary(summary('42', { reviews: { commentCount: invalid, ratingCount: 0, averageScore: null } })), /invalid.*counts/);
  }
  const invalidRatio = { ...model.parseFeedbackSummary(summary()), reliability: { ...summary().reliability, ratio: NaN } };
  assert.equal(model.displayFeedbackReliability(.99, invalidRatio).source, 'Vast');
});

test('a rejected overlapping control cannot undo an immediate local withdrawal gate', async () => {
  const pending = deferred();
  const { store } = storeHarness({ feedbackProgramStatus: async () => active(), feedbackProgramConsent: async () => pending.promise });
  await store.getState().refreshStatus();
  const stop = store.getState().consent(false, false);
  assert.equal(await store.getState().consent(true, false), false);
  pending.reject(new Error('disk unavailable')); await stop;
  await store.getState().refreshStatus();
  assert.equal(store.getState().status.enabled, false);
});

test('late moderation results cannot invalidate or fail reads for a new authenticated viewer', async () => {
  for (const fails of [false, true]) {
    const pending = deferred(); let currentStatus = active();
    const { store } = storeHarness({ feedbackProgramStatus: async () => currentStatus, feedbackReviewBlockAuthor: async () => pending.promise });
    await store.getState().refreshStatus();
    const blocking = store.getState().block(idA);
    const result = assert.doesNotReject(async () => { try { await blocking; } catch { assert.equal(fails, true); } });
    currentStatus = active({ userId: '00000000-0000-4000-8000-000000000003', consentEpoch: 2 });
    await store.getState().refreshStatus();
    await store.getState().loadSummaries(['42']);
    const revision = store.getState().dataRevision;
    if (fails) pending.reject(new Error('old viewer no longer authorized')); else pending.resolve({});
    await result;
    assert.equal(store.getState().dataRevision, revision);
    assert.equal(store.getState().dataError, null);
    await store.getState().loadReviews('42', null);
  }
});

const offerFixture = (overrides = {}) => ({ id: 1, machineId: '42', hostId: 8, hostLabel: 'Machine offer', locationLabel: 'Canada',
  country: 'CA', region: '', city: '', latitude: 0, longitude: 0, reliability: .99, gpuName: 'GPU', gpuRamMb: 8192, gpuCount: 1,
  cpuName: 'CPU', cpuCores: 4, internetDownMbps: 100, internetUpMbps: 50, hourlyPrice: .25, computeHourlyPrice: .2, storageHourlyPrice: .05,
  availableStorageGb: 100, estimatedDistanceKm: 100, score: 9, timeRemainingHours: 0, isVerified: false, isDatacenter: false,
  offerType: 'on-demand', hasStaticIp: true, hasAvx: true, ...overrides });

test('actual card feedback renders 0%, measured zero jitter, count/source, and a non-selecting review icon', () => {
  let opened = 0; let stopped = 0;
  const zero = { meanMs: 0, minMs: 0, maxMs: 0, sampleCount: 60, contributingSessions: 1,
    highSampleRatio: 0, severeSampleRatio: 0, metricVersion: 'rtt_jitter_ewma_window60_report1s_v1', scope: 'all_routes' };
  const props = { offer: offerFixture(), enabled: true, unavailable: false,
    summary: model.parseFeedbackSummary(summary('42', { jitter: zero })), onReviews: () => { opened++; } };
  const card = componentHarness('src/features/feedback/MachineFeedback.tsx', 'MachineFeedback', props, {});
  card.render();
  assert.match(card.text(), /Reliability 0\.0% · No Land/);
  assert.match(card.text(), /2 No Land sessions · Small sample/);
  assert.match(card.text(), /Original Vast: 99\.0%/);
  assert.match(card.text(), /Jitter 0\.0 ms/);
  assert.match(card.text(), /N\/A · 0 comments/);
  const icon = card.nodes().find((node) => node.props['aria-label']?.startsWith('View machine reviews'));
  icon.props.onClick({ stopPropagation: () => { stopped++; } });
  assert.equal(opened, 1); assert.equal(stopped, 1);
  props.summary = undefined; card.render(); assert.match(card.text(), /Jitter N\/A/); assert.match(card.text(), /Reliability 99\.0% · Vast/);
  props.offer.machineId = null; card.render();
  assert.equal(card.nodes().find((node) => node.props['aria-label']?.startsWith('View machine reviews')).props.disabled, true);
  props.enabled = false; card.render();
  assert.equal(card.nodes().some((node) => node.props['aria-label']?.startsWith('View machine reviews')), false);
  card.dispose();
});

function reviewsHarness(overrides = {}) {
  const calls = [];
  const firstReview = { reviewId: idA, score: 8, comment: 'First visible comment', publishedDate: '2026-10-25', hardwareSnapshot: null };
  const state = { status: model.normalizeFeedbackStatus(active()), dataRevision: 0, dataError: null,
    loadReviews: async (machineId, cursor) => {
      calls.push(['page', machineId, cursor]);
      return { value: { reviews: [firstReview], nextCursor: null }, expiresAt: Date.now() + 60_000 };
    },
    retryReads: () => { state.dataRevision++; },
    report: async (reviewId, reason) => { calls.push(['report', reviewId, reason]); state.dataRevision++; },
    block: async (reviewId) => { calls.push(['block', reviewId]); state.dataRevision++; }, ...overrides };
  const modal = componentHarness('src/features/feedback/MachineReviewsModal.tsx', 'MachineReviewsModal', {
    machineId: '42', offer: offerFixture(), onClose: () => calls.push(['close']),
  }, { 'src/features/feedback/feedbackStore.ts': componentStore(state),
    'src/features/feedback/useFeedbackSummaries.ts': { useFeedbackSummaries: () => ({ summaries: { '42': model.parseFeedbackSummary(summary('42', { reviews: { commentCount: 1, ratingCount: 1, averageScore: 8 } })) }, loading: false }) } });
  modal.render();
  return { modal, state, calls, firstReview };
}

test('review content is hidden in the very first render after viewer or visibility changes', async (t) => {
  const pending = deferred();
  const { modal, state } = reviewsHarness(); t.after(() => modal.dispose()); await modal.flush();
  const comments = () => modal.nodes().filter((node) => node.props.review).map((node) => node.props.review.comment);
  assert.deepEqual(comments(), ['First visible comment']);
  state.status = model.normalizeFeedbackStatus(active({ userId: '00000000-0000-4000-8000-000000000003', consentEpoch: 2 }));
  state.loadReviews = async () => pending.promise;
  modal.render(); assert.deepEqual(comments(), []);
  await modal.flush(); assert.deepEqual(comments(), []);
  modal.dispose(); pending.resolve({ value: emptyPage(), expiresAt: Date.now() + 60_000 });
});

test('review pagination appends, deduplicates, and stops a repeated opaque cursor', async (t) => {
  const { modal, state, firstReview } = reviewsHarness(); t.after(() => modal.dispose());
  state.loadReviews = async (_machineId, cursor) => ({ value: cursor === null ? { reviews: [firstReview], nextCursor: 'next' } : {
    reviews: [firstReview, { ...firstReview, reviewId: idB, comment: 'Second visible comment' }], nextCursor: 'next' }, expiresAt: Date.now() + 60_000 });
  state.dataRevision++; modal.render(); await modal.flush();
  modal.button('Load more reviews').props.onClick(); await modal.flush();
  assert.deepEqual(modal.nodes().filter((node) => node.props.review).map((node) => node.props.review.comment), ['First visible comment', 'Second visible comment']);
  assert.equal(modal.nodes().filter((node) => node.type === 'li').length, 2);
  assert.equal(modal.nodes().some((node) => node.props.onClick && node.props.children === 'Load more reviews'), false);
  modal.dispose();
});

test('review reporting and confirmed blocking send only the public review ID and chosen reason', async (t) => {
  const { modal, calls } = reviewsHarness(); t.after(() => modal.dispose()); await modal.flush();
  modal.button('Report').props.onClick(); modal.render();
  modal.nodes().find((node) => node.type === 'select').props.onChange({ target: { value: 'spam' } }); modal.render();
  modal.button('Send report').props.onClick(); await modal.flush();
  assert.deepEqual(calls.find(([kind]) => kind === 'report'), ['report', idA, 'spam']);
  modal.button('Block contributor…').props.onClick(); modal.render();
  assert.equal(calls.some(([kind]) => kind === 'block'), false);
  modal.button('Confirm block').props.onClick(); await modal.flush();
  assert.deepEqual(calls.find(([kind]) => kind === 'block'), ['block', idA]);
  assert.doesNotMatch(modal.text(), new RegExp(idA));
  modal.dispose();
});

test('destroy eligibility from an earlier viewer cannot display or stage a prompt for a new one', async () => {
  const { component, state, calls } = destroyHarness(); await component.flush();
  assert.equal(component.nodes().filter((node) => node.props.type === 'radio').length, 10);
  state.status = model.normalizeFeedbackStatus(active({ userId: '00000000-0000-4000-8000-000000000003', consentEpoch: 2 }));
  component.render();
  assert.equal(component.nodes().filter((node) => node.props.type === 'radio').length, 0);
  component.button('Confirm destroy').props.onClick(); await component.flush();
  assert.equal(calls.some(([kind]) => kind === 'stage'), false);
  assert.equal(calls.filter(([kind]) => kind === 'destroy').length, 1);
  component.dispose();
});

test('actual server picker keeps Vast sort/filter and selection separate from community display', async () => {
  const selected = [];
  const feedback = { enabled: true, loading: false, error: null, summaries: { '42': model.parseFeedbackSummary(summary()) } };
  const props = { open: true, onClose: () => {}, offers: [offerFixture({ id: 1, machineId: '42', reliability: .99, score: 10 }),
    offerFixture({ id: 2, machineId: '43', reliability: .95, score: 20 })], selectedOfferId: null,
    serverPreferences: { storageGb: 100, geolocationCountryCode: 'GLOBAL' }, storageGb: 100, availableCountries: [], searchingOffers: false,
    offersPage: 1, offersHasNextPage: false, busy: false, onSearchOffers: async () => {}, onNextPage: async () => {}, onPreviousPage: async () => {},
    onManualLocationSave: async () => {}, onSelectOffer: async (id) => selected.push(id), onUpdateServerPreferences: async () => {} };
  const picker = componentHarness('src/features/servers/ServerPickerModal.tsx', 'ServerPickerModal', props, {
    'src/lib/i18n.tsx': { translate: (key) => key, translateSource: (text) => text },
    'src/components/ui/AIPromptHelper.tsx': { AIPromptHelper: () => null },
    'src/prompts/appPrompts.ts': { APP_PROMPTS: {} },
    'src/features/feedback/MachineReviewsModal.tsx': { MachineReviewsModal: () => null },
    'src/features/feedback/feedbackStore.ts': { useFeedbackStore: (selector) => selector({ retryReads: () => {} }) },
    'src/features/feedback/useFeedbackSummaries.ts': { useFeedbackSummaries: () => feedback },
  }, { window: { addEventListener() {}, removeEventListener() {} } });
  const cards = () => picker.nodes().filter((node) => node.props.offer && node.props.onReviews);
  picker.render(); assert.deepEqual(cards().map((node) => node.props.offer.id), [2, 1]);
  picker.button('Highest Vast reliability').props.onClick(); picker.render();
  assert.deepEqual(cards().map((node) => node.props.offer.id), [1, 2]);
  feedback.summaries['43'] = model.parseFeedbackSummary(summary('43', { reliability: { ...summary().reliability, ratio: 1, successfulSessions: 2, failedSessions: 0 } }));
  picker.render(); assert.deepEqual(cards().map((node) => node.props.offer.id), [1, 2]);
  cards()[0].props.onReviews(); picker.render(); assert.deepEqual(selected, []);
  const inputs = picker.nodes().filter((node) => node.type === 'input');
  inputs.find((node) => node.props.placeholder === '95').props.onChange({ target: { value: '98' } }); picker.render();
  assert.deepEqual(cards().map((node) => node.props.offer.id), [1]);
  assert.equal(cards()[0].props.summary.reliability.ratio, 0);
  assert.deepEqual(selected, []);
  picker.dispose();
});
