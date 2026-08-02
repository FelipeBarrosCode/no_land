#!/usr/bin/env node
import { chmodSync, copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const srcTauriDir = join(repoRoot, 'src-tauri');
const sidecarCrateDir = join(repoRoot, 'mic-sidecar');
const mode = process.argv[2] ?? 'build';
const passthroughArgs = process.argv.slice(3);
const target = readTarget(passthroughArgs);
const release = mode === 'build';
const executableName = process.platform === 'win32' ? 'noland-mic-sender.exe' : 'noland-mic-sender';

const cargoArgs = ['build', '--manifest-path', join(sidecarCrateDir, 'Cargo.toml')];
if (release) cargoArgs.push('--release');
if (target) cargoArgs.push('--target', target);

const sidecarTargetDir = resolve(srcTauriDir, '.native-deps', 'mic-sidecar-target');

const cargo = spawnSync('cargo', cargoArgs, {
  cwd: repoRoot,
  stdio: 'inherit',
  env: {
    ...process.env,
    NOLAND_SKIP_APP_BUILD_RS: '1',
    CARGO_TARGET_DIR: sidecarTargetDir,
  },
});

if (cargo.status !== 0) {
  process.exit(cargo.status ?? 1);
}

const profileDir = release ? 'release' : 'debug';
const builtBinary = target
  ? join(sidecarTargetDir, target, profileDir, executableName)
  : join(sidecarTargetDir, profileDir, executableName);
const packagingTarget = release ? (target ?? defaultHostTarget()) : undefined;

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

if (packagingTarget) {
  const binariesDir = join(srcTauriDir, 'binaries');
  mkdirSync(binariesDir, { recursive: true });
  const stagedName = process.platform === 'win32'
    ? `noland-mic-sender-${packagingTarget}.exe`
    : `noland-mic-sender-${packagingTarget}`;
  const stagedBinary = join(binariesDir, stagedName);
  copyFileSync(builtBinary, stagedBinary);
  if (process.platform !== 'win32') {
    chmodSync(stagedBinary, 0o755);
  }
  console.log(`Staged mic sidecar for packaging: ${stagedBinary}`);
} else {
  console.log(`Prepared mic sidecar for local ${mode}: ${builtBinary}`);
}

console.log(`Mic sidecar target dir: ${sidecarTargetDir}`);

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
  return `${process.arch}-${process.platform}`;
}
