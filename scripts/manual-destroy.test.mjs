import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import test from 'node:test';

const read = (file) => readFileSync(new URL(`../${file}`, import.meta.url), 'utf8');
const lifecycle = read('src-tauri/src/services/instance_lifecycle.rs');
const destroyStart = lifecycle.indexOf('pub async fn destroy_instance(');
const backupStart = lifecycle.indexOf('async fn maybe_run_backup_first(', destroyStart);
const destroy = lifecycle.slice(destroyStart, backupStart);
const pause = lifecycle.slice(lifecycle.indexOf('pub async fn pause_instance('), destroyStart);

test('manual destruction reaches the provider without invoking backup or seal, regardless of storage settings', () => {
  assert.ok(destroyStart > 0 && backupStart > destroyStart);
  assert.match(destroy, /vast\.destroy_instance\(instance_id\)\.await\?/);
  assert.doesNotMatch(destroy, /Self::maybe_run_backup_first\(|SharedStorageManager::|start_agent_(?:backup|seal)\(|seal_before_stop\(/);
  assert.doesNotMatch(destroy, /shared_storage\.settings|shared_storage_profiles/);
  assert.match(destroy, /(?:begin_action|acquire_lock)\(context, instance_id, "destroy"\)|acquire_lock\(instance_id, "destroy"\)/);
  assert.match(destroy, /api_key\.trim\(\)\.is_empty\(\)/);
  assert.match(destroy, /reconcile_owned_instances/);
  assert.match(destroy, /remove_local_wireguard_config/);
});

test('pause and automatic stop routes still wait for a successful seal before the provider pause', () => {
  const backup = pause.indexOf('Self::maybe_run_backup_first(context, instance_id).await?');
  const provider = pause.indexOf('vast.pause_instance(instance_id).await?');
  assert.ok(backup >= 0 && provider > backup);
  assert.match(lifecycle.slice(backupStart), /SharedStorageManager::seal_before_stop\(context, &remote, instance_id, &target_user\)\.await\?/);
});

test('manual destroy confirmation and progress explicitly use the no-backup warning', () => {
  const component = (() => {
    try { return read('src/features/feedback/DestroyFeedbackConfirmation.tsx'); }
    catch { return read('src/features/feedback/DestroyInstanceConfirmation.tsx'); }
  })();
  assert.match(component, /translate\("instance\.destroy\.noBackup"\)/);
  assert.doesNotMatch(component, /translate\("(?:feedback\.destroy\.description|feedback\.2ade73bb98ac1b84|feedback\.4e7ebcf61c4fd052|feedback\.a12a8072e8160996)"\)/);
  const store = read('src/store/appStore.ts');
  const action = store.slice(store.indexOf('destroyInstance: async'), store.indexOf('destroyInstance: async') + 900);
  assert.match(action, /translate\("instance\.destroy\.noBackup"\)/);
  assert.doesNotMatch(action, /generated\.9bb868b4f3f40791/);
  for (const file of readdirSync(new URL('../src/locales/', import.meta.url)).filter((file) => file.endsWith('.json'))) {
    const locale = JSON.parse(read(`src/locales/${file}`));
    assert.ok(locale['instance.destroy.noBackup']?.length > 30, `${file}: missing data-loss warning`);
  }
  assert.match(JSON.parse(read('src/locales/en-US.json'))['instance.destroy.noBackup'], /without an automatic Shared Storage backup.*even if Shared Storage is enabled.*unsynced data will be lost/);
});
