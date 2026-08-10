#!/usr/bin/env node
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const srcTauriDir = join(repoRoot, 'src-tauri');
const sidecarCrateDir = join(repoRoot, 'mic-sidecar');
const netHelperCrateDir = join(srcTauriDir, 'crates', 'noland-net-helper');
const mode = process.argv[2] ?? 'build';
const passthroughArgs = process.argv.slice(3);
const target = readTarget(passthroughArgs) ?? defaultHostTarget();
const release = mode === 'build';
const executableName = process.platform === 'win32' ? 'noland-mic-sender.exe' : 'noland-mic-sender';
const netHelperExecutableName = isWindowsTarget(target)
  ? 'noland-net-helper.exe'
  : 'noland-net-helper';
const gstreamerVersion = process.env.NOLAND_GSTREAMER_VERSION?.trim()
  || process.env.GSTREAMER_VERSION?.trim()
  || '1.24.13';

const cargoArgs = ['build', '--manifest-path', join(sidecarCrateDir, 'Cargo.toml')];
if (release) cargoArgs.push('--release');
if (target) cargoArgs.push('--target', target);

const sidecarTargetDir = resolve(srcTauriDir, '.native-deps', 'mic-sidecar-target');
const netHelperTargetDir = resolve(srcTauriDir, '.native-deps', 'net-helper-target');
let nativeEnv = buildNativeEnv(target);
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

  nativeEnv = buildNativeEnv(target);
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

const netHelperCargoArgs = [
  'build',
  '--manifest-path', join(netHelperCrateDir, 'Cargo.toml'),
];
if (release) netHelperCargoArgs.push('--release');
if (target) netHelperCargoArgs.push('--target', target);
const netHelperCargo = spawnSync('cargo', netHelperCargoArgs, {
  cwd: repoRoot,
  stdio: 'inherit',
  env: {
    ...nativeEnv,
    CARGO_TARGET_DIR: netHelperTargetDir,
  },
});
if (netHelperCargo.status !== 0) {
  process.exit(netHelperCargo.status ?? 1);
}

const profileDir = release ? 'release' : 'debug';
const builtBinary = target
  ? join(sidecarTargetDir, target, profileDir, executableName)
  : join(sidecarTargetDir, profileDir, executableName);
const packagingTarget = release ? (target ?? defaultHostTarget()) : undefined;
const builtNetHelper = target
  ? join(netHelperTargetDir, target, profileDir, netHelperExecutableName)
  : join(netHelperTargetDir, profileDir, netHelperExecutableName);

if (!existsSync(builtBinary)) {
  console.error(`Expected mic sidecar binary was not produced: ${builtBinary}`);
  process.exit(1);
}
if (!existsSync(builtNetHelper)) {
  console.error(`Expected Noland GotaTun helper binary was not produced: ${builtNetHelper}`);
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

  const helperStagedName = isWindowsTarget(managedTarget)
    ? `noland-net-helper-${managedTarget}.exe`
    : `noland-net-helper-${managedTarget}`;
  const helperStagedPath = join(binariesDir, helperStagedName);
  copyFileSync(builtNetHelper, helperStagedPath);
  if (!isWindowsTarget(managedTarget)) {
    chmodSync(helperStagedPath, 0o755);
  }
  console.log(`Staged embedded GotaTun helper for packaging: ${helperStagedPath}`);

  if (isWindowsTarget(managedTarget)) {
    stageWindowsWintun(managedTarget, binariesDir);
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

  if (isWindowsTarget(packagingTarget) && windowsTargetNeedsGstreamer(packagingTarget)) {
    stageWindowsGstreamerRuntime(packagingTarget, binariesDir);
  } else if (packagingTarget.includes('linux')) {
    stageLinuxGstreamerRuntime(packagingTarget, binariesDir);
  } else if (isWindowsTarget(packagingTarget)) {
    console.log(`Skipping bundled Windows GStreamer runtime for ${packagingTarget}; microphone passthrough falls back to an unsupported stub on this target.`);
  }
} else {
  console.log(`Prepared mic sidecar for local ${mode}: ${builtBinary}`);
}

console.log(`Mic sidecar target dir: ${sidecarTargetDir}`);

function managedToolSpecs(targetTriple) {
  if (isWindowsTarget(targetTriple)) {
    return [
      { lookupName: 'ssh.exe', stagedStem: 'ssh', envVarName: 'NOLAND_SSH_BIN' },
      { lookupName: 'scp.exe', stagedStem: 'scp', envVarName: 'NOLAND_SCP_BIN' },
      { lookupName: 'ssh-keygen.exe', stagedStem: 'ssh-keygen', envVarName: 'NOLAND_SSH_KEYGEN_BIN' },
    ];
  }

  return [
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


  if (resolve(sourcePath) !== resolve(stagedBinary)) {
    copyFileSync(sourcePath, stagedBinary);
  }
  if (!isWindowsTarget(targetTriple)) {
    chmodSync(stagedBinary, 0o755);
  }
  console.log(`Staged ${tool.lookupName} sidecar for packaging: ${stagedBinary}`);
}

function stageWindowsWintun(targetTriple, binariesDir) {
  const stagedPath = join(binariesDir, `wintun-${targetTriple}.dll`);
  const stagedLicense = join(binariesDir, 'wintun-LICENSE.txt');
  const source = process.env.NOLAND_WINTUN_DLL?.trim();
  if (source && existsSync(source)) {
    const licenseSource = resolve(dirname(source), '..', '..', 'LICENSE.txt');
    if (!existsSync(licenseSource)) {
      console.error(`Wintun license was not found next to the staged DLL: ${licenseSource}`);
      process.exit(1);
    }
    copyFileSync(source, stagedPath);
    copyFileSync(licenseSource, stagedLicense);
    console.log(`Staged Wintun adapter library: ${stagedPath}`);
    return;
  }
  if (existsSync(stagedPath) && existsSync(stagedLicense)) {
    return;
  }
  console.error('Required bundled Wintun adapter library/license was not found. Set NOLAND_WINTUN_DLL or run the CI managed-tool staging step.');
  process.exit(1);
}

function windowsTargetNeedsGstreamer(targetTriple) {
  return !targetTriple.includes('aarch64');
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
  const frameworkCandidates = [
    process.env.NOLAND_GSTREAMER_FRAMEWORK?.trim(),
    join(srcTauriDir, 'bundled', 'macos', 'GStreamer.framework'),
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
