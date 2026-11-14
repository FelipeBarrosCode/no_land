import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = readFileSync(new URL('../../src-tauri/src/services/shared_storage/agent_runtime.rs', import.meta.url), 'utf8');
const between = (start, end) => source.split(start)[1].split(end)[0];

test('agent installation must finish before a compatible daemon can satisfy readiness', () => {
  const wait = between('async fn wait_for_bootstrap(', 'fn bootstrap_is_ready(');
  const receipt = wait.indexOf('status.parse::<i32>()');
  const health = wait.indexOf('call_agent_raw_unlocked(');
  assert.ok(receipt >= 0 && health > receipt);
  assert.doesNotMatch(wait, /probe_agent\(remote\)/u);
  assert.match(wait, /bootstrap_is_ready\(Some\(exit_code\), health\.as_ref\(\)\)/u);
  const ready = between('fn bootstrap_is_ready(', 'async fn read_bootstrap_log(');
  assert.match(ready, /exit_code == Some\(0\) && health\.is_some_and\(agent_api_is_compatible\)/u);
});

test('deployments and complete RPC exchanges share an endpoint admission gate', () => {
  const ensure = between('pub async fn ensure_state_agent(', 'async fn ensure_state_agent_exclusive(');
  assert.match(ensure, /agent_access\(remote\)/u);
  assert.match(ensure, /access\.write\(\)\.await/u);
  const rpc = between('pub async fn call_agent_raw(', 'async fn call_agent_raw_unlocked(');
  assert.match(rpc, /agent_access\(remote\)/u);
  assert.match(rpc, /access\.read\(\)\.await/u);
  assert.match(rpc, /call_agent_raw_unlocked\(remote, method, params\)\.await/u);
});

test('bootstrap workspaces cannot collide on a second or remove a previous workspace', () => {
  const stamp = between('fn deployment_stamp()', '// Keep this aligned');
  assert.match(stamp, /Uuid::new_v4\(\)\.as_u128\(\)\.to_string\(\)/u);
  assert.doesNotMatch(source, /rm -rf \{src\}/u);
  assert.match(source, /\{sudo\}mkdir \{src\}/u);
  assert.match(source, /sources changed while preparing the update/u);
});

test('incomplete replies fail closed without automatically retrying confirmed deletion', () => {
  const parser = between('fn parse_rpc_response(', '#[cfg(test)]');
  assert.match(parser, /output\.is_empty\(\)/u);
  assert.match(parser, /output\.last\(\) != Some\(&b'\\n'\)/u);
  assert.match(parser, /response ID did not match the request/u);
  assert.match(parser, /Check the operation status before retrying a confirmed action/u);
  assert.doesNotMatch(parser, /StartDeletion".*call_agent_raw|ensure_state_agent\(/su);
});
