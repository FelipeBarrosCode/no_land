#!/usr/bin/env node
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const [mode = 'build', ...tauriArgs] = process.argv.slice(2);

const target = readTarget(tauriArgs);
const prepArgs = [resolve(repoRoot, 'scripts', 'prepare-mic-sidecar.mjs'), mode, ...tauriArgs];
const prep = spawnSync(process.execPath, prepArgs, {
  cwd: repoRoot,
  stdio: 'inherit',
  env: {
    ...process.env,
    ...(target ? { NOLAND_MIC_SENDER_TARGET: target } : {}),
  },
});
if (prep.status !== 0) {
  process.exit(prep.status ?? 1);
}

const tauri = spawnSync('npx', ['tauri', mode, ...tauriArgs], {
  cwd: repoRoot,
  stdio: 'inherit',
  env: process.env,
});
if (tauri.status !== 0) {
  process.exit(tauri.status ?? 1);
}

if (process.platform === 'darwin' && mode === 'build') {
  const fix = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'fix-macos-bundle-deps.mjs'), target ?? 'aarch64-apple-darwin'], {
    cwd: repoRoot,
    stdio: 'inherit',
    env: process.env,
  });
  process.exit(fix.status ?? 1);
}

process.exit(0);

function readTarget(args) {
  for (let i = 0; i < args.length; i += 1) {
    if (args[i] === '--target' && args[i + 1]) {
      return args[i + 1];
    }
    if (args[i].startsWith('--target=')) {
      return args[i].slice('--target='.length);
    }
  }
  return undefined;
}
