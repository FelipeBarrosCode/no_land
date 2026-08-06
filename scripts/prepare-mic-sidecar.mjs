#!/usr/bin/env node
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const srcTauriDir = join(repoRoot, 'src-tauri');
const sidecarCrateDir = join(repoRoot, 'mic-sidecar');
const mode = process.argv[2] ?? 'build';
const passthroughArgs = process.argv.slice(3);
const target = readTarget(passthroughArgs) ?? defaultHostTarget();
const release = mode === 'build';
const executableName = process.platform === 'win32' ? 'noland-mic-sender.exe' : 'noland-mic-sender';
const gstreamerVersion = process.env.NOLAND_GSTREAMER_VERSION?.trim() || '1.24.13';

const cargoArgs = ['build', '--manifest-path', join(sidecarCrateDir, 'Cargo.toml')];
if (release) cargoArgs.push('--release');
if (target) cargoArgs.push('--target', target);

const sidecarTargetDir = resolve(srcTauriDir, '.native-deps', 'mic-sidecar-target');
const nativeEnv = buildNativeEnv(target);
const managedTarget = target ?? defaultHostTarget();

if (target) {
  const bootstrap = spawnSync(process.execPath, [resolve(repoRoot, 'scripts', 'bootstrap-native-deps.mjs'), '--target', target], {
    cwd: repoRoot,
    stdio: 'inherit',
    env: nativeEnv,
  });
  if (bootstrap.status !== 0) {
    process.exit(bootstrap.status ?? 1);
  }
}

const cargo = spawnSync('cargo', cargoArgs, {
  cwd: repoRoot,
  stdio: 'inherit',
  env: {
    ...nativeEnv,
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
    env: nativeEnv,
  });
  if (fix.status !== 0) {
    process.exit(fix.status ?? 1);
  }
}

const binariesDir = join(srcTauriDir, 'binaries');
mkdirSync(binariesDir, { recursive: true });

if (managedTarget) {
  for (const tool of managedToolSpecs(managedTarget)) {
    stageBundledTool(tool, managedTarget, binariesDir);
  }
}

if (packagingTarget) {
  const stagedName = process.platform === 'win32'
    ? `noland-mic-sender-${packagingTarget}.exe`
    : `noland-mic-sender-${packagingTarget}`;
  const stagedBinary = join(binariesDir, stagedName);
  copyFileSync(builtBinary, stagedBinary);
  if (process.platform !== 'win32') {
    chmodSync(stagedBinary, 0o755);
  }
  console.log(`Staged mic sidecar for packaging: ${stagedBinary}`);

  if (isWindowsTarget(packagingTarget)) {
    stageWindowsGstreamerRuntime(packagingTarget, binariesDir);
  } else if (packagingTarget.includes('linux')) {
    stageLinuxGstreamerRuntime(packagingTarget, binariesDir);
  }
} else {
  console.log(`Prepared mic sidecar for local ${mode}: ${builtBinary}`);
}

console.log(`Mic sidecar target dir: ${sidecarTargetDir}`);

function managedToolSpecs(targetTriple) {
  if (isWindowsTarget(targetTriple)) {
    return [
      { lookupName: 'gotatun.exe', stagedStem: 'gotatun', envVarName: 'NOLAND_GOTATUN_BIN' },
      { lookupName: 'wg.exe', stagedStem: 'wg', envVarName: 'NOLAND_WG_BIN' },
      { lookupName: 'wireguard.exe', stagedStem: 'wireguard', envVarName: 'NOLAND_WIREGUARD_EXE_BIN' },
      { lookupName: 'ssh.exe', stagedStem: 'ssh', envVarName: 'NOLAND_SSH_BIN' },
      { lookupName: 'scp.exe', stagedStem: 'scp', envVarName: 'NOLAND_SCP_BIN' },
      { lookupName: 'ssh-keygen.exe', stagedStem: 'ssh-keygen', envVarName: 'NOLAND_SSH_KEYGEN_BIN' },
    ];
  }

  return [
    { lookupName: 'gotatun', stagedStem: 'gotatun', envVarName: 'NOLAND_GOTATUN_BIN' },
    { lookupName: 'wg', stagedStem: 'wg', envVarName: 'NOLAND_WG_BIN' },
    { lookupName: 'wg-quick', stagedStem: 'wg-quick', envVarName: 'NOLAND_WG_QUICK_BIN' },
    { lookupName: 'ssh', stagedStem: 'ssh', envVarName: 'NOLAND_SSH_BIN' },
    { lookupName: 'scp', stagedStem: 'scp', envVarName: 'NOLAND_SCP_BIN' },
    { lookupName: 'ssh-keygen', stagedStem: 'ssh-keygen', envVarName: 'NOLAND_SSH_KEYGEN_BIN' },
  ];
}

function stageBundledTool(tool, targetTriple, binariesDir) {
  const stagedBinary = join(
    binariesDir,
    `${tool.stagedStem}-${targetTriple}${isWindowsTarget(targetTriple) ? '.exe' : ''}`,
  );

  if (targetTriple.endsWith('apple-darwin') && ['ssh', 'scp', 'ssh-keygen'].includes(tool.lookupName)) {
    writeFileSyncSafe(stagedBinary, `#!/bin/sh\nexec /usr/bin/${tool.lookupName} "$@"\n`);
    chmodSync(stagedBinary, 0o755);
    console.log(`Staged macOS ${tool.lookupName} wrapper for packaging: ${stagedBinary}`);
    return;
  }

  const sourcePath = resolveToolPath(tool, targetTriple, binariesDir, stagedBinary);
  if (!sourcePath) {
    console.error(`Required bundled tool '${tool.lookupName}' was not found. Set ${tool.envVarName} or pre-stage a project-managed copy under src-tauri/binaries before building.`);
    process.exit(1);
  }

  if (targetTriple.endsWith('apple-darwin') && tool.lookupName === 'wg-quick') {
    const bashSource = resolveManagedBashPath(targetTriple);
    const bashDest = join(binariesDir, `wg-bash-${targetTriple}`);
    const realScript = join(binariesDir, `wg-quick-real-${targetTriple}`);
    const wrapper = stagedBinary;
    const sourceForReal = resolve(sourcePath) === resolve(wrapper) ? (existsSync(realScript) ? realScript : wrapper) : sourcePath;

    if (!bashSource) {
      console.error(`Project-managed Bash sidecar was not found for ${targetTriple}. Run bootstrap-native-deps first.`);
      process.exit(1);
    }

    if (resolve(sourceForReal) === resolve(wrapper) && !existsSync(realScript)) {
      copyFileSync(wrapper, realScript);
    } else if (resolve(sourceForReal) !== resolve(realScript)) {
      copyFileSync(sourceForReal, realScript);
    }
    patchMacosWgQuickScript(realScript);
    copyFileSync(bashSource, bashDest);
    chmodSync(realScript, 0o755);
    chmodSync(bashDest, 0o755);
    writeFileSyncSafe(wrapper, `#!/bin/sh\nSELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\nexport WG="$SELF_DIR/wg-${targetTriple}"\nexec "$SELF_DIR/${basename(bashDest)}" "$SELF_DIR/${basename(realScript)}" "$@"\n`);
    chmodSync(wrapper, 0o755);
    console.log(`Staged macOS wg-quick wrapper for packaging: ${wrapper}`);
    return;
  }

  if (resolve(sourcePath) !== resolve(stagedBinary)) {
    copyFileSync(sourcePath, stagedBinary);
  }
  if (!isWindowsTarget(targetTriple)) {
    chmodSync(stagedBinary, 0o755);
  }
  console.log(`Staged ${tool.lookupName} sidecar for packaging: ${stagedBinary}`);
}

function stageWindowsGstreamerRuntime(targetTriple, binariesDir) {
  const root = resolveWindowsGstreamerRoot(targetTriple);
  if (!root) {
    console.error(`Project-managed Windows GStreamer root was not found for ${targetTriple}. Run bootstrap-native-deps first.`);
    process.exit(1);
  }

  const destinationRoot = join(binariesDir, 'gstreamer', targetTriple);
  rmSync(destinationRoot, { recursive: true, force: true });
  mkdirSync(destinationRoot, { recursive: true });

  copyDirIfExists(join(root, 'bin'), join(destinationRoot, 'bin'));
  copyDirIfExists(join(root, 'lib', 'gstreamer-1.0'), join(destinationRoot, 'lib', 'gstreamer-1.0'));
  copyDirIfExists(join(root, 'libexec', 'gstreamer-1.0'), join(destinationRoot, 'libexec', 'gstreamer-1.0'));
  copyDirIfExists(join(root, 'lib', 'girepository-1.0'), join(destinationRoot, 'lib', 'girepository-1.0'));

  console.log(`Staged Windows GStreamer runtime for packaging: ${destinationRoot}`);
}

function stageLinuxGstreamerRuntime(targetTriple, binariesDir) {
  const root = resolveLinuxGstreamerRoot(targetTriple);
  if (!root) {
    return;
  }

  const destinationRoot = join(binariesDir, 'gstreamer', targetTriple);
  rmSync(destinationRoot, { recursive: true, force: true });
  mkdirSync(destinationRoot, { recursive: true });

  copyDirIfExists(join(root, 'lib'), join(destinationRoot, 'lib'));
  copyDirIfExists(join(root, 'lib64'), join(destinationRoot, 'lib64'));
  copyDirIfExists(join(root, 'libexec'), join(destinationRoot, 'libexec'));

  console.log(`Staged Linux GStreamer runtime for packaging: ${destinationRoot}`);
}

function patchMacosWgQuickScript(path) {
  if (!existsSync(path)) {
    return;
  }
  let content = readFileSync(path, 'utf8');
  if (!content.includes('WG_CMD="${WG:-wg}"')) {
    content = content.replace(
      'PROGRAM="${0##*/}"\nARGS=( "$@" )\n',
      'PROGRAM="${0##*/}"\nARGS=( "$@" )\nWG_CMD="${WG:-wg}"\n',
    );
  }

  const replacements = [
    ['wg show interfaces', '"$WG_CMD" show interfaces'],
    ['done < <(wg show "$REAL_INTERFACE" endpoints)', 'done < <("$WG_CMD" show "$REAL_INTERFACE" endpoints)'],
    ['cmd wg addconf "$REAL_INTERFACE" <(echo "$WG_CONFIG")', 'cmd "$WG_CMD" addconf "$REAL_INTERFACE" <(echo "$WG_CONFIG")'],
    ['current_config="$(cmd wg showconf "$REAL_INTERFACE")"', 'current_config="$(cmd "$WG_CMD" showconf "$REAL_INTERFACE")"'],
    ['done < <(wg show "$REAL_INTERFACE" allowed-ips)', 'done < <("$WG_CMD" show "$REAL_INTERFACE" allowed-ips)'],
    ['if ! get_real_interface || [[ " $(wg show interfaces) " != *" $REAL_INTERFACE "* ]]; then', 'if ! get_real_interface || [[ " $("$WG_CMD" show interfaces) " != *" $REAL_INTERFACE "* ]]; then'],
  ];

  for (const [from, to] of replacements) {
    content = content.replaceAll(from, to);
  }

  writeFileSync(path, content, 'utf8');
}

function writeFileSyncSafe(path, content) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content, 'utf8');
}

function copyDirIfExists(source, destination) {
  if (!existsSync(source)) {
    return;
  }
  cpSync(source, destination, { recursive: true, force: true, dereference: true });
}

function resolveLinuxGstreamerRoot(targetTriple) {
  const explicit = process.env.NOLAND_GSTREAMER_ROOT?.trim();
  if (explicit && (existsSync(join(explicit, 'lib')) || existsSync(join(explicit, 'lib64')))) {
    return explicit;
  }

  const candidate = join(srcTauriDir, '.native-deps', targetTriple, 'gstreamer');
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

  const candidate = join(srcTauriDir, '.native-deps', targetTriple, 'gstreamer', '1.0', archDir);
  return existsSync(join(candidate, 'bin')) ? candidate : null;
}

function resolveManagedBashPath(targetTriple) {
  const envOverride = process.env.NOLAND_BASH_BIN?.trim();
  if (envOverride && existsSync(envOverride)) {
    return envOverride;
  }

  const candidate = join(srcTauriDir, '.native-deps', targetTriple, 'bin', 'bash');
  return existsSync(candidate) ? candidate : null;
}

function resolveToolPath(tool, targetTriple, binariesDir, stagedBinary) {
  const envOverride = process.env[tool.envVarName]?.trim();
  if (envOverride && existsSync(envOverride)) {
    return envOverride;
  }
  if (existsSync(stagedBinary)) {
    return stagedBinary;
  }

  const extension = isWindowsTarget(targetTriple) ? '.exe' : '';
  const projectCandidates = [
    join(binariesDir, `${tool.stagedStem}-${targetTriple}${extension}`),
    join(srcTauriDir, 'binaries', `${tool.stagedStem}-${targetTriple}${extension}`),
  ];

  if (targetTriple === defaultHostTarget()) {
    projectCandidates.push(
      join(srcTauriDir, 'binaries', `${tool.lookupName}`),
      join(srcTauriDir, 'binaries', `${tool.stagedStem}${extension}`),
    );
  }

  return projectCandidates.find((candidate) => existsSync(candidate)) ?? null;
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

function isWindowsTarget(targetTriple) {
  return typeof targetTriple === 'string' && targetTriple.includes('windows');
}

function buildNativeEnv(targetTriple) {
  const env = {
    ...process.env,
  };

  if (!targetTriple) {
    return env;
  }

  env.NOLAND_NATIVE_DEPS_PREFIX = join(srcTauriDir, '.native-deps', targetTriple);
  env.OPENSSL_ROOT_DIR = env.NOLAND_NATIVE_DEPS_PREFIX;
  env.OPENSSL_DIR = env.NOLAND_NATIVE_DEPS_PREFIX;

  if (targetTriple.endsWith('apple-darwin')) {
    env.NOLAND_GSTREAMER_FRAMEWORK = join(srcTauriDir, 'bundled', 'macos', 'GStreamer.framework');
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
    }
    return env;
  }

  if (isWindowsTarget(targetTriple)) {
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

function resolveMacPkgConfigRoots() {
  const cacheRoot = join(srcTauriDir, '.native-deps', 'cache', `gstreamer-${gstreamerVersion}-macos-universal`, 'devel-expanded');
  return [
    join(cacheRoot, `base-system-1.0-devel-${gstreamerVersion}-universal.pkg`, 'Payload', 'lib', 'pkgconfig'),
    join(cacheRoot, `gstreamer-1.0-core-devel-${gstreamerVersion}-universal.pkg`, 'Payload', 'lib', 'pkgconfig'),
  ].filter((candidate) => existsSync(candidate));
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
