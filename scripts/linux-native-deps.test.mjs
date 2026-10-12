import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

test('native dependencies explicitly resolve the GStreamer/libunwind runner conflict', () => {
  // Capture package selection without running sudo or changing this computer.
  const commands = execFileSync('bash', ['-c',
    'sudo() { printf "%s\\n" "$*"; }; source "$1"',
    'test', 'scripts/ci/install-linux-native-deps.sh',
  ], { encoding: 'utf8' }).trim().split('\n');
  assert.equal(commands[0], 'apt-get update');
  const packages = commands[1].split(' ');
  assert.ok(packages.includes('libgstreamer1.0-dev'));
  assert.ok(packages.includes('libgstreamer-plugins-base1.0-dev'));
  assert.ok(packages.includes('libunwind-dev'));
});

test('remote microphone agent builds also explicitly select libunwind-dev', () => {
  const workflow = readFileSync('.github/workflows/_build.yml', 'utf8');
  const agentJob = workflow.split('  build-unix:')[0];
  const install = agentJob.split('\n').find((line) => line.includes('apt-get install'));
  assert.ok(install);
  assert.ok(install.split(' ').includes('libunwind-dev'));
});
