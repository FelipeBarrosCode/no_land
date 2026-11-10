import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = (path) => readFileSync(new URL(`../src-tauri/src/${path}`, import.meta.url), 'utf8');

test('startup errors stop the app instead of deleting or resetting persisted state', () => {
  const source = read('main.rs');
  assert.doesNotMatch(source, /reset_persisted_state|remove_file\s*\(\s*(?:&)?state_path/);
  for (const stage of ['state-load', 'state-normalization']) {
    assert.ok(source.includes(`write_startup_diagnostic("${stage}", &message);\n                    return Err(message.into());`), stage);
  }
  assert.doesNotMatch(source, /continuing with defaults|deleting and recreating/);
});

test('the state store propagates read, parse, migration and save failures', () => {
  const source = read('services/state_store.rs');
  const implementation = source.split('impl StateStore for JsonStateStore {')[1].split('#[cfg(test)]')[0];
  assert.doesNotMatch(source, /recover_invalid_state|fs::rename\(&self\.state_path/);
  assert.doesNotMatch(implementation, /\.exists\(\)|Err\([^)]*\)\s*=>\s*Map::new/);
  assert.ok(implementation.includes('fs::try_exists(&self.state_path).await?'));
  assert.ok(implementation.includes('let migrated = self.migrate_value(raw_value)?;'));
  assert.ok(implementation.includes('self.save_state(&migrated).await?;'));
  assert.ok(implementation.includes('let mut root_map = self.load_existing_root_map().await?;'));
});

test('Moonlight cannot silently replace malformed shared state with defaults', () => {
  const source = read('moonlight/infrastructure/persistence/state_repository.rs');
  assert.doesNotMatch(source, /recover_corrupt_state|fs::rename\(&self\.state_path/);
  assert.ok(source.includes('self.state_path.try_exists()?'));
  assert.ok(source.includes('serde_json::from_str(&contents)?'));
  assert.ok(source.includes('existing file was preserved'));
});

test('failed atomic writes only clean up the temporary file, never the original', () => {
  const source = read('utils/atomic_file.rs').split('#[cfg(test)]')[0];
  assert.ok(source.indexOf('file.write_all(contents)?') < source.indexOf('replace_file(&tmp_path, path)?'));
  assert.ok(source.indexOf('file.sync_all()?') < source.indexOf('replace_file(&tmp_path, path)?'));
  assert.deepEqual([...source.matchAll(/fs::remove_file\(([^)]+)\)/g)].map((match) => match[1]), ['&tmp_path']);
});
