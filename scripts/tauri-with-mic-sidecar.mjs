#!/usr/bin/env node
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const [mode = 'build', ...tauriArgs] = process.argv.slice(2);

const requestedTarget = readTarget(tauriArgs);
const target = requestedTarget ?? defaultHostTarget();
const tauriArgsWithTarget = requestedTarget || !target ? tauriArgs : [...tauriArgs, '--target', target];
const prepArgs = [resolve(repoRoot, 'scripts', 'prepare-mic-sidecar.mjs'), mode, ...tauriArgsWithTarget];
const prepEnv = {
  ...process.env,
  ...(target ? { NOLAND_MIC_SENDER_TARGET: target } : {}),
};

const prep = spawnSync(process.execPath, prepArgs, {
  cwd: repoRoot,
  stdio: 'inherit',
  env: prepEnv,
});
if (prep.status !== 0) {
  process.exit(prep.status ?? 1);
}

const tauriEnv = {
  ...process.env,
};

if (mode === 'dev') {
  const sidecarTargetDir = join(repoRoot, 'src-tauri', '.native-deps', 'mic-sidecar-target');
  const profileDir = 'debug';
  const executableName = process.platform === 'win32' ? 'noland-mic-sender.exe' : 'noland-mic-sender';
  tauriEnv.NOLAND_MIC_SENDER_BIN = target
    ? join(sidecarTargetDir, target, profileDir, executableName)
    : join(sidecarTargetDir, profileDir, executableName);
}

const tauri = spawnSync('npx', ['tauri', mode, ...tauriArgsWithTarget], {
  cwd: repoRoot,
  stdio: 'inherit',
  env: tauriEnv,
});
if (tauri.status !== 0) {
  process.exit(tauri.status ?? 1);
}

if (process.platform === 'darwin' && mode === 'build') {
  const targetTriple = target ?? 'aarch64-apple-darwin';
  let fix = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'fix-macos-bundle-deps.mjs'), targetTriple], {
    cwd: repoRoot,
    stdio: 'inherit',
    env: process.env,
  });
  if (fix.status !== 0) {
    process.exit(fix.status ?? 1);
  }

  if (!bundleHasBundledSdl3(targetTriple)) {
    console.warn('First macOS bundle dependency fix did not leave libSDL3.dylib in the finished app bundle; retrying fix step once.');
    fix = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'fix-macos-bundle-deps.mjs'), targetTriple], {
      cwd: repoRoot,
      stdio: 'inherit',
      env: process.env,
    });
    if (fix.status !== 0) {
      process.exit(fix.status ?? 1);
    }
  }

  if (!bundleHasBundledSdl3(targetTriple)) {
    console.error('macOS bundle verification failed: libSDL3.dylib is still missing from the finished app bundle.');
    process.exit(1);
  }
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

function defaultHostTarget() {
  if (process.platform === 'darwin') {
    if (process.arch === 'arm64') return 'aarch64-apple-darwin';
    if (process.arch === 'x64') return 'x86_64-apple-darwin';
  }
  if (process.platform === 'win32') {
    if (process.arch === 'x64') return 'x86_64-pc-windows-msvc';
    if (process.arch === 'arm64') return 'aarch64-pc-windows-msvc';
  }
  if (process.platform === 'linux') {
    if (process.arch === 'x64') return 'x86_64-unknown-linux-gnu';
    if (process.arch === 'arm64') return 'aarch64-unknown-linux-gnu';
  }
  return undefined;
}

function bundleHasBundledSdl3(targetTriple) {
  const productName = 'Noland Connect';
  const candidates = [
    join(repoRoot, 'src-tauri', 'target', 'release', 'bundle', 'macos', `${productName}.app`),
    join(repoRoot, 'src-tauri', 'target', targetTriple, 'release', 'bundle', 'macos', `${productName}.app`),
  ].filter((appPath, index, all) => all.indexOf(appPath) === index && existsSync(appPath));

  if (candidates.length === 0) {
    return false;
  }

  return candidates.some((appPath) => existsSync(join(appPath, 'Contents', 'Frameworks', 'libSDL3.dylib')));
}
