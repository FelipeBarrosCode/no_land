#!/usr/bin/env node
import { chmodSync, copyFileSync, existsSync, mkdirSync, readdirSync, realpathSync, renameSync, rmSync, statSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const target = process.argv[2] ?? process.env.NOLAND_MIC_SENDER_TARGET ?? 'aarch64-apple-darwin';
const productName = 'Noland Connect';
const tripleTargetDir = join(repoRoot, 'src-tauri', 'target', target, 'release');
const defaultTargetDir = join(repoRoot, 'src-tauri', 'target', 'release');
const targetReleaseDir = chooseTargetReleaseDir();
const appPath = join(targetReleaseDir, 'bundle', 'macos', `${productName}.app`);
const dmgPath = join(targetReleaseDir, 'bundle', 'dmg', `${productName}_0.1.0_${target.includes('aarch64') ? 'aarch64' : 'x64'}.dmg`);

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
const microphoneUsageDescription = 'Noland Connect needs microphone access to forward your local mic into your cloud gaming session.';
const frameworkBundleSource = join(frameworksDir, 'GStreamer.framework');
const frameworkBundleDir = join(resourcesDir, 'gstreamer', 'macos', 'GStreamer.framework');
const bundledFrameworkBuildLibDir = toPosix(join(repoRoot, 'src-tauri', 'bundled', 'macos', 'GStreamer.framework', 'Versions', 'Current', 'lib'));

if (existsSync(frameworkBundleSource) && !existsSync(frameworkBundleDir)) {
  mkdirSync(dirname(frameworkBundleDir), { recursive: true });
  renameSync(frameworkBundleSource, frameworkBundleDir);
}

const frameworkRoot = join(frameworkBundleDir, 'Versions', 'Current');
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

const frameworkFiles = existsSync(frameworkLibDir) ? listFiles(frameworkLibDir).filter(isMachOCandidate) : [];
const libexecFiles = existsSync(frameworkLibexecDir) ? listFiles(frameworkLibexecDir).filter(isMachOCandidate) : [];
const macosFiles = listFiles(macosDir).filter(isMachOCandidate);
const frameworkRootLibs = existsSync(frameworksDir)
  ? readdirSync(frameworksDir, { withFileTypes: true })
      .filter((entry) => entry.isFile())
      .map((entry) => join(frameworksDir, entry.name))
      .filter(isMachOCandidate)
  : [];

ensureBundledSdl3(frameworksDir, frameworkRootLibs);
ensureMicrophoneUsageDescription(infoPlistPath, microphoneUsageDescription);

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

adhocSign(appPath, [...frameworkFiles, ...libexecFiles, ...frameworkRootLibs, ...externalLibs.values(), ...macosFiles]);
rebuildDmg(appPath, dmgPath, productName);
console.log(`Patched macOS bundle dependencies: ${appPath}`);

function chooseTargetReleaseDir() {
  const candidates = [tripleTargetDir, defaultTargetDir]
    .map((dir) => ({
      dir,
      app: join(dir, 'bundle', 'macos', `${productName}.app`),
    }))
    .filter((entry) => existsSync(entry.app));

  if (candidates.length === 0) {
    return existsSync(tripleTargetDir) ? tripleTargetDir : defaultTargetDir;
  }

  candidates.sort((a, b) => statSync(b.app).mtimeMs - statSync(a.app).mtimeMs);
  return candidates[0].dir;
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
  const dest = join(frameworksDir, basename(dep));
  if (!existsSync(dest)) {
    mkdirSync(dirname(dest), { recursive: true });
    copyFileSync(dep, dest);
    try {
      chmodSync(dest, statSync(dep).mode);
    } catch {}
  }
  externalLibs.set(dep, dest);
  return dest;
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

function adhocSign(app, nestedFiles) {
  run('xattr', ['-cr', app], { allowFailure: true });

  const uniqueNested = Array.from(new Set((nestedFiles || []).map(safeRealpath))).filter((file) => existsSync(file)).sort((a, b) => b.length - a.length);
  for (const file of uniqueNested) {
    run('codesign', ['--force', '--sign', '-', '--timestamp=none', file], { allowFailure: false });
  }

  run('codesign', ['--force', '--sign', '-', '--timestamp=none', app], { allowFailure: false });
}

function rebuildDmg(app, dmg, volumeName) {
  mkdirSync(dirname(dmg), { recursive: true });
  if (existsSync(dmg)) rmSync(dmg, { force: true });
  run('hdiutil', ['create', '-volname', volumeName, '-srcfolder', app, '-ov', '-format', 'UDZO', dmg]);
}

function ensureBundledSdl3(frameworksDir, frameworkRootLibs) {
  const sdl2Compat = join(frameworksDir, 'libSDL2-2.0.0.dylib');
  if (!existsSync(sdl2Compat)) return;

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

  const sdl3Candidates = [
    '/opt/homebrew/opt/sdl3/lib/libSDL3.dylib',
    '/opt/homebrew/lib/libSDL3.dylib',
    '/usr/local/opt/sdl3/lib/libSDL3.dylib',
    '/usr/local/lib/libSDL3.dylib',
  ];

  const sdl3 = sdl3Candidates.find((candidate) => existsSync(candidate));
  if (!sdl3) {
    throw new Error(`SDL3 companion library is required for ${sdl2Compat} but no libSDL3.dylib source was found`);
  }

  const companionCandidates = [
    sdl3.replace(/libSDL3\.dylib$/, 'libSDL3.0.dylib'),
  ];

  const toCopy = [sdl3, ...companionCandidates.filter((candidate) => existsSync(candidate))];
  for (const source of toCopy) {
    const dest = join(frameworksDir, basename(source));
    if (!existsSync(dest)) {
      copyFileSync(source, dest);
      try {
        chmodSync(dest, statSync(source).mode);
      } catch {}
    }
    if (!frameworkRootLibs.includes(dest) && isMachOCandidate(dest)) {
      frameworkRootLibs.push(dest);
    }
  }

  if (!existsSync(sdl3Dest)) {
    throw new Error(`Failed to bundle libSDL3.dylib into ${frameworksDir}`);
  }
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

function isHomebrewPath(dep) {
  return dep.startsWith('/opt/homebrew/') || dep.startsWith('/usr/local/');
}

function shouldRewriteDependency(dep) {
  return isHomebrewPath(dep)
    || dep.startsWith('/Library/Frameworks/GStreamer.framework/')
    || dep.startsWith('@executable_path/../Frameworks/GStreamer.framework/')
    || dep.startsWith('@executable_path/../Resources/gstreamer/macos/GStreamer.framework/')
    || dep.startsWith('@rpath/GStreamer.framework/')
    || dep.startsWith(`${bundledFrameworkBuildLibDir}/`)
    || dep.includes('/GStreamer.framework/');
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
