#!/usr/bin/env node
import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
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

  console.log(`[bootstrap-native-deps] macOS bootstrap start for ${targetTriple}`);
  console.log('[bootstrap-native-deps] Ensuring expanded macOS GStreamer packages');
  const gstreamerPackages = ensureExpandedMacGstreamerPackages();
  console.log('[bootstrap-native-deps] Ensuring staged macOS GStreamer framework');
  ensureMacGstreamerFramework();
  console.log('[bootstrap-native-deps] Ensuring OpenSSL');
  ensureOpenSsl(prefix, targetTriple);
  console.log('[bootstrap-native-deps] Ensuring Opus');
  ensureOpus(prefix, targetTriple);
  console.log('[bootstrap-native-deps] Ensuring SDL2');
  ensureSdl2(prefix, targetTriple);
  console.log('[bootstrap-native-deps] Ensuring Bash');
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
  ensureSdl2(prefix, targetTriple);
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

  ensureOpus(prefix, targetTriple);
  ensureSdl2(prefix, targetTriple);
  const gstreamerRoot = windowsTargetNeedsGstreamer(targetTriple)
    ? ensureWindowsGstreamerRoot(prefix, targetTriple)
    : null;
  const libclangRoot = ensureWindowsLibclangRoot(targetTriple);
  const openSshRoot = ensureWindowsOpenSshRoot(targetTriple);

  console.log(`Native dependency bootstrap ready for ${targetTriple}`);
  console.log(`  prefix: ${prefix}`);
  if (gstreamerRoot) {
    console.log(`  gstreamer: ${gstreamerRoot}`);
  } else {
    console.log('  gstreamer: skipped (Windows ARM64 microphone sidecar uses a stub fallback because upstream GStreamer MSVC ARM64 packages are not published)');
  }
  console.log(`  libclang: ${libclangRoot}`);
  console.log(`  openssh: ${openSshRoot}`);
}

function ensureMacGstreamerFramework() {
  if (hasStructuredGstreamerFramework(bundledGstreamerFramework)) {
    console.log(`Using staged project GStreamer framework: ${bundledGstreamerFramework}`);
    return;
  }

  const explicitSource = process.env.NOLAND_GSTREAMER_FRAMEWORK?.trim();
  if (explicitSource && hasGstreamerFrameworkSource(explicitSource)) {
    stageFrameworkCopy(explicitSource, bundledGstreamerFramework);
    console.log(`Staged GStreamer framework from NOLAND_GSTREAMER_FRAMEWORK=${explicitSource}`);
    return;
  }

  const systemFramework = '/Library/Frameworks/GStreamer.framework';
  if (hasGstreamerFrameworkSource(systemFramework)) {
    stageFrameworkCopy(systemFramework, bundledGstreamerFramework);
    console.log(`Staged GStreamer framework from ${systemFramework}`);
    return;
  }

  const extractedFramework = ensureExtractedMacGstreamerFramework();
  if (extractedFramework) {
    stageFrameworkCopy(extractedFramework, bundledGstreamerFramework);
    console.log(`Staged GStreamer framework from extracted runtime package for ${gstreamerVersion}`);
    return;
  }

  const { runtimePkg } = ensureExpandedMacGstreamerPackages();
  const installedFramework = installMacGstreamerRuntimeFramework(runtimePkg, systemFramework);
  if (installedFramework) {
    stageFrameworkCopy(installedFramework, bundledGstreamerFramework);
    console.log(`Staged GStreamer framework from installed runtime at ${installedFramework}`);
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

  if (install.status === 0 && hasGstreamerFrameworkSource(systemFramework)) {
    return systemFramework;
  }

  const stderr = [install.stderr, install.stdout].filter(Boolean).join('\n').trim();
  console.warn(`macOS GStreamer runtime installer did not produce a usable framework: ${stderr || `status ${install.status}`}`);
  if (existsSync(systemFramework)) {
    console.warn(`System framework path exists but did not validate: ${systemFramework}`);
  }
  return hasGstreamerFrameworkSource(systemFramework) ? systemFramework : null;
}

function ensureExtractedMacGstreamerFramework() {
  const cacheRoot = macGstreamerCacheRoot();
  const extractedRoot = join(cacheRoot, 'root');
  const extractedFramework = join(extractedRoot, 'Library', 'Frameworks', 'GStreamer.framework');
  if (hasGstreamerFrameworkSource(extractedFramework)) {
    return extractedFramework;
  }
  if (hasFlattenedGstreamerRuntime(extractedRoot)) {
    return extractedRoot;
  }

  const { runtimePkg } = ensureExpandedMacGstreamerPackages();
  rmSync(extractedRoot, { recursive: true, force: true });
  mkdirSync(extractedRoot, { recursive: true });
  extractFlatPkg(runtimePkg, extractedRoot, join(cacheRoot, 'runtime-expanded'));

  if (hasGstreamerFrameworkSource(extractedFramework)) {
    return extractedFramework;
  }
  if (hasFlattenedGstreamerRuntime(extractedRoot)) {
    return extractedRoot;
  }

  const discoveredFramework = findDirectoriesNamed(extractedRoot, 'GStreamer.framework')
    .find((candidate) => hasGstreamerFrameworkSource(candidate));
  if (discoveredFramework) {
    return discoveredFramework;
  }

  const runtimeExpandedFramework = findDirectoriesNamed(join(cacheRoot, 'runtime-expanded'), 'GStreamer.framework')
    .find((candidate) => hasGstreamerFrameworkSource(candidate));
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
    sanitizeLinuxGstreamerRoot(expectedRoot);
    return expectedRoot;
  }

  const explicitRoot = process.env.NOLAND_GSTREAMER_ROOT?.trim();
  if (explicitRoot && hasLinuxGstreamerRoot(explicitRoot)) {
    stageDirectory(explicitRoot, expectedRoot);
    sanitizeLinuxGstreamerRoot(expectedRoot);
    console.log(`Staged Linux GStreamer root from NOLAND_GSTREAMER_ROOT=${explicitRoot}`);
    return expectedRoot;
  }

  const systemRoot = discoverLinuxSystemGstreamer();
  if (systemRoot) {
    stageLinuxSystemGstreamerRoot(systemRoot, expectedRoot);
    console.log(`Staged Linux GStreamer runtime from system packages under ${systemRoot.libDir}`);
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

function ensureWindowsLibclangRoot(targetTriple) {
  const explicit = process.env.LIBCLANG_PATH?.trim();
  if (explicit && existsSync(join(explicit, 'libclang.dll'))) {
    return explicit;
  }

  const version = '18.1.8';
  const cacheRoot = join(nativeDepsRoot, 'cache', `llvm-${version}-windows-x64`);
  const archivePath = join(cacheRoot, `clang+llvm-${version}-x86_64-pc-windows-msvc.tar.xz`);
  const extractedRoot = join(cacheRoot, 'extracted');
  if (existsSync(join(extractedRoot, 'bin', 'libclang.dll'))) {
    return join(extractedRoot, 'bin');
  }

  const directRoot = join(extractedRoot, `clang+llvm-${version}-x86_64-pc-windows-msvc`);
  if (existsSync(join(directRoot, 'bin', 'libclang.dll'))) {
    return join(directRoot, 'bin');
  }

  mkdirSync(cacheRoot, { recursive: true });
  downloadFile(
    `https://github.com/llvm/llvm-project/releases/download/llvmorg-${version}/clang%2Bllvm-${version}-x86_64-pc-windows-msvc.tar.xz`,
    archivePath,
    { expectedKind: 'tar.xz' },
  );
  rmSync(extractedRoot, { recursive: true, force: true });
  mkdirSync(extractedRoot, { recursive: true });
  extractTarXz(archivePath, extractedRoot);

  const extractedEntries = readdirSync(extractedRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => join(extractedRoot, entry.name));
  const llvmRoot = extractedEntries.find((candidate) => existsSync(join(candidate, 'bin', 'libclang.dll')));
  if (llvmRoot) {
    return join(llvmRoot, 'bin');
  }

  if (existsSync(join(extractedRoot, 'bin', 'libclang.dll'))) {
    return join(extractedRoot, 'bin');
  }

  throw new Error(`Downloaded LLVM archive did not contain libclang.dll for ${targetTriple}`);
}

function ensureWindowsOpenSshRoot(targetTriple) {
  const explicitRoot = process.env.NOLAND_OPENSSH_ROOT?.trim();
  if (explicitRoot && hasWindowsOpenSshRoot(explicitRoot)) {
    return explicitRoot;
  }

  const cacheRoot = join(nativeDepsRoot, 'cache', `openssh-${targetTriple}`);
  const stagedRoot = join(cacheRoot, 'root');
  if (hasWindowsOpenSshRoot(stagedRoot)) {
    return stagedRoot;
  }

  const systemRoot = process.env.SystemRoot?.trim() || 'C:\\Windows';
  const sourceRoot = join(systemRoot, 'System32', 'OpenSSH');
  if (!hasWindowsOpenSshRoot(sourceRoot)) {
    throw new Error(
      `Project-managed Windows OpenSSH sidecars are missing and no bootstrap source was found at ${sourceRoot}. Set NOLAND_OPENSSH_ROOT to a directory containing ssh.exe, scp.exe, and ssh-keygen.exe.`
    );
  }

  rmSync(stagedRoot, { recursive: true, force: true });
  mkdirSync(stagedRoot, { recursive: true });

  const requiredFiles = [
    'ssh.exe',
    'scp.exe',
    'ssh-keygen.exe',
    'sftp.exe',
    'ssh-agent.exe',
    'ssh-add.exe',
    'ssh-keyscan.exe',
    'ssh-pkcs11-helper.exe',
    'ssh-sk-helper.exe',
    'LICENSE.txt',
    'NOTICE.txt',
  ];
  for (const name of requiredFiles) {
    const source = join(sourceRoot, name);
    if (!existsSync(source)) {
      if (['LICENSE.txt', 'NOTICE.txt', 'ssh-pkcs11-helper.exe', 'ssh-sk-helper.exe'].includes(name)) {
        continue;
      }
      throw new Error(`Windows OpenSSH bootstrap source is missing required file ${source}`);
    }
    copyFileSync(source, join(stagedRoot, name));
  }

  return stagedRoot;
}

function hasWindowsOpenSshRoot(root) {
  return Boolean(root)
    && existsSync(join(root, 'ssh.exe'))
    && existsSync(join(root, 'scp.exe'))
    && existsSync(join(root, 'ssh-keygen.exe'));
}

function ensureOpenSsl(prefix, targetTriple) {
  const cryptoLib = join(prefix, 'lib', 'libcrypto.3.dylib');
  const sslLib = join(prefix, 'lib', 'libssl.3.dylib');
  if (existsSync(cryptoLib) && existsSync(sslLib)) {
    console.log(`Using staged OpenSSL for ${targetTriple}`);
    return;
  }

  const localPrefix = resolveMacLocalOpenSslPrefix();
  if (localPrefix) {
    stageMacOpenSslPrefix(localPrefix, prefix);
    if (existsSync(cryptoLib) && existsSync(sslLib)) {
      console.log(`Staged OpenSSL for ${targetTriple} from local prefix ${localPrefix}`);
      return;
    }
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
  const libName = isWindowsTarget(targetTriple)
    ? 'opus.lib'
    : isMacTarget(targetTriple)
      ? 'libopus.dylib'
      : 'libopus.a';
  const libPath = join(prefix, 'lib', libName);
  const pkgConfig = join(prefix, 'lib', 'pkgconfig', 'opus.pc');
  const staticStamp = join(prefix, 'lib', '.noland-opus-static');
  const expectedVariantExists = isMacTarget(targetTriple)
    ? existsSync(libPath)
    : existsSync(libPath) && existsSync(staticStamp);
  if (expectedVariantExists && (existsSync(pkgConfig) || isWindowsTarget(targetTriple))) {
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
    `-DBUILD_SHARED_LIBS=${isMacTarget(targetTriple) ? 'ON' : 'OFF'}`,
    `-DOPUS_BUILD_SHARED_LIBRARY=${isMacTarget(targetTriple) ? 'ON' : 'OFF'}`,
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
  if (!isMacTarget(targetTriple)) {
    writeFileSync(staticStamp, `${targetTriple}\n`, 'utf8');
  }
}

function ensureSdl2(prefix, targetTriple) {
  const libPath = isWindowsTarget(targetTriple)
    ? join(prefix, 'lib', 'SDL2-static.lib')
    : isMacTarget(targetTriple)
      ? join(prefix, 'lib', 'libSDL2.dylib')
      : join(prefix, 'lib', 'libSDL2.a');
  const pkgConfig = join(prefix, 'lib', 'pkgconfig', 'sdl2.pc');
  if (existsSync(libPath) && (existsSync(pkgConfig) || isWindowsTarget(targetTriple))) {
    console.log(`Using staged SDL2 for ${targetTriple}`);
    return;
  }

  console.log(`[bootstrap-native-deps] Building SDL2 from source for ${targetTriple}`);

  ensureExtractedSourceTarball(
    'https://github.com/libsdl-org/SDL/releases/download/release-2.30.10/SDL2-2.30.10.tar.gz',
    'SDL2-2.30.10.tar.gz',
    'SDL2-2.30.10',
  );
  const sourceDir = join(nativeDepsRoot, 'src', 'SDL2-2.30.10');
  const buildDir = join(nativeDepsRoot, `build-sdl2-${targetTriple}`);
  rmSync(buildDir, { recursive: true, force: true });
  mkdirSync(buildDir, { recursive: true });

  const shared = isMacTarget(targetTriple) ? 'ON' : 'OFF';
  const staticLibrary = isMacTarget(targetTriple) ? 'OFF' : 'ON';
  run('cmake', [
    '-S', sourceDir,
    '-B', buildDir,
    '-DCMAKE_BUILD_TYPE=Release',
    `-DCMAKE_INSTALL_PREFIX=${prefix}`,
    `-DSDL_SHARED=${shared}`,
    `-DSDL_STATIC=${staticLibrary}`,
    '-DSDL_STATIC_PIC=ON',
    '-DSDL_TEST=OFF',
    '-DSDL2_DISABLE_INSTALL_DOCS=ON',
    '-DSDL_X11_SHARED=ON',
    '-DSDL_WAYLAND_SHARED=ON',
    '-DSDL_ALSA_SHARED=ON',
    '-DSDL_PULSEAUDIO_SHARED=ON',
    '-DSDL_PIPEWIRE_SHARED=ON',
    '-DSDL_HIDAPI_LIBUSB_SHARED=ON',
    ...cmakeTargetArgs(targetTriple),
  ], { env: nativeBuildEnv(targetTriple) });
  console.log(`[bootstrap-native-deps] SDL2 configure complete for ${targetTriple}`);
  run('cmake', ['--build', buildDir, '--config', 'Release', '--parallel', cpuCount()], {
    env: nativeBuildEnv(targetTriple),
  });
  console.log(`[bootstrap-native-deps] SDL2 build complete for ${targetTriple}`);
  run('cmake', ['--install', buildDir, '--config', 'Release'], { env: nativeBuildEnv(targetTriple) });
  console.log(`[bootstrap-native-deps] SDL2 install complete for ${targetTriple}`);
}

function ensureBash(prefix, targetTriple) {
  const bashPath = join(prefix, 'bin', 'bash');
  if (existsSync(bashPath)) {
    console.log(`Using staged Bash for ${targetTriple}`);
    return;
  }

  console.log(`[bootstrap-native-deps] Building Bash from source for ${targetTriple}`);

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
  console.log(`[bootstrap-native-deps] Bash configure complete for ${targetTriple}`);
  run('make', ['-j', cpuCount()], { cwd: buildDir, env });
  console.log(`[bootstrap-native-deps] Bash build complete for ${targetTriple}`);
  run('make', ['install'], { cwd: buildDir, env });
  console.log(`[bootstrap-native-deps] Bash install complete for ${targetTriple}`);

  if (!existsSync(bashPath)) {
    throw new Error(`Bash install completed but ${bashPath} was not produced`);
  }
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

function discoverLinuxSystemGstreamer() {
  if (process.platform !== 'linux') {
    return null;
  }

  const libDir = pkgConfigVariable('gstreamer-1.0', 'libdir');
  if (!libDir || !existsSync(libDir)) {
    return null;
  }

  const pluginsDir = locateExistingPath([
    pkgConfigVariable('gstreamer-1.0', 'pluginsdir'),
    join(libDir, 'gstreamer-1.0'),
  ]);
  const pluginScannerDir = pkgConfigVariable('gstreamer-1.0', 'pluginscannerdir');
  const pluginScanner = pkgConfigVariable('gstreamer-1.0', 'pluginscanner');
  const libexecDir = pkgConfigVariable('gstreamer-1.0', 'libexecdir');
  const scannerPath = locateExistingPath([
    pluginScanner,
    pluginScannerDir ? join(pluginScannerDir, 'gst-plugin-scanner') : null,
    libexecDir ? join(libexecDir, 'gstreamer-1.0', 'gst-plugin-scanner') : null,
    join(libDir, 'gstreamer1.0', 'gstreamer-1.0', 'gst-plugin-scanner'),
    join(libDir, 'gstreamer-1.0', 'gst-plugin-scanner'),
    '/usr/libexec/gstreamer-1.0/gst-plugin-scanner',
    '/usr/lib/gstreamer-1.0/gst-plugin-scanner',
    ...findFilesNamed(libDir, 'gst-plugin-scanner'),
  ]);
  const pkgConfigPath = locateExistingPath([
    join(libDir, 'pkgconfig', 'gstreamer-1.0.pc'),
    '/usr/lib/pkgconfig/gstreamer-1.0.pc',
    '/usr/share/pkgconfig/gstreamer-1.0.pc',
  ]);

  if (!pluginsDir || !pkgConfigPath || !scannerPath) {
    return null;
  }

  return {
    libDir,
    pluginsDir,
    scannerPath,
    pkgConfigPath,
    typelibDir: locateExistingPath([
      join(libDir, 'girepository-1.0'),
      '/usr/lib/girepository-1.0',
      '/usr/lib64/girepository-1.0',
    ]),
  };
}

function stageLinuxSystemGstreamerRoot(systemRoot, destination) {
  rmSync(destination, { recursive: true, force: true });

  const libDest = join(destination, 'lib');
  const pluginDest = join(libDest, 'gstreamer-1.0');
  const libexecDest = join(destination, 'libexec', 'gstreamer-1.0');
  const pkgConfigDest = join(libDest, 'pkgconfig');
  const typelibDest = join(libDest, 'girepository-1.0');

  mkdirSync(libDest, { recursive: true });
  mkdirSync(pluginDest, { recursive: true });
  mkdirSync(libexecDest, { recursive: true });
  mkdirSync(pkgConfigDest, { recursive: true });

  const requiredPluginNames = new Set([
    'libgstcoreelements.so',
    'libgstapp.so',
    'libgstautodetect.so',
    'libgstplayback.so',
    'libgstvideoconvertscale.so',
    'libgstvideoconvert.so',
    'libgstvideoscale.so',
    'libgstvideoparsersbad.so',
    'libgstlibav.so',
    'libgstximagesink.so',
    'libgstxvimagesink.so',
    'libgstwaylandsink.so',
    'libgstopengl.so',
    'libgstva.so',
    'libgstvaapi.so',
    'libgstvideo4linux2.so',
    'libgstv4l2codecs.so',
    'libgstnvcodec.so',
    'libgstaudioconvert.so',
    'libgstaudioresample.so',
    'libgstaudiorate.so',
    'libgstpipewire.so',
    'libgstopus.so',
    'libgstrtp.so',
    'libgstrtpmanager.so',
    'libgstudp.so',
    'libgsttypefindfunctions.so',
    'libgstvolume.so',
  ]);

  const stagedFiles = [];
  stagedFiles.push(...copyDirectoryEntriesMatching(systemRoot.libDir, libDest, /^libgst.+\.so(?:\..+)?$/u));
  stagedFiles.push(...copyDirectoryEntriesMatching(systemRoot.libDir, libDest, /^libcrypto\.so(?:\..+)?$/u));
  stagedFiles.push(...copyDirectoryEntriesMatching(systemRoot.pluginsDir, pluginDest, (name) => requiredPluginNames.has(name)));

  if (!systemRoot.scannerPath || !existsSync(systemRoot.scannerPath)) {
    throw new Error('The system GStreamer installation does not provide gst-plugin-scanner');
  }
  const scannerDest = join(libexecDest, 'gst-plugin-scanner');
  copyFileWithMode(systemRoot.scannerPath, scannerDest);
  stagedFiles.push(scannerDest);

  if (systemRoot.pkgConfigPath && existsSync(systemRoot.pkgConfigPath)) {
    copyFileWithMode(systemRoot.pkgConfigPath, join(pkgConfigDest, 'gstreamer-1.0.pc'));
  }

  if (systemRoot.typelibDir && existsSync(systemRoot.typelibDir)) {
    stageDirectory(systemRoot.typelibDir, typelibDest);
  }

  stageLinuxDependencyClosure(stagedFiles, libDest);
  sanitizeLinuxGstreamerRoot(destination);
  patchLinuxRuntimeRpaths(destination);
}

function patchLinuxRuntimeRpaths(root) {
  const probe = run('patchelf', ['--version'], { allowFailure: true, captureOutput: true });
  if (probe.status !== 0) {
    throw new Error('patchelf is required to make the bundled Linux GStreamer runtime relocatable');
  }

  const libDir = join(root, 'lib');
  const pluginDir = join(libDir, 'gstreamer-1.0');
  const scanner = join(root, 'libexec', 'gstreamer-1.0', 'gst-plugin-scanner');
  const targets = [
    ...linuxElfFilesInDirectory(libDir, { recursive: false }).map((path) => [path, '$ORIGIN']),
    ...linuxElfFilesInDirectory(pluginDir, { recursive: true }).map((path) => [path, '$ORIGIN/..']),
    ...(existsSync(scanner) ? [[scanner, '$ORIGIN/../../lib']] : []),
  ];

  for (const [path, rpath] of targets) {
    run('patchelf', ['--force-rpath', '--set-rpath', rpath, path]);
  }
}

function linuxElfFilesInDirectory(root, { recursive }) {
  if (!existsSync(root)) return [];
  const files = [];
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) {
        if (recursive) stack.push(path);
        continue;
      }
      if (!entry.isFile() && !entry.isSymbolicLink()) continue;
      const kind = run('file', ['-b', path], { allowFailure: true, captureOutput: true });
      if (kind.status === 0 && kind.stdout.includes('ELF')) files.push(path);
    }
  }
  return files;
}

function stageLinuxDependencyClosure(seedFiles, libDest) {
  const queue = [...seedFiles];
  const seen = new Set();

  while (queue.length > 0) {
    const current = queue.pop();
    if (!current || seen.has(current) || !existsSync(current)) {
      continue;
    }
    seen.add(current);

    for (const dep of linuxSharedLibraryDependencies(current)) {
      if (shouldSkipLinuxBundledDependency(dep)) {
        continue;
      }

      const dest = join(libDest, basename(dep));
      if (!existsSync(dest)) {
        copyFileWithMode(dep, dest);
        queue.push(dest);
      }
    }
  }
}

function linuxSharedLibraryDependencies(path) {
  const result = run('ldd', [path], { allowFailure: true, captureOutput: true });
  if (result.status !== 0) {
    return [];
  }

  return result.stdout
    .split(/\r?\n/u)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const mapped = line.match(/=>\s+(\/[^\s]+)\s+\(/u);
      if (mapped) return mapped[1];
      const direct = line.match(/^(\/[^\s]+)\s+\(/u);
      return direct ? direct[1] : null;
    })
    .filter((value) => value && existsSync(value));
}

const LINUX_SYSTEM_LIBRARY_PATTERNS = [
  /^ld-linux/u,
  /^ld-musl/u,
  /^libc\.so/u,
  /^libm\.so/u,
  /^libpthread\.so/u,
  /^libdl\.so/u,
  /^librt\.so/u,
  /^libresolv\.so/u,
  /^libutil\.so/u,

  // Do not bundle the Linux desktop platform stack. GTK/WebKit/AT-SPI/GIO/GLib
  // must come from the user's distro as one compatible set. Bundling any of
  // these can make LTS systems crash on startup with symbol lookup errors like:
  //   libatspi.so.0: undefined symbol: g_once_init_leave_pointer
  /^libglib-2\.0\.so/u,
  /^libgobject-2\.0\.so/u,
  /^libgio-2\.0\.so/u,
  /^libgmodule-2\.0\.so/u,
  /^libgthread-2\.0\.so/u,
  /^libatk-1\.0\.so/u,
  /^libatk-bridge-2\.0\.so/u,
  /^libatspi\.so/u,
  /^libgtk-3\.so/u,
  /^libgdk-3\.so/u,
  /^libwebkit2gtk/u,
  /^libjavascriptcoregtk/u,
  /^libpango/u,
  /^libpangocairo/u,
  /^libpangoft2/u,
  /^libharfbuzz/u,
  /^libcairo/u,
  /^libcairo-gobject/u,
  /^libgdk_pixbuf-2\.0\.so/u,
  /^libepoxy\.so/u,
  /^libdbus-1\.so/u,
  /^libsystemd\.so/u,
  /^libselinux\.so/u,
  /^libmount\.so/u,
  /^libblkid\.so/u,
  /^libffi\.so/u,
  /^libpcre2-8\.so/u,
  /^libz\.so/u,
  /^libzstd\.so/u,
  /^liblzma\.so/u,
  /^libbrotli/u,
  /^libgraphite2\.so/u,
  /^libfontconfig\.so/u,
  /^libfreetype\.so/u,
  /^libexpat\.so/u,
  /^libpng16\.so/u,
  /^libwayland-/u,
  /^libxkbcommon\.so/u,
  /^libX/u,
  /^libxcb/u,
];

function shouldSkipLinuxBundledDependency(dep) {
  return shouldUseSystemLinuxLibrary(basename(dep));
}

function shouldUseSystemLinuxLibrary(name) {
  return LINUX_SYSTEM_LIBRARY_PATTERNS.some((pattern) => pattern.test(name));
}

function sanitizeLinuxGstreamerRoot(root) {
  for (const libDir of [join(root, 'lib'), join(root, 'lib64')]) {
    if (!existsSync(libDir)) {
      continue;
    }

    for (const entry of readdirSync(libDir, { withFileTypes: true })) {
      if (!entry.isFile() && !entry.isSymbolicLink()) {
        continue;
      }
      if (!shouldUseSystemLinuxLibrary(entry.name)) {
        continue;
      }

      const path = join(libDir, entry.name);
      rmSync(path, { force: true });
      console.log(`[bootstrap-native-deps] Removed distro-owned Linux library from bundled runtime: ${path}`);
    }
  }
}

function copyDirectoryEntriesMatching(sourceDir, destinationDir, matcher) {
  if (!existsSync(sourceDir)) {
    return [];
  }

  const copied = [];
  for (const entry of readdirSync(sourceDir, { withFileTypes: true })) {
    if (!entry.isFile() && !entry.isSymbolicLink()) {
      continue;
    }

    const matches = typeof matcher === 'function'
      ? matcher(entry.name)
      : matcher.test(entry.name);
    if (!matches) {
      continue;
    }

    const source = join(sourceDir, entry.name);
    const dest = join(destinationDir, entry.name);
    copyFileWithMode(source, dest);
    copied.push(dest);
  }
  return copied;
}

function copyFileWithMode(source, destination) {
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(source, destination);
  try {
    chmodSync(destination, statSync(source).mode);
  } catch {}
}

function locateExistingPath(candidates) {
  return candidates.find((candidate) => candidate && existsSync(candidate)) ?? null;
}

function resolveMacLocalOpenSslPrefix() {
  if (process.platform !== 'darwin') {
    return null;
  }

  const envPrefix = process.env.OPENSSL_ROOT_DIR?.trim();
  const brewPrefix = run('brew', ['--prefix', 'openssl@3'], { allowFailure: true, captureOutput: true }).stdout.trim();
  const candidate = locateExistingPath([
    envPrefix,
    brewPrefix || null,
    '/opt/homebrew/opt/openssl@3',
    '/usr/local/opt/openssl@3',
  ]);

  if (!candidate) {
    return null;
  }

  return existsSync(join(candidate, 'lib', 'libcrypto.3.dylib')) && existsSync(join(candidate, 'lib', 'libssl.3.dylib'))
    ? candidate
    : null;
}

function stageMacOpenSslPrefix(sourcePrefix, destinationPrefix) {
  mkdirSync(join(destinationPrefix, 'include'), { recursive: true });
  mkdirSync(join(destinationPrefix, 'lib'), { recursive: true });

  const includeDir = join(sourcePrefix, 'include', 'openssl');
  if (existsSync(includeDir)) {
    cpSync(includeDir, join(destinationPrefix, 'include', 'openssl'), { recursive: true, force: true, dereference: true });
  }

  const libDir = join(sourcePrefix, 'lib');
  for (const name of ['libcrypto.3.dylib', 'libssl.3.dylib', 'libcrypto.dylib', 'libssl.dylib']) {
    const source = join(libDir, name);
    if (existsSync(source)) {
      copyFileWithMode(source, join(destinationPrefix, 'lib', name));
    }
  }

  const pkgConfigDir = join(libDir, 'pkgconfig');
  if (existsSync(pkgConfigDir)) {
    mkdirSync(join(destinationPrefix, 'lib', 'pkgconfig'), { recursive: true });
    for (const name of ['libcrypto.pc', 'libssl.pc', 'openssl.pc']) {
      const source = join(pkgConfigDir, name);
      if (existsSync(source)) {
        copyFileWithMode(source, join(destinationPrefix, 'lib', 'pkgconfig', name));
      }
    }
  }
}

function pkgConfigVariable(pkgName, variable) {
  const result = run('pkg-config', [`--variable=${variable}`, pkgName], { allowFailure: true, captureOutput: true });
  if (result.status !== 0) {
    return null;
  }
  const value = result.stdout.trim();
  return value || null;
}

function windowsTargetNeedsGstreamer(targetTriple) {
  return !targetTriple.includes('aarch64');
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

function requiredMacGstreamerRuntimeFiles() {
  return [
    'lib/libgstreamer-1.0.0.dylib',
    'lib/libgstapp-1.0.0.dylib',
    'lib/libgstbase-1.0.0.dylib',
    'lib/libgstaudio-1.0.0.dylib',
    'lib/libgstrtp-1.0.0.dylib',
    'lib/libgstnet-1.0.0.dylib',
    'lib/libgstpbutils-1.0.0.dylib',
    'lib/libgsttag-1.0.0.dylib',
    'lib/libgobject-2.0.0.dylib',
    'lib/libglib-2.0.0.dylib',
    'lib/libgio-2.0.0.dylib',
    'lib/libintl.8.dylib',
    'lib/libopus.0.dylib',
    'lib/gstreamer-1.0/libgstcoreelements.dylib',
    'lib/gstreamer-1.0/libgstapp.dylib',
    'lib/gstreamer-1.0/libgstaudioconvert.dylib',
    'lib/gstreamer-1.0/libgstaudioresample.dylib',
    'lib/gstreamer-1.0/libgstosxaudio.dylib',
    'lib/gstreamer-1.0/libgstopus.dylib',
    'lib/gstreamer-1.0/libgstrtp.dylib',
    'lib/gstreamer-1.0/libgstrtpmanager.dylib',
    'lib/gstreamer-1.0/libgstudp.dylib',
  ];
}

function isUsableFile(file) {
  try {
    return statSync(file).isFile() && statSync(file).size > 0;
  } catch {
    return false;
  }
}

function hasRuntimeFiles(root, requiredFiles) {
  const files = requiredFiles ?? requiredMacGstreamerRuntimeFiles();
  return files.every((relativePath) => isUsableFile(join(root, relativePath)));
}

function hasStructuredGstreamerFramework(path) {
  if (!path) return false;
  return [
    join(path, 'Versions', 'Current'),
    join(path, 'Versions', '1.0'),
    join(path, 'Versions', gstreamerVersion),
  ].some((root) => hasRuntimeFiles(root));
}

function hasFlattenedGstreamerRuntime(path) {
  return Boolean(path) && hasRuntimeFiles(path);
}

function hasGstreamerFrameworkSource(path) {
  return hasStructuredGstreamerFramework(path) || hasFlattenedGstreamerRuntime(path);
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
  ) && existsSync(join(path, 'libexec', 'gstreamer-1.0', 'gst-plugin-scanner'));
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

  if (hasStructuredGstreamerFramework(source)) {
    cpSync(source, destination, {
      recursive: true,
      force: true,
      dereference: false,
      verbatimSymlinks: true,
    });
    return;
  }

  if (hasFlattenedGstreamerRuntime(source)) {
    synthesizeFrameworkFromFlattenedRuntime(source, destination);
    return;
  }

  throw new Error(`Cannot stage macOS GStreamer framework from unusable source ${source}`);
}

function synthesizeFrameworkFromFlattenedRuntime(source, destination) {
  const versionsDir = join(destination, 'Versions');
  const versionDir = join(versionsDir, '1.0');
  mkdirSync(versionDir, { recursive: true });

  for (const entry of ['bin', 'etc', 'lib', 'libexec', 'share', 'Resources']) {
    const sourcePath = join(source, entry);
    if (!existsSync(sourcePath)) continue;
    cpSync(sourcePath, join(versionDir, entry), {
      recursive: true,
      force: true,
      dereference: true,
    });
  }

  const frameworkBinary = join(source, 'GStreamer');
  if (existsSync(frameworkBinary)) {
    copyFileSync(frameworkBinary, join(versionDir, 'GStreamer'));
  }

  symlinkSync('1.0', join(versionsDir, 'Current'));
  symlinkSync('Versions/Current/GStreamer', join(destination, 'GStreamer'));
  if (existsSync(join(versionDir, 'Resources'))) {
    symlinkSync('Versions/Current/Resources', join(destination, 'Resources'));
  }
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

  if (expectedKind === 'tar.xz') {
    return header.length >= 6
      && header[0] === 0xfd
      && header[1] === 0x37
      && header[2] === 0x7a
      && header[3] === 0x58
      && header[4] === 0x5a
      && header[5] === 0x00;
  }

  if (expectedKind === 'msi') {
    const msiMagic = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
    return msiMagic.every((byte, index) => header[index] === byte);
  }

  if (expectedKind === 'zip') {
    return header.length >= 4
      && header[0] === 0x50
      && header[1] === 0x4b
      && header[2] === 0x03
      && header[3] === 0x04;
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
  extractTarGz(tarballPath, extractRoot);
  return extractedPath;
}

function extractTarballSource(tarballPath, extractRoot, extractedDirName) {
  rmSync(extractRoot, { recursive: true, force: true });
  mkdirSync(extractRoot, { recursive: true });
  extractTarGz(tarballPath, extractRoot);
  return join(extractRoot, extractedDirName);
}

function extractTarGz(tarballPath, extractRoot) {
  mkdirSync(extractRoot, { recursive: true });
  run('cmake', ['-E', 'tar', 'xzf', tarballPath], { cwd: extractRoot });
}

function extractTarXz(tarballPath, extractRoot) {
  mkdirSync(extractRoot, { recursive: true });
  run('cmake', ['-E', 'tar', 'xJf', tarballPath], { cwd: extractRoot });
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
