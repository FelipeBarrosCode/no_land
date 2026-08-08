#!/usr/bin/env node
import { chmodSync, copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, readdirSync, realpathSync, renameSync, rmSync, statSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const target = process.argv[2] ?? process.env.NOLAND_MIC_SENDER_TARGET ?? 'aarch64-apple-darwin';
const tauriConfig = JSON.parse(readFileSync(join(repoRoot, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const productName = tauriConfig.productName ?? 'Noland Connect';
const version = tauriConfig.version ?? '0.1.0';
const tripleTargetDir = join(repoRoot, 'src-tauri', 'target', target, 'release');
const defaultTargetDir = join(repoRoot, 'src-tauri', 'target', 'release');
const bundleAppRelativePath = join('bundle', 'macos', `${productName}.app`);
const bundleDmgRelativePath = join('bundle', 'dmg', `${productName}_${version}_${target.includes('aarch64') ? 'aarch64' : 'x64'}.dmg`);
const targetReleaseDir = chooseTargetReleaseDir();
const appPath = join(targetReleaseDir, bundleAppRelativePath);
const dmgPath = join(targetReleaseDir, bundleDmgRelativePath);

if (process.platform !== 'darwin') {
  console.log('Skipping macOS bundle dependency fix on non-macOS host');
  process.exit(0);
}

if (!existsSync(appPath)) {
  console.error(`App bundle not found: ${appPath}`);
  process.exit(1);
}

const contentsDir = join(appPath, 'Contents');
const infoPlistPath = join(contentsDir, 'Info.plist');
const macosDir = join(contentsDir, 'MacOS');
const frameworksDir = join(contentsDir, 'Frameworks');
const resourcesDir = join(contentsDir, 'Resources');
const resourcesBinariesDir = join(resourcesDir, 'binaries');
const microphoneUsageDescription = 'Noland Connect needs microphone access to forward your local mic into your cloud gaming session.';
const frameworkBundleSource = join(frameworksDir, 'GStreamer.framework');
const frameworkBundleDir = join(resourcesDir, 'gstreamer', 'macos', 'GStreamer.framework');
const bundledFrameworkBuildLibDir = toPosix(join(repoRoot, 'src-tauri', 'bundled', 'macos', 'GStreamer.framework', 'Versions', 'Current', 'lib'));
const nativePrefix = process.env.NOLAND_NATIVE_DEPS_PREFIX?.trim() ? resolve(process.env.NOLAND_NATIVE_DEPS_PREFIX.trim()) : null;

if (existsSync(frameworkBundleSource) && !existsSync(frameworkBundleDir)) {
  mkdirSync(dirname(frameworkBundleDir), { recursive: true });
  renameSync(frameworkBundleSource, frameworkBundleDir);
}

const frameworkRoot = join(frameworkBundleDir, 'Versions', 'Current');
const frameworkBinDir = join(frameworkRoot, 'bin');
const frameworkLibDir = join(frameworkRoot, 'lib');
const frameworkLibexecDir = join(frameworkRoot, 'libexec');
const frameworkPluginDir = join(frameworkLibDir, 'gstreamer-1.0');
const frameworkPluginValidateDir = join(frameworkPluginDir, 'validate');
const frameworkShareValidateDir = join(frameworkRoot, 'share', 'gstreamer-1.0', 'validate');
const allowedGStreamerPlugins = new Set([
  'libgstcoreelements.dylib',
  'libgstaudioconvert.dylib',
  'libgstaudioresample.dylib',
  'libgstaudiorate.dylib',
  'libgstosxaudio.dylib',
  'libgstopus.dylib',
  'libgstrtp.dylib',
  'libgstrtpmanager.dylib',
  'libgstudp.dylib',
  'libgsttypefindfunctions.dylib',
  'libgstvolume.dylib',
]);

if (existsSync(frameworkBinDir)) {
  console.log(`[fix-macos-bundle-deps] Removing unused GStreamer command-line tools from ${frameworkBinDir}`);
  rmSync(frameworkBinDir, { recursive: true, force: true });
}
if (existsSync(frameworkPluginValidateDir)) {
  rmSync(frameworkPluginValidateDir, { recursive: true, force: true });
}
if (existsSync(frameworkShareValidateDir)) {
  rmSync(frameworkShareValidateDir, { recursive: true, force: true });
}
if (existsSync(frameworkPluginDir)) {
  for (const entry of readdirSync(frameworkPluginDir, { withFileTypes: true })) {
    const full = join(frameworkPluginDir, entry.name);
    if (entry.isDirectory()) {
      rmSync(full, { recursive: true, force: true });
      continue;
    }
    if (!allowedGStreamerPlugins.has(entry.name)) {
      rmSync(full, { recursive: true, force: true });
    }
  }
}

const appleSigningIdentity = process.env.APPLE_SIGNING_IDENTITY?.trim() || '';
console.log(`[fix-macos-bundle-deps] Preparing macOS bundle fix for ${target}`);
console.log(`[fix-macos-bundle-deps] App bundle: ${appPath}`);
console.log(`[fix-macos-bundle-deps] Signing identity: ${appleSigningIdentity || 'ad-hoc'}`);

const frameworkRootLibs = existsSync(frameworksDir)
  ? readdirSync(frameworksDir, { withFileTypes: true })
      .filter((entry) => entry.isFile())
      .map((entry) => join(frameworksDir, entry.name))
      .filter(isMachOCandidate)
  : [];

ensureBundledSdl3(frameworksDir, frameworkRootLibs);
ensureBundledWireguardSidecars(target, resourcesBinariesDir);
pruneIrrelevantMacResourceSidecars(resourcesBinariesDir, target);
ensureMicrophoneUsageDescription(infoPlistPath, microphoneUsageDescription);
sanitizeBundleSymlinks(appPath);

const frameworkFiles = existsSync(frameworkLibDir) ? listFiles(frameworkLibDir).filter(isMachOCandidate) : [];
const libexecFiles = existsSync(frameworkLibexecDir) ? listFiles(frameworkLibexecDir).filter(isMachOCandidate) : [];
const macosFiles = listFiles(macosDir).filter(isCodeSignableFile);
const resourceBinaryFiles = existsSync(resourcesBinariesDir)
  ? listFiles(resourcesBinariesDir).filter(isCodeSignableFile)
  : [];
const explicitMacSidecarFiles = collectExplicitMacSidecarFiles(macosDir, resourcesBinariesDir, target);

const frameworkIndex = new Map();
for (const file of frameworkFiles) {
  const rel = relative(frameworkLibDir, file);
  frameworkIndex.set(rel, file);
  frameworkIndex.set(basename(file), file);
}

const frameworkRootIndex = new Map();
for (const file of frameworkRootLibs) {
  frameworkRootIndex.set(basename(file), file);
}

const scanned = new Set();
const allTargets = new Set([...frameworkFiles, ...libexecFiles, ...frameworkRootLibs, ...macosFiles]);
const externalLibs = new Map();

for (const file of [...allTargets]) {
  patchFile(file);
}
for (const file of [...allTargets]) {
  rewriteRemainingGStreamerDeps(file);
}
for (const file of [...allTargets]) {
  rewriteBuildTreeGStreamerDeps(file);
}

for (const file of frameworkFiles) {
  setInstallId(file, frameworkIdFor(file));
}
for (const file of externalLibs.values()) {
  setInstallId(file, `@rpath/${basename(file)}`);
}
for (const file of frameworkRootLibs) {
  setInstallId(file, `@rpath/${basename(file)}`);
}

console.log(`[fix-macos-bundle-deps] Framework dylibs: ${frameworkFiles.length}, libexec tools: ${libexecFiles.length}, root framework dylibs: ${frameworkRootLibs.length}`);
console.log(`[fix-macos-bundle-deps] Resource binaries: ${resourceBinaryFiles.length}, explicit sidecars: ${explicitMacSidecarFiles.length}, app executables: ${macosFiles.length}`);
console.log('[fix-macos-bundle-deps] Re-signing patched bundle contents');
resignBundle(appPath, [...frameworkFiles, ...libexecFiles, ...frameworkRootLibs, ...externalLibs.values(), ...resourceBinaryFiles, ...explicitMacSidecarFiles, ...macosFiles, frameworkBundleDir]);
console.log('[fix-macos-bundle-deps] Verifying explicitly managed macOS sidecars');
verifySignedMacSidecars(explicitMacSidecarFiles);
console.log('[fix-macos-bundle-deps] Rebuilding DMG payload');
cleanupStaleMacDmgArtifacts(targetReleaseDir);
rebuildDmg(appPath, dmgPath, productName);
if (!verifyDmgBundle(dmgPath, productName)) {
  console.warn(`Initial DMG payload verification failed for ${dmgPath}; rebuilding from a fresh copy of the patched app bundle.`);
  rebuildDmgFromFreshCopy(appPath, dmgPath, productName);
  if (!verifyDmgBundle(dmgPath, productName)) {
    throw new Error(`Final DMG verification failed for ${dmgPath}`);
  }
}
console.log(`Patched macOS bundle dependencies: ${appPath}`);

function chooseTargetReleaseDir() {
  const explicitTargetApp = join(tripleTargetDir, bundleAppRelativePath);
  if (existsSync(explicitTargetApp)) {
    return tripleTargetDir;
  }

  const defaultTargetApp = join(defaultTargetDir, bundleAppRelativePath);
  if (existsSync(defaultTargetApp)) {
    return defaultTargetDir;
  }

  return existsSync(tripleTargetDir) ? tripleTargetDir : defaultTargetDir;
}

function patchFile(file) {
  const realFile = safeRealpath(file);
  if (scanned.has(realFile)) return;
  scanned.add(realFile);

  const deps = listDependencies(file);
  for (const dep of deps) {
    if (!shouldRewriteDependency(dep)) continue;
    const bundledTarget = resolveBundledTarget(dep);
    if (!bundledTarget) continue;
    const desired = installNameForConsumer(file, bundledTarget);
    run('install_name_tool', ['-change', dep, desired, file]);
    if (!allTargets.has(bundledTarget)) {
      allTargets.add(bundledTarget);
      patchFile(bundledTarget);
    }
  }
}

function resolveBundledTarget(dep) {
  if (dep.startsWith('/Library/Frameworks/GStreamer.framework/Versions/Current/lib/')) {
    const suffix = dep.split('/lib/')[1];
    const candidate = suffix ? join(frameworkLibDir, suffix) : null;
    if (candidate && existsSync(candidate)) return candidate;
  }
  if (dep.startsWith('@executable_path/../Resources/gstreamer/macos/GStreamer.framework/Versions/Current/lib/')) {
    const suffix = dep.split('/lib/')[1];
    const candidate = suffix ? join(frameworkLibDir, suffix) : null;
    if (candidate && existsSync(candidate)) return candidate;
  }
  if (dep.startsWith('@executable_path/../Frameworks/GStreamer.framework/Versions/Current/lib/')) {
    const suffix = dep.split('/lib/')[1];
    const candidate = suffix ? join(frameworkLibDir, suffix) : null;
    if (candidate && existsSync(candidate)) return candidate;
  }
  if (dep.startsWith('@rpath/GStreamer.framework/Versions/Current/lib/')) {
    const suffix = dep.split('/lib/')[1];
    const candidate = suffix ? join(frameworkLibDir, suffix) : null;
    if (candidate && existsSync(candidate)) return candidate;
  }

  const nativeRpathTarget = resolveNativeRpathTarget(dep) || resolveNativeLoaderPathTarget(dep);
  if (nativeRpathTarget) {
    return stageExternalLibrary(nativeRpathTarget);
  }

  const suffix = dep.includes('/lib/') ? dep.split('/lib/')[1] : null;
  if (suffix) {
    const candidate = join(frameworkLibDir, suffix);
    if (existsSync(candidate)) return candidate;
  }
  const name = basename(dep);
  if (frameworkRootIndex.has(name)) {
    return frameworkRootIndex.get(name);
  }
  if (frameworkIndex.has(name)) {
    return frameworkIndex.get(name);
  }
  if (externalLibs.has(dep)) {
    return externalLibs.get(dep);
  }
  if (!existsSync(dep)) {
    return null;
  }

  return stageExternalLibrary(dep);
}

function installNameForConsumer(consumer, target) {
  if (target.startsWith(frameworkLibDir)) {
    const rel = relative(frameworkLibDir, target);
    if (consumer.startsWith(macosDir)) {
      return `@executable_path/../Resources/gstreamer/macos/GStreamer.framework/Versions/Current/lib/${toPosix(rel)}`;
    }
  }
  const consumerDir = dirname(consumer);
  const rel = relative(consumerDir, target);
  return `@loader_path/${toPosix(rel)}`;
}

function rewriteRemainingGStreamerDeps(file) {
  for (const dep of listDependencies(file)) {
    if (!dep.includes('GStreamer.framework/Versions/Current/lib/')) continue;
    const target = resolveBundledTarget(dep);
    if (!target) continue;
    const desired = installNameForConsumer(file, target);
    if (dep === desired) continue;
    run('install_name_tool', ['-change', dep, desired, file], { allowFailure: false });
  }
}

function rewriteBuildTreeGStreamerDeps(file) {
  for (const dep of listDependencies(file)) {
    if (!dep.startsWith(`${bundledFrameworkBuildLibDir}/`)) continue;
    const target = resolveBundledTarget(dep);
    if (!target) continue;
    const desired = installNameForConsumer(file, target);
    if (dep === desired) continue;
    run('install_name_tool', ['-change', dep, desired, file], { allowFailure: false });
  }
}

function frameworkIdFor(file) {
  const rel = relative(frameworkLibDir, file);
  return `@rpath/GStreamer.framework/Versions/Current/lib/${toPosix(rel)}`;
}

function listDependencies(file) {
  const output = run('otool', ['-L', file], { allowFailure: true });
  if (output.status !== 0) return [];
  return output.stdout
    .split('\n')
    .slice(1)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => line.split(' ')[0])
    .filter(Boolean);
}

function setInstallId(file, id) {
  if (!file.endsWith('.dylib')) return;
  run('install_name_tool', ['-id', id, file], { allowFailure: true });
}

function ensureBundledWireguardSidecars(targetTriple, destinationDir) {
  const stagedDir = join(repoRoot, 'src-tauri', 'binaries');
  const requiredNames = [
    `wg-${targetTriple}`,
    `wg-quick-${targetTriple}`,
  ];
  const optionalNames = targetTriple.endsWith('apple-darwin')
    ? [`wg-bash-${targetTriple}`, `wg-quick-real-${targetTriple}`]
    : [];
  const stagedSidecars = [...requiredNames, ...optionalNames].map((name) => ({
    name,
    source: join(stagedDir, name),
    dest: join(destinationDir, name),
    required: requiredNames.includes(name),
  }));

  mkdirSync(destinationDir, { recursive: true });
  for (const { source, dest, required } of stagedSidecars) {
    if (!existsSync(source)) {
      if (required) {
        throw new Error(`Required staged WireGuard sidecar is missing: ${source}`);
      }
      continue;
    }
    copyFileSync(source, dest);
    try {
      chmodSync(dest, statSync(source).mode);
    } catch {}
  }
}

function pruneIrrelevantMacResourceSidecars(destinationDir, targetTriple) {
  if (!existsSync(destinationDir)) return;

  const managedMacSidecarPrefixes = [
    'wg-',
    'wg-quick-',
    'wg-bash-',
    'wg-quick-real-',
    'ssh-',
    'scp-',
    'ssh-keygen-',
  ];

  for (const entry of readdirSync(destinationDir, { withFileTypes: true })) {
    if (!entry.isFile()) continue;

    if (/^wireguard-.*\.exe$/u.test(entry.name)) {
      rmSync(join(destinationDir, entry.name), { force: true });
      continue;
    }

    const isManagedMacSidecar = managedMacSidecarPrefixes.some((prefix) => entry.name.startsWith(prefix));
    if (!isManagedMacSidecar) continue;
    if (!entry.name.endsWith(targetTriple)) {
      rmSync(join(destinationDir, entry.name), { force: true });
    }
  }
}

function ensureMicrophoneUsageDescription(infoPlist, message) {
  if (!existsSync(infoPlist)) return;
  const printResult = run('/usr/libexec/PlistBuddy', ['-c', 'Print :NSMicrophoneUsageDescription', infoPlist], { allowFailure: true });
  const escapedMessage = message.replace(/"/g, '\\"');
  if (printResult.status === 0) {
    run('/usr/libexec/PlistBuddy', ['-c', `Set :NSMicrophoneUsageDescription "${escapedMessage}"`, infoPlist], { allowFailure: false });
    return;
  }
  run('/usr/libexec/PlistBuddy', ['-c', `Add :NSMicrophoneUsageDescription string "${escapedMessage}"`, infoPlist], { allowFailure: false });
}

function resignBundle(app, nestedFiles) {
  run('xattr', ['-cr', app], { allowFailure: true });

  const uniqueNested = Array.from(new Set((nestedFiles || []).map(safeRealpath)))
    .filter((file) => existsSync(file))
    .sort((a, b) => b.length - a.length);

  console.log(`[fix-macos-bundle-deps] Signing ${uniqueNested.length} nested code objects`);
  for (const file of uniqueNested) {
    signCodeObject(file, { runtime: shouldEnableHardenedRuntime(file) });
  }

  console.log('[fix-macos-bundle-deps] Signing final .app bundle');
  signCodeObject(app, { runtime: appleSigningIdentity !== '' });
}

function signCodeObject(path, { runtime }) {
  console.log(`[fix-macos-bundle-deps] Signing ${relative(appPath, path) || '.'}${runtime ? ' (runtime)' : ''}`);
  const args = ['--force'];
  if (appleSigningIdentity) {
    args.push('--sign', appleSigningIdentity, '--timestamp');
    if (runtime) {
      args.push('--options', 'runtime');
      args.push('--preserve-metadata=identifier,entitlements,flags,runtime,requirements');
    }
  } else {
    args.push('--sign', '-', '--timestamp=none');
  }
  args.push(path);
  run('codesign', args, { allowFailure: false });
}

function shouldEnableHardenedRuntime(file) {
  if (!appleSigningIdentity || !existsSync(file)) {
    return false;
  }
  if (basename(file).endsWith('.dylib')) {
    return false;
  }

  const info = run('file', ['-b', file], { allowFailure: true });
  return info.status === 0 && info.stdout.includes('Mach-O');
}

function collectExplicitMacSidecarFiles(appMacosDir, appResourcesBinariesDir, targetTriple) {
  const candidates = [
    join(appMacosDir, 'gotatun'),
    join(appMacosDir, `gotatun-${targetTriple}`),
    join(appMacosDir, 'noland-mic-sender'),
    join(appMacosDir, `noland-mic-sender-${targetTriple}`),
    join(appResourcesBinariesDir, `wg-${targetTriple}`),
    join(appResourcesBinariesDir, `wg-quick-${targetTriple}`),
    join(appResourcesBinariesDir, `wg-bash-${targetTriple}`),
    join(appResourcesBinariesDir, `wg-quick-real-${targetTriple}`),
    join(appResourcesBinariesDir, `ssh-${targetTriple}`),
    join(appResourcesBinariesDir, `scp-${targetTriple}`),
    join(appResourcesBinariesDir, `ssh-keygen-${targetTriple}`),
  ];
  return candidates.filter((file, index, list) => existsSync(file) && list.indexOf(file) === index);
}

function verifySignedMacSidecars(files) {
  for (const file of files) {
    const verify = run('codesign', ['--verify', '--verbose=2', file], { allowFailure: true });
    if (verify.status !== 0) {
      throw new Error(`Bundled macOS sidecar is not signed correctly: ${file}\n${verify.stderr || verify.stdout}`);
    }
  }
}

function rebuildDmg(app, dmg, volumeName) {
  mkdirSync(dirname(dmg), { recursive: true });
  if (existsSync(dmg)) rmSync(dmg, { force: true });

  const tempDir = mkdtempSync(join(tmpdir(), 'noland-dmg-out-'));
  const tempRoot = mkdtempSync(join(tmpdir(), 'noland-dmg-src-'));
  const tempDmg = join(tempDir, basename(dmg));
  const stagedApp = join(tempRoot, basename(app));
  try {
    run('ditto', [app, stagedApp]);
    run('hdiutil', ['create', '-volname', volumeName, '-srcfolder', stagedApp, '-ov', '-format', 'UDZO', tempDmg]);
    copyFileSync(tempDmg, dmg);
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
    rmSync(tempRoot, { recursive: true, force: true });
  }
}

function rebuildDmgFromFreshCopy(app, dmg, volumeName) {
  rebuildDmg(app, dmg, volumeName);
}

function cleanupStaleMacDmgArtifacts(releaseDir) {
  const macosBundleDir = join(releaseDir, 'bundle', 'macos');
  if (!existsSync(macosBundleDir)) return;

  for (const entry of readdirSync(macosBundleDir, { withFileTypes: true })) {
    if (!entry.isFile()) continue;
    if (!/^rw\.[^.]+\..+\.dmg$/u.test(entry.name)) continue;
    rmSync(join(macosBundleDir, entry.name), { force: true });
  }
}

function verifyDmgBundle(dmg, volumeName) {
  if (!existsSync(dmg)) {
    return false;
  }

  const mountPoint = mkdtempSync(join(tmpdir(), 'noland-dmg-mount-'));
  const mountedApp = join(mountPoint, `${volumeName}.app`);
  const mountedBinary = join(mountedApp, 'Contents', 'MacOS', 'noland-connect');
  const mountedFrameworksDir = join(mountedApp, 'Contents', 'Frameworks');

  try {
    const attach = run('hdiutil', ['attach', dmg, '-mountpoint', mountPoint, '-nobrowse', '-readonly'], { allowFailure: true });
    if (attach.status !== 0) {
      return false;
    }

    const requiredFrameworks = ['libcrypto.3.dylib', 'libopus.0.dylib', 'libSDL2-2.0.0.dylib'];
    for (const dylib of requiredFrameworks) {
      if (!existsSync(join(mountedFrameworksDir, dylib))) {
        return false;
      }
    }

    const deps = listDependencies(mountedBinary);
    return requiredFrameworks.every((dylib) => deps.includes(`@loader_path/../Frameworks/${dylib}`));
  } finally {
    run('hdiutil', ['detach', mountPoint], { allowFailure: true });
    rmSync(mountPoint, { recursive: true, force: true });
  }
}

function ensureBundledSdl3(frameworksDir, frameworkRootLibs) {
  const sdl2Compat = join(frameworksDir, 'libSDL2-2.0.0.dylib');
  if (!existsSync(sdl2Compat)) return;

  const sdl2Deps = listDependencies(sdl2Compat);
  const needsSdl3 = sdl2Deps.some((dep) => basename(dep).startsWith('libSDL3'));
  if (!needsSdl3) {
    return;
  }

  const sdl3Dest = join(frameworksDir, 'libSDL3.dylib');
  const sdl3CompatDest = join(frameworksDir, 'libSDL3.0.dylib');
  if (existsSync(sdl3Dest) && existsSync(sdl3CompatDest)) {
    for (const existing of [sdl3Dest, sdl3CompatDest]) {
      if (!frameworkRootLibs.includes(existing) && isMachOCandidate(existing)) {
        frameworkRootLibs.push(existing);
      }
    }
    return;
  }

  const nativePrefix = process.env.NOLAND_NATIVE_DEPS_PREFIX?.trim();
  const explicitSdl3 = process.env.NOLAND_SDL3_DYLIB?.trim();
  const sdl3Candidates = [
    explicitSdl3,
    nativePrefix ? join(nativePrefix, 'lib', 'libSDL3.dylib') : null,
  ].filter(Boolean);

  const sdl3 = sdl3Candidates.find((candidate) => existsSync(candidate));
  if (!sdl3) {
    throw new Error(`SDL3 companion library is required for ${sdl2Compat} but no project-managed libSDL3.dylib source was found`);
  }

  const companionCandidates = [
    sdl3.replace(/libSDL3\.dylib$/, 'libSDL3.0.dylib'),
  ];

  const toCopy = [sdl3, ...companionCandidates.filter((candidate) => existsSync(candidate))];
  for (const source of toCopy) {
    const dest = join(frameworksDir, basename(source));
    if (existsSync(dest)) {
      rmSync(dest, { force: true });
    }
    copyFileSync(source, dest);
    try {
      chmodSync(dest, statSync(source).mode);
    } catch {}
    if (!frameworkRootLibs.includes(dest) && isMachOCandidate(dest)) {
      frameworkRootLibs.push(dest);
    }
  }

  if (!existsSync(sdl3Dest)) {
    throw new Error(`Failed to bundle libSDL3.dylib into ${frameworksDir}`);
  }
}

function sanitizeBundleSymlinks(root) {
  if (!existsSync(root)) return;

  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      const stats = lstatSync(full);
      if (stats.isSymbolicLink()) {
        materializeSymlink(full, root);
        const materializedStats = lstatSync(full);
        if (materializedStats.isDirectory()) {
          stack.push(full);
        }
        continue;
      }
      if (stats.isDirectory()) {
        stack.push(full);
      }
    }
  }
}

function materializeSymlink(path, bundleRoot) {
  const rawTarget = readlinkSync(path);
  const resolvedTarget = resolve(dirname(path), rawTarget);
  if (!existsSync(resolvedTarget)) {
    throw new Error(`Bundle contains a broken symlink: ${path} -> ${rawTarget}`);
  }

  const relativePath = relative(resolve(bundleRoot), resolvedTarget);
  const pointsOutsideBundle = relativePath === '' ? false : relativePath.startsWith('..');
  console.log(`[fix-macos-bundle-deps] Materializing symlink ${relative(appPath, path) || '.'} -> ${rawTarget}${pointsOutsideBundle ? ' (outside bundle)' : ''}`);

  rmSync(path, { recursive: true, force: true });
  const targetStats = statSync(resolvedTarget);
  if (targetStats.isDirectory()) {
    cpSync(resolvedTarget, path, { recursive: true, force: true, dereference: true });
    return;
  }

  copyFileSync(resolvedTarget, path);
  try {
    chmodSync(path, targetStats.mode);
  } catch {}
}

function listFiles(root) {
  if (!existsSync(root)) return [];
  const results = [];
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      if (entry.isDirectory()) {
        stack.push(full);
      } else if (entry.isFile() || entry.isSymbolicLink()) {
        results.push(full);
      }
    }
  }
  return results;
}

function isMachOCandidate(file) {
  const name = basename(file);
  if (name.endsWith('.dylib')) return true;
  try {
    const mode = statSync(file).mode;
    if ((mode & 0o111) === 0) {
      return false;
    }
    const info = run('file', ['-b', file], { allowFailure: true });
    return info.status === 0 && info.stdout.includes('Mach-O');
  } catch {
    return false;
  }
}

function isCodeSignableFile(file) {
  if (isMachOCandidate(file)) {
    return true;
  }
  try {
    return (statSync(file).mode & 0o111) !== 0;
  } catch {
    return false;
  }
}

function isHomebrewPath(dep) {
  return dep.startsWith('/opt/homebrew/') || dep.startsWith('/usr/local/');
}

function isManagedNativeDependency(dep) {
  if (!nativePrefix) {
    return false;
  }

  const nativeLibDir = toPosix(join(nativePrefix, 'lib'));
  const nativeLib64Dir = toPosix(join(nativePrefix, 'lib64'));
  return dep.startsWith(`${nativeLibDir}/`) || dep.startsWith(`${nativeLib64Dir}/`);
}

function shouldRewriteDependency(dep) {
  return isHomebrewPath(dep)
    || isManagedNativeDependency(dep)
    || dep.startsWith('/Library/Frameworks/GStreamer.framework/')
    || dep.startsWith('@executable_path/../Frameworks/GStreamer.framework/')
    || dep.startsWith('@executable_path/../Resources/gstreamer/macos/GStreamer.framework/')
    || dep.startsWith('@rpath/GStreamer.framework/')
    || dep.startsWith(`${bundledFrameworkBuildLibDir}/`)
    || dep.includes('/GStreamer.framework/')
    || Boolean(resolveNativeRpathTarget(dep))
    || Boolean(resolveNativeLoaderPathTarget(dep));
}

function resolveNativeRpathTarget(dep) {
  if (!nativePrefix || !dep.startsWith('@rpath/')) {
    return null;
  }

  const candidate = join(nativePrefix, 'lib', basename(dep));
  return existsSync(candidate) ? candidate : null;
}

function resolveNativeLoaderPathTarget(dep) {
  if (!nativePrefix || !dep.startsWith('@loader_path/')) {
    return null;
  }
  if (!dep.includes('.native-deps/')) {
    return null;
  }

  const candidate = join(nativePrefix, 'lib', basename(dep));
  return existsSync(candidate) ? candidate : null;
}

function stageExternalLibrary(sourcePath) {
  if (externalLibs.has(sourcePath)) {
    return externalLibs.get(sourcePath);
  }

  const dest = join(frameworksDir, basename(sourcePath));
  if (!existsSync(dest)) {
    mkdirSync(dirname(dest), { recursive: true });
    copyFileSync(sourcePath, dest);
    try {
      chmodSync(dest, statSync(sourcePath).mode);
    } catch {}
  }
  externalLibs.set(sourcePath, dest);
  if (!frameworkRootIndex.has(basename(dest))) {
    frameworkRootIndex.set(basename(dest), dest);
  }
  return dest;
}

function toPosix(value) {
  return value.split('\\').join('/');
}

function safeRealpath(file) {
  try {
    return realpathSync(file);
  } catch {
    return file;
  }
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (result.status !== 0 && !options.allowFailure) {
    throw new Error(`${command} ${args.join(' ')} failed: ${result.stderr || result.stdout}`);
  }
  return result;
}
