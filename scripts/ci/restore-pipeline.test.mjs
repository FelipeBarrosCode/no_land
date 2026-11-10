import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import test from 'node:test';

const read = (path) => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
const restore = read('state-agent/crates/noland-restore/src/pipeline.rs');
const journal = read('state-agent/crates/noland-restore/src/journal.rs');
const agent = read('state-agent/crates/noland-state-agent/src/restore.rs');
const storage = read('state-agent/crates/noland-storage/src/rclone.rs');
const pack = read('state-agent/crates/noland-pack/src/lib.rs');

test('production agent uses one bounded restore pipeline, not tier-wide barriers', () => {
  assert.match(agent, /restore_pipelined\(/);
  assert.doesNotMatch(agent, /download_and_verify_to\(|\.publish_to\(/);
  assert.match(agent, /sample\.completed_entries/);
  assert.match(agent, /sample\.published_bytes/);
});

test('authenticated records go directly to destination partials without a decrypted CAS', () => {
  assert.match(restore, /download_stream_observed/);
  assert.match(restore, /SeekFrom::Start\(placement\.offset\)/);
  assert.match(restore, /O_NOFOLLOW/);
  assert.doesNotMatch(restore, /materialized\/\.chunks|extract_missing\(/);
  assert.match(pack, /let plain = decode_record/);
  assert.match(pack, /consume\(entry, &plain\)/);
});

test('journaling is checksummed, incremental, and durable before filesystem mutations', () => {
  assert.match(journal, /file\.sync_all\(\)/);
  assert.match(journal, /checksum mismatch/);
  assert.match(restore, /journal::append/);
  assert.match(restore, /PUBLICATION_BATCH/);
  assert.match(restore, /output\.sync_all\(\)/);
});

test('streaming network reads are ordered, bounded, and cancellable', () => {
  assert.match(storage, /vec!\["cat"\.into\(\)\]/);
  assert.match(storage, /consumer\.restart\(\)/);
  assert.match(storage, /consumer\.write\(/);
  assert.match(storage, /kill_on_drop\(true\)/);
  assert.match(restore, /used\.saturating_add\(\*size\) > budget/);
  assert.match(restore, /active\.swap_remove/);
});

test('all clients show published bytes even outside an isolated transfer phase', () => {
  const handoff = read('src-tauri/src/services/shared_storage/agent_handoff.rs');
  const store = read('src/store/appStore.ts');
  assert.match(handoff, /get\("published_bytes"\)/);
  assert.match(handoff, /get\("pipeline"\)/);
  assert.match(store, /event\.kind === "restore"/);
  assert.match(store, /\[event\.message, units, bytes,/);
});
