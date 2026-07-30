#!/usr/bin/env node
import { chmodSync, copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const srcTauriDir = join(repoRoot, 'src-tauri');
const mode = process.argv[2] ?? 'build';
const passthroughArgs = process.argv.slice(3);
const target = readTarget(passthroughArgs);
const release = mode === 'build';
const executableName = process.platform === 'win32' ? 'noland-mic-sender.exe' : 'noland-mic-sender';

const cargoArgs = ['build', '--manifest-path', join(srcTauriDir, 'Cargo.toml'), '--bin', 'noland-mic-sender'];
if (release) cargoArgs.push('--release');
if (target) cargoArgs.push('--target', target);

const cargo = spawnSync('cargo', cargoArgs, {
  cwd: repoRoot,
  stdio: 'inherit',
  env: {
    ...process.env,
    NOLAND_SKIP_APP_BUILD_RS: '1',
  },
});

if (cargo.status !== 0) {
  process.exit(cargo.status ?? 1);
}

const profileDir = release ? 'release' : 'debug';
const builtBinary = target
  ? join(srcTauriDir, 'target', target, profileDir, executableName)
  : join(srcTauriDir, 'target', profileDir, executableName);

if (!existsSync(builtBinary)) {
  console.error(`Expected mic sidecar binary was not produced: ${builtBinary}`);
  process.exit(1);
}

if (process.platform === 'darwin') {
  const fix = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'fix-macos-dev-sidecar.mjs'), builtBinary], {
    cwd: repoRoot,
    stdio: 'inherit',
    env: process.env,
  });
  if (fix.status !== 0) {
    process.exit(fix.status ?? 1);
  }
}

if (release && target) {
  const binariesDir = join(srcTauriDir, 'binaries');
  mkdirSync(binariesDir, { recursive: true });
  const stagedName = process.platform === 'win32'
    ? `noland-mic-sender-${target}.exe`
    : `noland-mic-sender-${target}`;
  const stagedBinary = join(binariesDir, stagedName);
  copyFileSync(builtBinary, stagedBinary);
  if (process.platform !== 'win32') {
    chmodSync(stagedBinary, 0o755);
  }
  console.log(`Staged mic sidecar for packaging: ${stagedBinary}`);
} else {
  console.log(`Prepared mic sidecar for local ${mode}: ${builtBinary}`);
}

function readTarget(args) {
  const envTarget = process.env.NOLAND_MIC_SENDER_TARGET?.trim() || process.env.TAURI_ENV_TARGET_TRIPLE?.trim();
  if (envTarget) return envTarget;
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
