#!/usr/bin/env node
import { chmodSync, copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const gstreamerVersion = process.env.NOLAND_GSTREAMER_VERSION?.trim()
  || process.env.GSTREAMER_VERSION?.trim()
  || '1.24.13';
const [mode = 'build', ...tauriArgs] = process.argv.slice(2);

const requestedTarget = readTarget(tauriArgs);
const target = requestedTarget ?? defaultHostTarget();
const tauriArgsWithTarget = requestedTarget || !target ? tauriArgs : [...tauriArgs, '--target', target];
const windowsTargetConfig = resolveWindowsTargetConfig(target, tauriArgsWithTarget);
const tauriArgsPrepared = [...tauriArgsWithTarget, ...windowsTargetConfig];
const tauriCliArgs = tauriArgsPrepared;
const prepArgs = [resolve(repoRoot, 'scripts', 'prepare-mic-sidecar.mjs'), mode, ...tauriArgsPrepared];
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

if (process.platform === 'darwin' && mode === 'build') {
  delete tauriEnv.APPLE_ID;
  delete tauriEnv.APPLE_PASSWORD;
  delete tauriEnv.APPLE_TEAM_ID;
  delete tauriEnv.APPLE_API_ISSUER;
  delete tauriEnv.APPLE_API_KEY;
  console.log('[tauri-with-mic-sidecar] Deferring macOS notarization until after bundle fix/signing completes');
}

const managedNetHelper = findManagedTool(
  'noland-net-helper',
  target,
  'NOLAND_NET_HELPER_BIN',
);
if (managedNetHelper) {
  tauriEnv.NOLAND_NET_HELPER_BIN = managedNetHelper;
}
if (target?.includes('windows')) {
  const stagedWintun = join(repoRoot, 'src-tauri', 'binaries', `wintun-${target}.dll`);
  const wintun = process.env.NOLAND_WINTUN_DLL?.trim() || (existsSync(stagedWintun) ? stagedWintun : undefined);
  if (wintun) {
    tauriEnv.NOLAND_WINTUN_DLL = wintun;
  }
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

const tauriCliScript = resolve(repoRoot, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
const tauriCommand = process.platform === 'win32' && existsSync(tauriCliScript)
  ? process.execPath
  : 'npx';
const tauriCommandArgs = process.platform === 'win32' && existsSync(tauriCliScript)
  ? [tauriCliScript, mode, ...tauriCliArgs]
  : ['tauri', mode, ...tauriCliArgs];
const maxTauriAttempts = process.platform === 'win32' && mode === 'build' ? 3 : 1;
let tauri;
for (let attempt = 1; attempt <= maxTauriAttempts; attempt += 1) {
  console.log(`[tauri-with-mic-sidecar] Running Tauri ${mode} for target ${target} (attempt ${attempt}/${maxTauriAttempts})`);
  tauri = spawnSync(tauriCommand, tauriCommandArgs, {
    cwd: repoRoot,
    stdio: 'inherit',
    env: tauriEnv,
  });
  if (!tauri.error && tauri.status === 0) {
    break;
  }
  if (tauri.error) {
    console.error(`Failed to launch Tauri CLI via ${tauriCommand}: ${tauri.error.message}`);
  }
  if (attempt < maxTauriAttempts) {
    const delayMs = attempt * 15_000;
    console.warn(`[tauri-with-mic-sidecar] Windows bundling failed; retrying in ${delayMs / 1_000}s. Cargo output is cached, so the application will not be rebuilt from scratch.`);
    sleepSync(delayMs);
  }
}
if (!tauri || tauri.error || tauri.status !== 0) {
  process.exit(tauri?.status ?? 1);
}

if (process.platform === 'darwin' && mode === 'build') {
  const targetTriple = target ?? 'aarch64-apple-darwin';
  console.log(`[tauri-with-mic-sidecar] Starting macOS bundle dependency fix for ${targetTriple}`);
  let fix = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'fix-macos-bundle-deps.mjs'), targetTriple], {
    cwd: repoRoot,
    stdio: 'inherit',
    env: nativeEnv,
  });
  if (fix.status !== 0) {
    process.exit(fix.status ?? 1);
  }

  console.log(`[tauri-with-mic-sidecar] First macOS bundle dependency fix finished for ${targetTriple}`);
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
    console.log(`[tauri-with-mic-sidecar] Second macOS bundle dependency fix finished for ${targetTriple}`);
  }

  if (!bundleHasRequiredSdl3(targetTriple)) {
    console.error('macOS bundle verification failed: the finished app bundle still references SDL3 without bundling libSDL3.dylib.');
    process.exit(1);
  }

  const notarizationConfigured = Boolean(
    process.env.APPLE_ID && process.env.APPLE_PASSWORD && process.env.APPLE_TEAM_ID,
  );
  if (process.env.NOLAND_REQUIRE_SIGNED_RELEASE === '1' && !notarizationConfigured) {
    console.error('Refusing to produce a macOS release artifact without APPLE_ID, APPLE_PASSWORD, and APPLE_TEAM_ID notarization credentials.');
    process.exit(1);
  }

  if (notarizationConfigured) {
    console.log(`[tauri-with-mic-sidecar] Starting custom macOS notarization for ${targetTriple}`);
    const notarize = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'notarize-macos-bundle.mjs'), targetTriple], {
      cwd: repoRoot,
      stdio: 'inherit',
      env: nativeEnv,
    });
    if (notarize.status !== 0) {
      process.exit(notarize.status ?? 1);
    }
    console.log(`[tauri-with-mic-sidecar] Custom macOS notarization finished for ${targetTriple}`);
  } else {
    console.log('[tauri-with-mic-sidecar] Skipping custom macOS notarization because APPLE_ID/APPLE_PASSWORD/APPLE_TEAM_ID are not all configured');
  }
}

process.exit(0);


function sleepSync(milliseconds) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds);
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

function hasConfigPath(args, configPath) {
  for (let i = 0; i < args.length; i += 1) {
    if (args[i] === '--config' && args[i + 1]) {
      const values = args[i + 1].split(',').map((value) => value.trim()).filter(Boolean);
      if (values.includes(configPath)) {
        return true;
      }
    }
    if (args[i].startsWith('--config=')) {
      const values = args[i].slice('--config='.length).split(',').map((value) => value.trim()).filter(Boolean);
      if (values.includes(configPath)) {
        return true;
      }
    }
  }
  return false;
}

function resolveWindowsTargetConfig(targetTriple, args) {
  if (!targetTriple?.includes('windows')) {
    return [];
  }

  const configPath = targetTriple === 'aarch64-pc-windows-msvc'
    ? 'src-tauri/tauri.windows.arm64.conf.json'
    : targetTriple === 'x86_64-pc-windows-msvc'
      ? 'src-tauri/tauri.windows.x64.conf.json'
      : null;

  if (!configPath || hasConfigPath(args, configPath)) {
    return [];
  }

  return ['--config', configPath];
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
    env.MACOSX_DEPLOYMENT_TARGET = env.MACOSX_DEPLOYMENT_TARGET?.trim() || '12.0';
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
