#!/usr/bin/env node
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const repoRoot = resolve(__dirname, '..');
const gstreamerVersion = process.env.NOLAND_GSTREAMER_VERSION?.trim()
  || process.env.GSTREAMER_VERSION?.trim()
  || '1.24.13';
const [mode = 'build', ...tauriArgs] = process.argv.slice(2);

const requestedTarget = readTarget(tauriArgs);
const target = requestedTarget ?? defaultHostTarget();
const tauriArgsWithTarget = requestedTarget || !target ? tauriArgs : [...tauriArgs, '--target', target];
const tauriCliArgs = mode === 'build'
  ? ensureCargoFeature(tauriArgsWithTarget, 'moonlight-config-bin')
  : tauriArgsWithTarget;
const prepArgs = [resolve(repoRoot, 'scripts', 'prepare-mic-sidecar.mjs'), mode, ...tauriArgsWithTarget];
let nativeEnv = buildNativeEnv(target);
const prepEnv = {
  ...nativeEnv,
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

nativeEnv = buildNativeEnv(target);

const tauriEnv = {
  ...nativeEnv,
};

const managedWg = findManagedTool(process.platform === 'win32' ? 'wg' : 'wg', target, 'NOLAND_WG_BIN');
const managedWgQuick = process.platform === 'win32'
  ? undefined
  : findManagedTool('wg-quick', target, 'NOLAND_WG_QUICK_BIN');
const managedWireguardExe = process.platform === 'win32'
  ? findManagedTool('wireguard', target, 'NOLAND_WIREGUARD_EXE_BIN')
  : undefined;
const managedGotatun = findManagedTool('gotatun', target, 'NOLAND_GOTATUN_BIN');
if (managedWg) {
  tauriEnv.NOLAND_WG_BIN = managedWg;
}
if (managedWgQuick) {
  tauriEnv.NOLAND_WG_QUICK_BIN = managedWgQuick;
}
if (managedWireguardExe) {
  tauriEnv.NOLAND_WIREGUARD_EXE_BIN = managedWireguardExe;
}
if (managedGotatun) {
  tauriEnv.NOLAND_GOTATUN_BIN = managedGotatun;
}

if (mode === 'dev') {
  const sidecarTargetDir = join(repoRoot, 'src-tauri', '.native-deps', 'mic-sidecar-target');
  const profileDir = 'debug';
  const executableName = process.platform === 'win32' ? 'noland-mic-sender.exe' : 'noland-mic-sender';
  tauriEnv.NOLAND_MIC_SENDER_BIN = target
    ? join(sidecarTargetDir, target, profileDir, executableName)
    : join(sidecarTargetDir, profileDir, executableName);

  if (process.platform === 'darwin' && target?.endsWith('apple-darwin')) {
    stageMacosDevRuntimeDeps(target);
  }
}

const tauriInvocation = resolveTauriCliInvocation();
console.log(`Launching Tauri CLI: ${tauriInvocation.command} ${[...tauriInvocation.args, mode, ...tauriCliArgs].join(' ')}`);
const tauri = spawnSync(tauriInvocation.command, [...tauriInvocation.args, mode, ...tauriCliArgs], {
  cwd: repoRoot,
  stdio: 'inherit',
  env: tauriEnv,
});
if (tauri.error) {
  console.error(`Failed to launch Tauri CLI via ${tauriInvocation.command}: ${tauri.error.message}`);
  process.exit(1);
}
if (tauri.status !== 0) {
  console.error(`Tauri CLI exited with status ${tauri.status ?? 'unknown'}${tauri.signal ? ` (signal ${tauri.signal})` : ''}`);
  process.exit(tauri.status ?? 1);
}

if (process.platform === 'darwin' && mode === 'build') {
  const targetTriple = target ?? 'aarch64-apple-darwin';
  let fix = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'fix-macos-bundle-deps.mjs'), targetTriple], {
    cwd: repoRoot,
    stdio: 'inherit',
    env: nativeEnv,
  });
  if (fix.status !== 0) {
    process.exit(fix.status ?? 1);
  }

  if (!bundleHasRequiredSdl3(targetTriple)) {
    console.warn('First macOS bundle dependency fix did not leave the required SDL3 companion in the finished app bundle; retrying fix step once.');
    fix = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'fix-macos-bundle-deps.mjs'), targetTriple], {
      cwd: repoRoot,
      stdio: 'inherit',
      env: nativeEnv,
    });
    if (fix.status !== 0) {
      process.exit(fix.status ?? 1);
    }
  }

  if (!bundleHasRequiredSdl3(targetTriple)) {
    console.error('macOS bundle verification failed: the finished app bundle still references SDL3 without bundling libSDL3.dylib.');
    process.exit(1);
  }
}

process.exit(0);

function ensureCargoFeature(args, feature) {
  const cloned = [...args];

  for (let i = 0; i < cloned.length; i += 1) {
    if (cloned[i] === '--features' && cloned[i + 1]) {
      const features = new Set(cloned[i + 1].split(',').map((value) => value.trim()).filter(Boolean));
      features.add(feature);
      cloned[i + 1] = [...features].join(',');
      return cloned;
    }

    if (cloned[i].startsWith('--features=')) {
      const existing = cloned[i].slice('--features='.length);
      const features = new Set(existing.split(',').map((value) => value.trim()).filter(Boolean));
      features.add(feature);
      cloned[i] = `--features=${[...features].join(',')}`;
      return cloned;
    }
  }

  cloned.push('--features', feature);
  return cloned;
}

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

function buildNativeEnv(targetTriple) {
  const env = {
    ...process.env,
  };

  if (!targetTriple) {
    return env;
  }

  env.NOLAND_NATIVE_DEPS_PREFIX = join(repoRoot, 'src-tauri', '.native-deps', targetTriple);
  env.OPENSSL_ROOT_DIR = env.NOLAND_NATIVE_DEPS_PREFIX;
  env.OPENSSL_DIR = env.NOLAND_NATIVE_DEPS_PREFIX;

  if (targetTriple.endsWith('apple-darwin')) {
    env.NOLAND_GSTREAMER_FRAMEWORK = join(repoRoot, 'src-tauri', 'bundled', 'macos', 'GStreamer.framework');
    env.PKG_CONFIG_PATH = [
      ...resolveMacPkgConfigRoots(),
      join(env.NOLAND_NATIVE_DEPS_PREFIX, 'lib', 'pkgconfig'),
      join(env.NOLAND_NATIVE_DEPS_PREFIX, 'share', 'pkgconfig'),
      process.env.PKG_CONFIG_PATH,
    ].filter(Boolean).join(':');
    return env;
  }

  if (targetTriple.includes('linux')) {
    env.PKG_CONFIG_PATH = [
      join(env.NOLAND_NATIVE_DEPS_PREFIX, 'lib', 'pkgconfig'),
      join(env.NOLAND_NATIVE_DEPS_PREFIX, 'share', 'pkgconfig'),
      process.env.PKG_CONFIG_PATH,
    ].filter(Boolean).join(':');

    const gstreamerRoot = resolveLinuxGstreamerRoot(targetTriple);
    if (gstreamerRoot) {
      env.NOLAND_GSTREAMER_ROOT = gstreamerRoot;
      env.PKG_CONFIG_PATH = [
        join(gstreamerRoot, 'lib', 'pkgconfig'),
        join(gstreamerRoot, 'lib64', 'pkgconfig'),
        env.PKG_CONFIG_PATH,
      ].filter(Boolean).join(':');
      env.LD_LIBRARY_PATH = [
        join(gstreamerRoot, 'lib'),
        join(gstreamerRoot, 'lib64'),
        process.env.LD_LIBRARY_PATH,
      ].filter(Boolean).join(':');
    }
    return env;
  }

  if (targetTriple.includes('windows')) {
    const gstreamerRoot = resolveWindowsGstreamerRoot(targetTriple);
    if (gstreamerRoot) {
      env.NOLAND_GSTREAMER_ROOT = gstreamerRoot;
      if (targetTriple.includes('aarch64')) {
        env.GSTREAMER_1_0_ROOT_MSVC_ARM64 = gstreamerRoot;
      } else {
        env.GSTREAMER_1_0_ROOT_MSVC_X86_64 = gstreamerRoot;
      }
      env.PKG_CONFIG_PATH = [
        join(gstreamerRoot, 'lib', 'pkgconfig'),
        join(env.NOLAND_NATIVE_DEPS_PREFIX, 'lib', 'pkgconfig'),
        process.env.PKG_CONFIG_PATH,
      ].filter(Boolean).join(';');
      env.PATH = [
        join(gstreamerRoot, 'bin'),
        process.env.PATH,
      ].filter(Boolean).join(';');
    }
  }

  return env;
}

function stageMacosDevRuntimeDeps(targetTriple) {
  const prefix = join(repoRoot, 'src-tauri', '.native-deps', targetTriple, 'lib');
  const targetRoot = join(repoRoot, 'src-tauri', 'target', targetTriple);
  const debugDir = join(targetRoot, 'debug');
  const frameworksDir = join(targetRoot, 'Frameworks');

  mkdirSync(debugDir, { recursive: true });
  mkdirSync(frameworksDir, { recursive: true });

  for (const dylib of ['libopus.0.dylib', 'libSDL2-2.0.0.dylib']) {
    const source = join(prefix, dylib);
    if (!existsSync(source)) {
      continue;
    }
    stageMacosRuntimeFile(source, join(debugDir, dylib));
    stageMacosRuntimeFile(source, join(frameworksDir, dylib));
  }
}

function stageMacosRuntimeFile(source, destination) {
  copyFileSync(source, destination);
  chmodSync(destination, 0o755);
}

function resolveMacPkgConfigRoots() {
  const cacheRoot = join(repoRoot, 'src-tauri', '.native-deps', 'cache', `gstreamer-${gstreamerVersion}-macos-universal`, 'devel-expanded');
  const frameworkCandidates = [
    process.env.NOLAND_GSTREAMER_FRAMEWORK?.trim(),
    join(repoRoot, 'src-tauri', 'bundled', 'macos', 'GStreamer.framework'),
    '/Library/Frameworks/GStreamer.framework',
  ].filter(Boolean);

  const roots = [
    join(cacheRoot, `base-system-1.0-devel-${gstreamerVersion}-universal.pkg`, 'Payload', 'lib', 'pkgconfig'),
    join(cacheRoot, `gstreamer-1.0-core-devel-${gstreamerVersion}-universal.pkg`, 'Payload', 'lib', 'pkgconfig'),
  ];

  for (const framework of frameworkCandidates) {
    roots.push(
      join(framework, 'Versions', 'Current', 'lib', 'pkgconfig'),
      join(framework, 'Versions', '1.0', 'lib', 'pkgconfig'),
      join(framework, 'lib', 'pkgconfig'),
      join(framework, 'Libraries', 'pkgconfig'),
    );
  }

  return [...new Set(roots.filter((candidate) => existsSync(candidate)))];
}

function resolveLinuxGstreamerRoot(targetTriple) {
  const explicit = process.env.NOLAND_GSTREAMER_ROOT?.trim();
  if (explicit && (existsSync(join(explicit, 'lib')) || existsSync(join(explicit, 'lib64')))) {
    return explicit;
  }

  const candidate = join(repoRoot, 'src-tauri', '.native-deps', targetTriple, 'gstreamer');
  return existsSync(join(candidate, 'lib')) || existsSync(join(candidate, 'lib64')) ? candidate : null;
}

function resolveWindowsGstreamerRoot(targetTriple) {
  const explicit = process.env.NOLAND_GSTREAMER_ROOT?.trim();
  if (explicit && existsSync(join(explicit, 'bin'))) {
    return explicit;
  }

  const archDir = targetTriple.includes('x86_64') ? 'msvc_x86_64' : targetTriple.includes('aarch64') ? 'msvc_arm64' : null;
  if (!archDir) {
    return null;
  }

  const candidate = join(repoRoot, 'src-tauri', '.native-deps', targetTriple, 'gstreamer', '1.0', archDir);
  return existsSync(join(candidate, 'bin')) ? candidate : null;
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

function findManagedTool(toolStem, targetTriple, envVarName) {
  const envOverride = process.env[envVarName]?.trim();
  if (envOverride && existsSync(envOverride)) {
    return envOverride;
  }

  const extension = process.platform === 'win32' ? '.exe' : '';
  const candidates = [
    join(repoRoot, 'src-tauri', 'binaries', `${toolStem}-${targetTriple}${extension}`),
    join(repoRoot, 'src-tauri', 'binaries', `${toolStem}${extension}`),
  ];

  return candidates.find((candidate) => existsSync(candidate));
}

function resolveTauriCliInvocation() {
  const npmExecPath = process.env.npm_execpath?.trim();
  if (npmExecPath && existsSync(npmExecPath)) {
    return {
      command: process.execPath,
      args: [npmExecPath, 'exec', '--yes', '--', 'tauri'],
    };
  }

  try {
    const packageJsonPath = require.resolve('@tauri-apps/cli/package.json');
    const packageJson = JSON.parse(readFileSync(packageJsonPath, 'utf8'));
    const binEntry = typeof packageJson.bin === 'string'
      ? packageJson.bin
      : packageJson.bin?.tauri;

    if (!binEntry) {
      throw new Error(`@tauri-apps/cli package at ${packageJsonPath} does not declare a tauri bin entry`);
    }

    return {
      command: process.execPath,
      args: [resolve(dirname(packageJsonPath), binEntry)],
    };
  } catch (error) {
    console.error(`Unable to resolve a local Tauri CLI entrypoint. Did npm install complete successfully? ${error instanceof Error ? error.message : String(error)}`);
    process.exit(1);
  }
}

function bundleHasRequiredSdl3(targetTriple) {
  const productName = 'Noland Connect';
  const candidates = [
    join(repoRoot, 'src-tauri', 'target', 'release', 'bundle', 'macos', `${productName}.app`),
    join(repoRoot, 'src-tauri', 'target', targetTriple, 'release', 'bundle', 'macos', `${productName}.app`),
  ].filter((appPath, index, all) => all.indexOf(appPath) === index && existsSync(appPath));

  if (candidates.length === 0) {
    return false;
  }

  return candidates.every((appPath) => {
    const frameworksDir = join(appPath, 'Contents', 'Frameworks');
    const sdl2Compat = join(frameworksDir, 'libSDL2-2.0.0.dylib');
    if (!existsSync(sdl2Compat)) {
      return true;
    }

    const deps = spawnSync('otool', ['-L', sdl2Compat], {
      cwd: repoRoot,
      encoding: 'utf8',
    });
    if (deps.status !== 0) {
      return false;
    }

    if (!deps.stdout.includes('libSDL3')) {
      return true;
    }

    return existsSync(join(frameworksDir, 'libSDL3.dylib'));
  });
}
