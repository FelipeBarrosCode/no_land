#!/usr/bin/env node
import {
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const srcTauriDir = join(repoRoot, 'src-tauri');
const nativeDepsRoot = join(srcTauriDir, '.native-deps');
const bundledMacosDir = join(srcTauriDir, 'bundled', 'macos');
const bundledGstreamerFramework = join(bundledMacosDir, 'GStreamer.framework');
const gstreamerVersion = process.env.NOLAND_GSTREAMER_VERSION?.trim()
  || process.env.GSTREAMER_VERSION?.trim()
  || '1.24.13';
const args = process.argv.slice(2);
const target = readTarget(args)
  || process.env.NOLAND_NATIVE_TARGET?.trim()
  || process.env.NOLAND_MIC_SENDER_TARGET?.trim()
  || process.env.TAURI_ENV_TARGET_TRIPLE?.trim()
  || defaultHostTarget();

if (!target) {
  console.error('Unable to determine native dependency target triple. Pass --target <triple>.');
  process.exit(1);
}

if (isMacTarget(target)) {
  bootstrapMacTarget(target);
  process.exit(0);
}

if (isLinuxTarget(target)) {
  bootstrapLinuxTarget(target);
  process.exit(0);
}

if (isWindowsTarget(target)) {
  bootstrapWindowsTarget(target);
  process.exit(0);
}

console.log(`No native dependency bootstrap is defined for ${target}`);
process.exit(0);

function bootstrapMacTarget(targetTriple) {
  const prefix = nativePrefix(targetTriple);
  mkdirSync(prefix, { recursive: true });

  const gstreamerPackages = ensureExpandedMacGstreamerPackages();
  ensureMacGstreamerFramework();
  ensureOpenSsl(prefix, targetTriple);
  ensureOpus(prefix, targetTriple);
  ensureSdl2(prefix, targetTriple);
  ensureBash(prefix, targetTriple);

  console.log(`Native dependency bootstrap ready for ${targetTriple}`);
  console.log(`  prefix: ${prefix}`);
  console.log(`  gstreamer framework: ${bundledGstreamerFramework}`);
  console.log(`  gstreamer pkg-config roots: ${gstreamerPackages.pkgConfigRoots.join(', ')}`);
}

function bootstrapLinuxTarget(targetTriple) {
  const prefix = nativePrefix(targetTriple);
  mkdirSync(prefix, { recursive: true });

  ensureOpus(prefix, targetTriple);
  const gstreamerRoot = ensureLinuxGstreamerRoot(prefix, targetTriple);

  if (gstreamerRoot) {
    console.log(`Using staged Linux GStreamer root: ${gstreamerRoot}`);
  } else {
    console.log('Linux bootstrap prepared project-local Opus. GStreamer/GTK/WebKit/Pulse remain system-provided unless NOLAND_GSTREAMER_ROOT is supplied.');
  }

  console.log(`Native dependency bootstrap ready for ${targetTriple}`);
  console.log(`  prefix: ${prefix}`);
}

function bootstrapWindowsTarget(targetTriple) {
  if (process.platform !== 'win32') {
    throw new Error(`Windows native bootstrap for ${targetTriple} must run on a Windows host.`);
  }

  const prefix = nativePrefix(targetTriple);
  mkdirSync(prefix, { recursive: true });

  const gstreamerRoot = ensureWindowsGstreamerRoot(prefix, targetTriple);

  console.log(`Native dependency bootstrap ready for ${targetTriple}`);
  console.log(`  prefix: ${prefix}`);
  console.log(`  gstreamer: ${gstreamerRoot}`);
}

function ensureMacGstreamerFramework() {
  if (hasGstreamerFramework(bundledGstreamerFramework)) {
    console.log(`Using staged project GStreamer framework: ${bundledGstreamerFramework}`);
    return;
  }

  const explicitSource = process.env.NOLAND_GSTREAMER_FRAMEWORK?.trim();
  if (explicitSource && hasGstreamerFramework(explicitSource)) {
    stageFrameworkCopy(explicitSource, bundledGstreamerFramework);
    console.log(`Staged GStreamer framework from NOLAND_GSTREAMER_FRAMEWORK=${explicitSource}`);
    return;
  }

  const systemFramework = '/Library/Frameworks/GStreamer.framework';
  if (hasGstreamerFramework(systemFramework)) {
    stageFrameworkCopy(systemFramework, bundledGstreamerFramework);
    console.log(`Staged GStreamer framework from ${systemFramework}`);
    return;
  }

  const { runtimePkg } = ensureExpandedMacGstreamerPackages();
  const installedFramework = installMacGstreamerRuntimeFramework(runtimePkg, systemFramework);
  if (installedFramework) {
    stageFrameworkCopy(installedFramework, bundledGstreamerFramework);
    console.log(`Staged GStreamer framework from installed runtime at ${installedFramework}`);
    return;
  }

  const extractedFramework = ensureExtractedMacGstreamerFramework();
  if (extractedFramework) {
    stageFrameworkCopy(extractedFramework, bundledGstreamerFramework);
    console.log(`Staged GStreamer framework from extracted runtime package for ${gstreamerVersion}`);
    return;
  }

  const discoveredInstalled = findDirectoriesNamed('/Library/Frameworks', 'GStreamer.framework');
  const discoveredExtracted = findDirectoriesNamed(macGstreamerCacheRoot(), 'GStreamer.framework');
  throw new Error(
    [
      'Could not stage a macOS GStreamer framework.',
      'Set NOLAND_GSTREAMER_FRAMEWORK to a valid GStreamer.framework path or let bootstrap install or extract the official framework packages.',
      `Installed candidates: ${discoveredInstalled.length > 0 ? discoveredInstalled.join(', ') : 'none'}`,
      `Extracted candidates: ${discoveredExtracted.length > 0 ? discoveredExtracted.join(', ') : 'none'}`,
    ].join(' '),
  );
}

function installMacGstreamerRuntimeFramework(runtimePkg, systemFramework) {
  if (!runtimePkg || !existsSync(runtimePkg)) {
    return null;
  }

  const install = run('sudo', ['installer', '-pkg', runtimePkg, '-target', '/'], {
    allowFailure: true,
    captureOutput: true,
  });

  if (install.status === 0 && hasGstreamerFramework(systemFramework)) {
    return systemFramework;
  }

  const stderr = [install.stderr, install.stdout].filter(Boolean).join('\n').trim();
  console.warn(`macOS GStreamer runtime installer did not produce a usable framework: ${stderr || `status ${install.status}`}`);
  if (existsSync(systemFramework)) {
    console.warn(`System framework path exists but did not validate: ${systemFramework}`);
  }
  return hasGstreamerFramework(systemFramework) ? systemFramework : null;
}

function ensureExtractedMacGstreamerFramework() {
  const cacheRoot = macGstreamerCacheRoot();
  const extractedRoot = join(cacheRoot, 'root');
  const extractedFramework = join(extractedRoot, 'Library', 'Frameworks', 'GStreamer.framework');
  if (hasGstreamerFramework(extractedFramework)) {
    return extractedFramework;
  }

  const { runtimePkg } = ensureExpandedMacGstreamerPackages();
  rmSync(extractedRoot, { recursive: true, force: true });
  mkdirSync(extractedRoot, { recursive: true });
  extractFlatPkg(runtimePkg, extractedRoot, join(cacheRoot, 'runtime-expanded'));

  if (hasGstreamerFramework(extractedFramework)) {
    return extractedFramework;
  }

  const discoveredFramework = findDirectoriesNamed(extractedRoot, 'GStreamer.framework')
    .find((candidate) => hasGstreamerFramework(candidate));
  if (discoveredFramework) {
    return discoveredFramework;
  }

  const runtimeExpandedFramework = findDirectoriesNamed(join(cacheRoot, 'runtime-expanded'), 'GStreamer.framework')
    .find((candidate) => hasGstreamerFramework(candidate));
  if (runtimeExpandedFramework) {
    return runtimeExpandedFramework;
  }

  return null;
}

function ensureExpandedMacGstreamerPackages() {
  const cacheRoot = macGstreamerCacheRoot();
  mkdirSync(cacheRoot, { recursive: true });

  const runtimePkg = join(cacheRoot, `gstreamer-1.0-${gstreamerVersion}-universal.pkg`);
  const develPkg = join(cacheRoot, `gstreamer-1.0-devel-${gstreamerVersion}-universal.pkg`);
  const runtimeUrl = `https://gstreamer.freedesktop.org/data/pkg/osx/${gstreamerVersion}/gstreamer-1.0-${gstreamerVersion}-universal.pkg`;
  const develUrl = `https://gstreamer.freedesktop.org/data/pkg/osx/${gstreamerVersion}/gstreamer-1.0-devel-${gstreamerVersion}-universal.pkg`;
  const runtimeExpandedDir = join(cacheRoot, 'runtime-expanded');
  const develExpandedDir = join(cacheRoot, 'devel-expanded');
  const glibComponentDir = join(
    develExpandedDir,
    `base-system-1.0-devel-${gstreamerVersion}-universal.pkg`,
  );
  const gstreamerComponentDir = join(
    develExpandedDir,
    `gstreamer-1.0-core-devel-${gstreamerVersion}-universal.pkg`,
  );
  const glibPkgConfigDir = join(glibComponentDir, 'Payload', 'lib', 'pkgconfig');
  const gstreamerPkgConfigDir = join(gstreamerComponentDir, 'Payload', 'lib', 'pkgconfig');

  downloadFile(runtimeUrl, runtimePkg, { expectedKind: 'xar-pkg' });
  downloadFile(develUrl, develPkg, { expectedKind: 'xar-pkg' });

  if (!existsSync(join(runtimeExpandedDir, 'Distribution'))) {
    expandMacPkg(runtimePkg, runtimeExpandedDir);
  }
  if (!existsSync(join(glibComponentDir, 'Payload')) || !existsSync(join(gstreamerComponentDir, 'Payload'))) {
    expandMacMetaPkg(develPkg, develExpandedDir, [
      `base-system-1.0-devel-${gstreamerVersion}-universal.pkg`,
      `gstreamer-1.0-core-devel-${gstreamerVersion}-universal.pkg`,
    ]);
  }

  expandMacPayloadInPlace(glibComponentDir);
  expandMacPayloadInPlace(gstreamerComponentDir);

  if (!existsSync(join(glibPkgConfigDir, 'glib-2.0.pc'))) {
    throw new Error(`Project-managed macOS GLib pkg-config metadata was not found at ${glibPkgConfigDir}`);
  }
  if (!existsSync(join(gstreamerPkgConfigDir, 'gstreamer-1.0.pc'))) {
    throw new Error(`Project-managed macOS GStreamer pkg-config metadata was not found at ${gstreamerPkgConfigDir}`);
  }

  return {
    cacheRoot,
    runtimePkg,
    develPkg,
    runtimeExpandedDir,
    develExpandedDir,
    pkgConfigRoots: [glibPkgConfigDir, gstreamerPkgConfigDir],
  };
}

function macGstreamerCacheRoot() {
  return join(nativeDepsRoot, 'cache', `gstreamer-${gstreamerVersion}-macos-universal`);
}

function expandMacPkg(pkgPath, expandedDir) {
  rmSync(expandedDir, { recursive: true, force: true });
  mkdirSync(dirname(expandedDir), { recursive: true });
  run('pkgutil', ['--expand-full', pkgPath, expandedDir]);
}

function expandMacMetaPkg(pkgPath, expandedDir, requiredComponents = []) {
  rmSync(expandedDir, { recursive: true, force: true });
  mkdirSync(dirname(expandedDir), { recursive: true });
  const result = run('pkgutil', ['--expand', pkgPath, expandedDir], {
    allowFailure: true,
    captureOutput: true,
  });

  if (result.status !== 0) {
    const stderr = [result.stderr, result.stdout].filter(Boolean).join('\n').trim();
    console.warn(`pkgutil --expand reported a non-zero exit while unpacking ${pkgPath}: ${stderr || `status ${result.status}`}`);
  }

  for (const componentName of requiredComponents) {
    if (!existsSync(join(expandedDir, componentName, 'Payload'))) {
      throw new Error(`Failed to expand required macOS package component ${componentName} from ${pkgPath}`);
    }
  }
}

function expandMacPayloadInPlace(componentPkgDir) {
  const payloadPath = join(componentPkgDir, 'Payload');
  if (!existsSync(payloadPath)) {
    throw new Error(`Missing Payload in ${componentPkgDir}`);
  }
  if (statSync(payloadPath).isDirectory()) {
    return;
  }

  const archivedPayloadPath = join(componentPkgDir, 'Payload.archive');
  rmSync(archivedPayloadPath, { force: true });
  renameSync(payloadPath, archivedPayloadPath);
  mkdirSync(payloadPath, { recursive: true });
  extractPayloadArchive(archivedPayloadPath, payloadPath);
}

function ensureLinuxGstreamerRoot(prefix, targetTriple) {
  const expectedRoot = linuxGstreamerRoot(prefix, targetTriple);
  if (hasLinuxGstreamerRoot(expectedRoot)) {
    return expectedRoot;
  }

  const explicitRoot = process.env.NOLAND_GSTREAMER_ROOT?.trim();
  if (explicitRoot && hasLinuxGstreamerRoot(explicitRoot)) {
    stageDirectory(explicitRoot, expectedRoot);
    console.log(`Staged Linux GStreamer root from NOLAND_GSTREAMER_ROOT=${explicitRoot}`);
    return expectedRoot;
  }

  return null;
}

function ensureWindowsGstreamerRoot(prefix, targetTriple) {
  const expectedRoot = windowsGstreamerRoot(prefix, targetTriple);
  if (hasWindowsGstreamerRoot(expectedRoot)) {
    console.log(`Using staged Windows GStreamer root: ${expectedRoot}`);
    return expectedRoot;
  }

  const explicitRoot = process.env.NOLAND_GSTREAMER_ROOT?.trim();
  if (explicitRoot && hasWindowsGstreamerRoot(explicitRoot)) {
    stageDirectory(explicitRoot, expectedRoot);
    console.log(`Staged Windows GStreamer root from NOLAND_GSTREAMER_ROOT=${explicitRoot}`);
    return expectedRoot;
  }

  const cacheRoot = join(nativeDepsRoot, 'cache', `gstreamer-${gstreamerVersion}-${windowsGstreamerArch(targetTriple)}`);
  const extractedRoot = join(cacheRoot, 'root');
  const downloadArch = windowsGstreamerDownloadArch(targetTriple);
  const runtimeMsi = join(cacheRoot, `gstreamer-1.0-msvc-${downloadArch}-${gstreamerVersion}.msi`);
  const develMsi = join(cacheRoot, `gstreamer-1.0-devel-msvc-${downloadArch}-${gstreamerVersion}.msi`);
  const runtimeUrl = `https://gstreamer.freedesktop.org/data/pkg/windows/${gstreamerVersion}/msvc/gstreamer-1.0-msvc-${downloadArch}-${gstreamerVersion}.msi`;
  const develUrl = `https://gstreamer.freedesktop.org/data/pkg/windows/${gstreamerVersion}/msvc/gstreamer-1.0-devel-msvc-${downloadArch}-${gstreamerVersion}.msi`;

  mkdirSync(cacheRoot, { recursive: true });
  downloadFile(runtimeUrl, runtimeMsi, { expectedKind: 'msi' });
  downloadFile(develUrl, develMsi, { expectedKind: 'msi' });

  rmSync(extractedRoot, { recursive: true, force: true });
  mkdirSync(extractedRoot, { recursive: true });

  extractWindowsMsi(runtimeMsi, extractedRoot);
  extractWindowsMsi(develMsi, extractedRoot);

  const discoveredRoot = findWindowsGstreamerRoot(extractedRoot);
  if (!discoveredRoot) {
    throw new Error(`Failed to discover extracted Windows GStreamer root under ${extractedRoot}`);
  }

  stageDirectory(discoveredRoot, expectedRoot);
  return expectedRoot;
}

function ensureOpenSsl(prefix, targetTriple) {
  const cryptoLib = join(prefix, 'lib', 'libcrypto.3.dylib');
  const sslLib = join(prefix, 'lib', 'libssl.3.dylib');
  if (existsSync(cryptoLib) && existsSync(sslLib)) {
    console.log(`Using staged OpenSSL for ${targetTriple}`);
    return;
  }

  const tarball = join(nativeDepsRoot, 'src', 'openssl-3.3.2.tar.gz');
  const tarballUrl = 'https://github.com/openssl/openssl/releases/download/openssl-3.3.2/openssl-3.3.2.tar.gz';
  downloadFile(tarballUrl, tarball, { expectedKind: 'tar.gz' });
  const extractRoot = join(nativeDepsRoot, `build-openssl-src-${targetTriple}`);
  const buildDir = extractTarballSource(tarball, extractRoot, 'openssl-3.3.2');

  const configureTarget = targetTriple.startsWith('aarch64-') ? 'darwin64-arm64-cc' : 'darwin64-x86_64-cc';
  const env = nativeBuildEnv(targetTriple);
  run('perl', [
    'Configure',
    configureTarget,
    'shared',
    'no-tests',
    'no-apps',
    `--prefix=${prefix}`,
    '--libdir=lib',
    `--openssldir=${join(prefix, 'ssl')}`,
  ], { cwd: buildDir, env });
  run('make', ['-j', cpuCount(), 'build_sw'], { cwd: buildDir, env });
  run('make', ['install_sw'], { cwd: buildDir, env });
}

function ensureOpus(prefix, targetTriple) {
  const libName = isWindowsTarget(targetTriple) ? 'opus.lib' : isMacTarget(targetTriple) ? 'libopus.dylib' : 'libopus.so';
  const libPath = join(prefix, 'lib', libName);
  const pkgConfig = join(prefix, 'lib', 'pkgconfig', 'opus.pc');
  if (existsSync(libPath) && (existsSync(pkgConfig) || isWindowsTarget(targetTriple))) {
    console.log(`Using staged Opus for ${targetTriple}`);
    return;
  }

  ensureExtractedSourceTarball(
    'https://downloads.xiph.org/releases/opus/opus-1.5.2.tar.gz',
    'opus-1.5.2.tar.gz',
    'opus-1.5.2',
  );
  const sourceDir = join(nativeDepsRoot, 'src', 'opus-1.5.2');
  const buildDir = join(nativeDepsRoot, `build-opus-${targetTriple}`);
  rmSync(buildDir, { recursive: true, force: true });
  mkdirSync(buildDir, { recursive: true });

  const cmakeArgs = [
    '-S', sourceDir,
    '-B', buildDir,
    '-DCMAKE_BUILD_TYPE=Release',
    `-DCMAKE_INSTALL_PREFIX=${prefix}`,
    '-DBUILD_SHARED_LIBS=ON',
    '-DOPUS_BUILD_PROGRAMS=OFF',
    '-DOPUS_BUILD_TESTING=OFF',
    '-DOPUS_STACK_PROTECTOR=OFF',
    '-DOPUS_DISABLE_INTRINSICS=OFF',
    ...cmakeTargetArgs(targetTriple),
  ];

  run('cmake', cmakeArgs, { env: nativeBuildEnv(targetTriple) });
  run('cmake', ['--build', buildDir, '--config', 'Release', '--parallel', cpuCount()], {
    env: nativeBuildEnv(targetTriple),
  });
  run('cmake', ['--install', buildDir, '--config', 'Release'], { env: nativeBuildEnv(targetTriple) });
}

function ensureSdl2(prefix, targetTriple) {
  const libPath = join(prefix, 'lib', 'libSDL2.dylib');
  const pkgConfig = join(prefix, 'lib', 'pkgconfig', 'sdl2.pc');
  if (existsSync(libPath) && existsSync(pkgConfig)) {
    console.log(`Using staged SDL2 for ${targetTriple}`);
    return;
  }

  ensureExtractedSourceTarball(
    'https://github.com/libsdl-org/SDL/releases/download/release-2.30.10/SDL2-2.30.10.tar.gz',
    'SDL2-2.30.10.tar.gz',
    'SDL2-2.30.10',
  );
  const sourceDir = join(nativeDepsRoot, 'src', 'SDL2-2.30.10');
  const buildDir = join(nativeDepsRoot, `build-sdl2-${targetTriple}`);
  rmSync(buildDir, { recursive: true, force: true });
  mkdirSync(buildDir, { recursive: true });

  run('cmake', [
    '-S', sourceDir,
    '-B', buildDir,
    '-DCMAKE_BUILD_TYPE=Release',
    `-DCMAKE_INSTALL_PREFIX=${prefix}`,
    '-DSDL_SHARED=ON',
    '-DSDL_STATIC=OFF',
    '-DSDL_TEST=OFF',
    '-DSDL2_DISABLE_INSTALL_DOCS=ON',
    ...cmakeTargetArgs(targetTriple),
  ], { env: nativeBuildEnv(targetTriple) });
  run('cmake', ['--build', buildDir, '--config', 'Release', '--parallel', cpuCount()], {
    env: nativeBuildEnv(targetTriple),
  });
  run('cmake', ['--install', buildDir, '--config', 'Release'], { env: nativeBuildEnv(targetTriple) });
}

function ensureBash(prefix, targetTriple) {
  const bashPath = join(prefix, 'bin', 'bash');
  if (existsSync(bashPath)) {
    console.log(`Using staged Bash for ${targetTriple}`);
    return;
  }

  const tarball = join(nativeDepsRoot, 'src', 'bash-5.2.tar.gz');
  const tarballUrl = 'https://ftp.gnu.org/gnu/bash/bash-5.2.tar.gz';
  downloadFile(tarballUrl, tarball, { expectedKind: 'tar.gz' });

  const extractRoot = join(nativeDepsRoot, `build-bash-src-${targetTriple}`);
  const buildDir = extractTarballSource(tarball, extractRoot, 'bash-5.2');
  const env = nativeBuildEnv(targetTriple);

  run('sh', [
    './configure',
    `--prefix=${prefix}`,
    '--disable-nls',
    '--without-bash-malloc',
    '--disable-readline',
    '--disable-history',
  ], { cwd: buildDir, env });
  run('make', ['-j', cpuCount()], { cwd: buildDir, env });
  run('make', ['install-bin', 'install-headers'], { cwd: buildDir, env });
}

function nativePrefix(targetTriple) {
  return join(nativeDepsRoot, targetTriple);
}

function nativeBuildEnv(targetTriple) {
  const env = { ...process.env };

  if (isMacTarget(targetTriple)) {
    const archFlag = targetTriple.startsWith('aarch64-') ? '-arch arm64' : '-arch x86_64';
    const deploymentTarget = process.env.MACOSX_DEPLOYMENT_TARGET?.trim();
    if (deploymentTarget) {
      env.MACOSX_DEPLOYMENT_TARGET = deploymentTarget;
    }
    env.CFLAGS = mergeFlags(env.CFLAGS, archFlag);
    env.CXXFLAGS = mergeFlags(env.CXXFLAGS, archFlag);
    env.LDFLAGS = mergeFlags(env.LDFLAGS, archFlag);
  }

  return env;
}

function cmakeTargetArgs(targetTriple) {
  const args = [];

  if (isMacTarget(targetTriple)) {
    if (targetTriple.startsWith('aarch64-')) {
      args.push('-DCMAKE_OSX_ARCHITECTURES=arm64');
    } else if (targetTriple.startsWith('x86_64-')) {
      args.push('-DCMAKE_OSX_ARCHITECTURES=x86_64');
    }
    if (process.env.MACOSX_DEPLOYMENT_TARGET?.trim()) {
      args.push(`-DCMAKE_OSX_DEPLOYMENT_TARGET=${process.env.MACOSX_DEPLOYMENT_TARGET.trim()}`);
    }
  }

  return args;
}

function linuxGstreamerRoot(prefix, targetTriple) {
  return join(prefix, 'gstreamer');
}

function windowsGstreamerRoot(prefix, targetTriple) {
  return join(prefix, 'gstreamer', '1.0', windowsGstreamerArch(targetTriple));
}

function windowsGstreamerArch(targetTriple) {
  if (targetTriple.includes('x86_64')) return 'msvc_x86_64';
  if (targetTriple.includes('aarch64')) return 'msvc_arm64';
  throw new Error(`Unsupported Windows GStreamer target: ${targetTriple}`);
}

function windowsGstreamerDownloadArch(targetTriple) {
  if (targetTriple.includes('x86_64')) return 'x86_64';
  if (targetTriple.includes('aarch64')) return 'arm64';
  throw new Error(`Unsupported Windows GStreamer download target: ${targetTriple}`);
}

function hasGstreamerFramework(path) {
  if (!path) return false;

  const versionRoots = [
    join(path, 'Versions', 'Current'),
    join(path, 'Versions', '1.0'),
    join(path, 'Versions', gstreamerVersion),
    path,
  ];

  return versionRoots.some((root) => (
    existsSync(join(root, 'lib', 'GStreamer'))
    || existsSync(join(root, 'lib', 'libgstreamer-1.0.dylib'))
    || existsSync(join(root, 'lib', 'libgstreamer-1.0.0.dylib'))
    || existsSync(join(root, 'Libraries', 'GStreamer'))
    || existsSync(join(root, 'Libraries', 'libgstreamer-1.0.dylib'))
    || existsSync(join(root, 'Libraries', 'libgstreamer-1.0.0.dylib'))
  ));
}

function hasLinuxGstreamerRoot(path) {
  if (!path) return false;
  return (
    existsSync(join(path, 'lib', 'libgstreamer-1.0.so'))
    || existsSync(join(path, 'lib', 'libgstreamer-1.0.so.0'))
    || existsSync(join(path, 'lib64', 'libgstreamer-1.0.so'))
    || existsSync(join(path, 'lib64', 'libgstreamer-1.0.so.0'))
  ) && (
    existsSync(join(path, 'lib', 'pkgconfig', 'gstreamer-1.0.pc'))
    || existsSync(join(path, 'lib64', 'pkgconfig', 'gstreamer-1.0.pc'))
  );
}

function hasWindowsGstreamerRoot(path) {
  if (!path) return false;
  return existsSync(join(path, 'bin', 'gstreamer-1.0-0.dll'))
    && existsSync(join(path, 'lib', 'pkgconfig', 'gstreamer-1.0.pc'));
}

function findWindowsGstreamerRoot(root) {
  if (!existsSync(root)) return null;
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    if (hasWindowsGstreamerRoot(current)) {
      return current;
    }
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        stack.push(join(current, entry.name));
      }
    }
  }
  return null;
}

function stageFrameworkCopy(source, destination) {
  rmSync(destination, { recursive: true, force: true });
  mkdirSync(dirname(destination), { recursive: true });
  cpSync(source, destination, {
    recursive: true,
    force: true,
    dereference: false,
    verbatimSymlinks: true,
  });
}

function stageDirectory(source, destination) {
  rmSync(destination, { recursive: true, force: true });
  mkdirSync(dirname(destination), { recursive: true });
  cpSync(source, destination, { recursive: true, force: true, dereference: true });
}

function extractFlatPkg(pkgPath, extractionRoot, expandedDir) {
  rmSync(expandedDir, { recursive: true, force: true });
  mkdirSync(dirname(expandedDir), { recursive: true });
  run('pkgutil', ['--expand-full', pkgPath, expandedDir]);

  const payloads = findFilesNamed(expandedDir, 'Payload');
  if (payloads.length > 0) {
    for (const payload of payloads) {
      extractPayloadArchive(payload, extractionRoot, pkgPath);
    }
    return;
  }

  // Newer flat packages may not produce Payload files — check if
  // pkgutil already laid out the files directly in expandedDir.
  // Preserve symlinks here because framework bundles commonly contain
  // relative symlinks that only resolve correctly once the entire tree
  // is copied into place.
  for (const entry of readdirSync(expandedDir, { withFileTypes: true })) {
    const source = join(expandedDir, entry.name);
    const destination = join(extractionRoot, entry.name);
    if (entry.isDirectory()) {
      cpSync(source, destination, {
        recursive: true,
        force: true,
        dereference: false,
        verbatimSymlinks: true,
      });
    } else if (entry.isFile()) {
      mkdirSync(dirname(destination), { recursive: true });
      copyFileSync(source, destination);
    }
  }
}

function extractPayloadArchive(payload, extractionRoot, packageLabel = payload) {
  const probe = run('file', ['-b', payload], { allowFailure: true, captureOutput: true });
  const description = probe.stdout.trim();
  if (/cpio archive/i.test(description)) {
    run('cpio', ['-idm', '--quiet'], {
      cwd: extractionRoot,
      inputPath: payload,
    });
    return;
  }
  if (/gzip compressed/i.test(description)) {
    runWithShell(`gzip -dc '${escapeForSingleQuotes(payload)}' | cpio -idm --quiet`, extractionRoot);
    return;
  }
  if (/xz compressed/i.test(description)) {
    runWithShell(`xz -dc '${escapeForSingleQuotes(payload)}' | cpio -idm --quiet`, extractionRoot);
    return;
  }
  if (/pbzx/i.test(description)) {
    throw new Error(`Unsupported pbzx payload in ${packageLabel}; manual GStreamer framework staging is required.`);
  }
  throw new Error(`Unsupported payload format for ${payload}: ${description || 'unknown'}`);
}

function extractWindowsMsi(msiPath, extractionRoot) {
  run('msiexec', [
    '/a',
    msiPath,
    '/qn',
    `TARGETDIR=${extractionRoot}`,
  ]);
}

function downloadFile(url, destination, options = {}) {
  const { expectedKind } = options;

  if (existsSync(destination) && validateDownloadedFile(destination, expectedKind)) {
    return;
  }

  if (existsSync(destination)) {
    rmSync(destination, { force: true });
  }

  mkdirSync(dirname(destination), { recursive: true });
  const tempDestination = `${destination}.partial`;
  rmSync(tempDestination, { force: true });
  run('curl', ['-fL', '--retry', '3', '--retry-all-errors', '-o', tempDestination, url]);

  if (!validateDownloadedFile(tempDestination, expectedKind)) {
    const preview = readFileSync(tempDestination)
      .subarray(0, 256)
      .toString('utf8')
      .replace(/\s+/g, ' ')
      .trim();
    rmSync(tempDestination, { force: true });
    throw new Error(`Downloaded file from ${url} to ${destination} but it was not a valid ${expectedKind || 'artifact'}. First bytes: ${preview || '<binary/empty>'}`);
  }

  renameSync(tempDestination, destination);
}

function validateDownloadedFile(path, expectedKind) {
  if (!existsSync(path)) {
    return false;
  }

  const stats = statSync(path, { throwIfNoEntry: false });
  if (!stats || !stats.isFile() || stats.size <= 0) {
    return false;
  }

  if (!expectedKind) {
    return true;
  }

  const header = readFileSync(path).subarray(0, 512);
  const headerUtf8 = header.toString('utf8').trimStart().toLowerCase();
  if (headerUtf8.startsWith('<!doctype html') || headerUtf8.startsWith('<html') || headerUtf8.startsWith('<?xml')) {
    return false;
  }

  if (expectedKind === 'tar.gz') {
    return header.length >= 2 && header[0] === 0x1f && header[1] === 0x8b;
  }

  if (expectedKind === 'msi') {
    const msiMagic = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
    return msiMagic.every((byte, index) => header[index] === byte);
  }

  if (expectedKind === 'xar-pkg') {
    return header.length >= 4
      && header[0] === 0x78
      && header[1] === 0x61
      && header[2] === 0x72
      && header[3] === 0x21;
  }

  return true;
}

function prepareBuildDir(sourceDir, buildDir) {
  rmSync(buildDir, { recursive: true, force: true });
  mkdirSync(dirname(buildDir), { recursive: true });
  cpSync(sourceDir, buildDir, { recursive: true, force: true, dereference: true });
}

function ensureExtractedSourceTarball(url, tarballName, extractedDirName) {
  const tarballPath = join(nativeDepsRoot, 'src', tarballName);
  const extractedPath = join(nativeDepsRoot, 'src', extractedDirName);
  if (existsSync(extractedPath)) {
    return extractedPath;
  }
  downloadFile(url, tarballPath, { expectedKind: 'tar.gz' });
  const extractRoot = join(nativeDepsRoot, 'src');
  run('tar', ['-xzf', tarballPath, '-C', extractRoot]);
  return extractedPath;
}

function extractTarballSource(tarballPath, extractRoot, extractedDirName) {
  rmSync(extractRoot, { recursive: true, force: true });
  mkdirSync(extractRoot, { recursive: true });
  run('tar', ['-xzf', tarballPath, '-C', extractRoot]);
  return join(extractRoot, extractedDirName);
}

function mergeFlags(existing, nextFlag) {
  return [existing?.trim(), nextFlag].filter(Boolean).join(' ');
}

function findFilesNamed(root, name) {
  const found = [];
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      if (entry.isDirectory()) {
        stack.push(full);
      } else if (entry.isFile() && entry.name === name) {
        found.push(full);
      }
    }
  }
  return found;
}

function findDirectoriesNamed(root, name) {
  if (!existsSync(root)) {
    return [];
  }

  const found = [];
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === name) {
          found.push(full);
        }
        stack.push(full);
      }
    }
  }
  return found;
}

function readTarget(argv) {
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--target' && argv[i + 1]) {
      return argv[i + 1];
    }
    if (argv[i].startsWith('--target=')) {
      return argv[i].slice('--target='.length);
    }
  }
  return undefined;
}

function defaultHostTarget() {
  if (process.platform === 'darwin') {
    if (process.arch === 'arm64') return 'aarch64-apple-darwin';
    if (process.arch === 'x64') return 'x86_64-apple-darwin';
  }
  if (process.platform === 'linux') {
    if (process.arch === 'x64') return 'x86_64-unknown-linux-gnu';
    if (process.arch === 'arm64') return 'aarch64-unknown-linux-gnu';
  }
  if (process.platform === 'win32') {
    if (process.arch === 'x64') return 'x86_64-pc-windows-msvc';
    if (process.arch === 'arm64') return 'aarch64-pc-windows-msvc';
  }
  return undefined;
}

function isMacTarget(targetTriple) {
  return typeof targetTriple === 'string' && targetTriple.endsWith('apple-darwin');
}

function isLinuxTarget(targetTriple) {
  return typeof targetTriple === 'string' && targetTriple.includes('linux');
}

function isWindowsTarget(targetTriple) {
  return typeof targetTriple === 'string' && targetTriple.includes('windows');
}

function cpuCount() {
  if (process.env.NOLAND_NATIVE_BUILD_JOBS?.trim()) {
    return process.env.NOLAND_NATIVE_BUILD_JOBS.trim();
  }
  const command = process.platform === 'darwin' ? 'sysctl' : process.platform === 'win32' ? 'powershell' : 'nproc';
  const args = process.platform === 'darwin'
    ? ['-n', 'hw.ncpu']
    : process.platform === 'win32'
      ? ['-NoProfile', '-Command', '[Environment]::ProcessorCount']
      : [];
  const detected = requireNumber(command, args);
  return String(Math.max(1, Math.min(8, detected || 4)));
}

function requireNumber(command, args) {
  const result = run(command, args, { allowFailure: true, captureOutput: true });
  if (result.status !== 0) return null;
  const value = Number.parseInt(result.stdout.trim(), 10);
  return Number.isFinite(value) ? value : null;
}

function run(command, args, options = {}) {
  const {
    cwd = repoRoot,
    env = process.env,
    allowFailure = false,
    captureOutput = false,
    inputPath,
  } = options;

  let input;
  if (inputPath) {
    const cat = spawnSync(process.platform === 'win32' ? 'cmd' : 'cat', process.platform === 'win32' ? ['/c', 'type', inputPath] : [inputPath], {
      cwd,
      env,
      stdio: ['ignore', 'pipe', 'inherit'],
      encoding: null,
      shell: process.platform === 'win32',
    });
    if (cat.status !== 0) {
      throw new Error(`Failed reading ${inputPath}`);
    }
    input = cat.stdout;
  }

  const result = spawnSync(command, args, {
    cwd,
    env,
    stdio: captureOutput ? ['pipe', 'pipe', 'pipe'] : (input ? ['pipe', 'inherit', 'inherit'] : 'inherit'),
    encoding: captureOutput ? 'utf8' : undefined,
    input,
    shell: false,
  });

  if (!allowFailure && result.status !== 0) {
    throw new Error(`Command failed: ${command} ${args.join(' ')}`);
  }

  return {
    status: result.status ?? 0,
    stdout: typeof result.stdout === 'string' ? result.stdout : '',
    stderr: typeof result.stderr === 'string' ? result.stderr : '',
  };
}

function runWithShell(command, cwd) {
  const shell = process.platform === 'win32' ? 'cmd' : 'sh';
  const shellArgs = process.platform === 'win32' ? ['/c', command] : ['-c', command];
  const result = spawnSync(shell, shellArgs, {
    cwd,
    env: process.env,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    throw new Error(`Command failed: ${command}`);
  }
}

function escapeForSingleQuotes(value) {
  return String(value).replace(/'/g, `"'"'`);
}
