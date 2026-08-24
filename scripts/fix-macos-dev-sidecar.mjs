#!/usr/bin/env node
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
} from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const sidecarPath = process.argv[2] ? resolve(repoRoot, process.argv[2]) : null;
const frameworkDir = resolve(repoRoot, 'src-tauri', 'bundled', 'macos', 'GStreamer.framework');
const sidecarEntitlements = resolve(repoRoot, 'mic-sidecar', 'noland-mic-sender.entitlements');
const frameworkRoot = join(frameworkDir, 'Versions', 'Current');
const frameworkLibDir = join(frameworkRoot, 'lib');
const frameworkLibexecDir = join(frameworkRoot, 'libexec');
const frameworkPluginDir = join(frameworkLibDir, 'gstreamer-1.0');
const frameworkPluginValidateDir = join(frameworkPluginDir, 'validate');
const frameworkShareValidateDir = join(frameworkRoot, 'share', 'gstreamer-1.0', 'validate');
const gstreamerVersion = process.env.NOLAND_GSTREAMER_VERSION?.trim()
  || process.env.GSTREAMER_VERSION?.trim()
  || '1.24.13';
const projectManagedRuntimeRoot = resolve(
  repoRoot,
  'src-tauri',
  '.native-deps',
  'cache',
  `gstreamer-${gstreamerVersion}-macos-universal`,
  'root',
);
const frameworkSourceCandidates = [
  projectManagedRuntimeRoot,
  process.env.NOLAND_GSTREAMER_FRAMEWORK?.trim(),
].filter(Boolean);
const nativePrefix = process.env.NOLAND_NATIVE_DEPS_PREFIX?.trim() ? resolve(process.env.NOLAND_NATIVE_DEPS_PREFIX.trim()) : null;
const allowedGStreamerPlugins = new Set([
  'libgstcoreelements.dylib',
  'libgstapp.dylib',
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
  'libgstwebrtcdsp.dylib',
]);
const requiredFrameworkFiles = [
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
  'lib/libgstbadaudio-1.0.0.dylib',
  'lib/libwebrtc-audio-processing-1.3.dylib',
  ...Array.from(allowedGStreamerPlugins, (plugin) => `lib/gstreamer-1.0/${plugin}`),
];

if (process.platform !== 'darwin') {
  process.exit(0);
}

if (!sidecarPath || !existsSync(sidecarPath)) {
  console.error(`Sidecar binary not found: ${sidecarPath ?? '<missing>'}`);
  process.exit(1);
}

prepareBundledFramework();

if (!existsSync(frameworkLibDir)) {
  console.error(`Bundled GStreamer runtime not found after preparation: ${frameworkLibDir}`);
  process.exit(1);
}

const frameworkFiles = listFiles(frameworkLibDir).filter(isMachOCandidate);
const libexecFiles = existsSync(frameworkLibexecDir) ? listFiles(frameworkLibexecDir).filter(isMachOCandidate) : [];
const frameworkIndex = new Map();
for (const file of frameworkFiles) {
  const rel = relative(frameworkLibDir, file);
  frameworkIndex.set(rel, file);
  frameworkIndex.set(basename(file), file);
}

const scanned = new Set();
const allTargets = new Set([...frameworkFiles, ...libexecFiles, sidecarPath]);
const externalLibs = new Map();

for (const file of [...allTargets]) {
  patchFile(file);
}

for (const file of [...frameworkFiles, ...libexecFiles]) {
  setInstallId(file, file);
}
for (const file of externalLibs.values()) {
  setInstallId(file, file);
}
ensureRpath(sidecarPath, frameworkLibDir);

adhocSign([sidecarPath, ...frameworkFiles, ...libexecFiles, ...externalLibs.values()]);
console.log(`Patched macOS dev sidecar runtime: ${sidecarPath}`);

function prepareBundledFramework() {
  if (!hasFrameworkRuntime(frameworkDir)) {
    const sourceFramework = resolveFrameworkSource();
    if (!sourceFramework) {
      console.error('Unable to locate a complete macOS GStreamer microphone runtime. Run node scripts/bootstrap-native-deps.mjs --target <triple> before local development builds.');
      process.exit(1);
    }

    if (resolve(sourceFramework) !== resolve(frameworkDir)) {
      console.warn(`[fix-macos-dev-sidecar] Restaging incomplete GStreamer framework from ${sourceFramework}`);
      removeIfExists(frameworkDir);
      copyDirResolved(sourceFramework, frameworkDir);
    }
  }

  restoreRequiredFrameworkFiles();
  restoreAllowedPlugins();
  pruneBundledPlugins();

  if (!hasFrameworkRuntime(frameworkDir)) {
    console.error('Bundled GStreamer framework is missing required microphone/Opus/RTP runtime files after repair.');
    process.exit(1);
  }
}

function restoreRequiredFrameworkFiles() {
  const sourceFramework = resolveFrameworkSource();
  if (!sourceFramework || resolve(sourceFramework) === resolve(frameworkDir)) return;

  const sourceRoot = frameworkVersionRoot(sourceFramework);
  const destinationRoot = frameworkVersionRoot(frameworkDir);
  if (!sourceRoot || !destinationRoot) return;

  for (const relativePath of requiredFrameworkFiles) {
    const source = join(sourceRoot, relativePath);
    const destination = join(destinationRoot, relativePath);
    if (!isUsableFile(destination) && isUsableFile(source)) {
      mkdirSync(dirname(destination), { recursive: true });
      copyFileSync(source, destination);
      chmodSync(destination, statSync(source).mode);
    }
  }
}

function restoreAllowedPlugins() {
  const sourceFramework = resolveFrameworkSource();
  if (!sourceFramework || resolve(sourceFramework) === resolve(frameworkDir)) return;

  const sourcePluginDirs = [
    join(sourceFramework, 'Versions', 'Current', 'lib', 'gstreamer-1.0'),
    join(sourceFramework, 'lib', 'gstreamer-1.0'),
  ];
  const sourcePluginDir = sourcePluginDirs.find((candidate) => existsSync(candidate));
  if (!sourcePluginDir) return;

  mkdirSync(frameworkPluginDir, { recursive: true });
  for (const plugin of allowedGStreamerPlugins) {
    const destination = join(frameworkPluginDir, plugin);
    const source = join(sourcePluginDir, plugin);
    if (!isUsableFile(destination) && isUsableFile(source)) {
      copyFileSync(source, destination);
      chmodSync(destination, statSync(source).mode);
    }
  }
}

function resolveFrameworkSource() {
  for (const candidate of frameworkSourceCandidates) {
    if (!candidate) continue;
    const path = resolve(candidate);
    if (hasFrameworkRuntime(path)) {
      return path;
    }
  }
  return null;
}

function frameworkVersionRoot(path) {
  for (const candidate of [
    join(path, 'Versions', 'Current'),
    join(path, 'Versions', '1.0'),
    path,
  ]) {
    if (existsSync(join(candidate, 'lib'))) return candidate;
  }
  return null;
}

function isUsableFile(path) {
  try {
    return statSync(path).isFile() && statSync(path).size > 0;
  } catch {
    return false;
  }
}

function hasFrameworkRuntime(path) {
  const root = frameworkVersionRoot(path);
  return Boolean(root) && requiredFrameworkFiles.every((relativePath) => isUsableFile(join(root, relativePath)));
}

function pruneBundledPlugins() {
  removeIfExists(frameworkPluginValidateDir);
  removeIfExists(frameworkShareValidateDir);

  if (!existsSync(frameworkPluginDir)) return;

  for (const entry of readdirSync(frameworkPluginDir, { withFileTypes: true })) {
    const full = join(frameworkPluginDir, entry.name);
    if (entry.isDirectory()) {
      removeIfExists(full);
      continue;
    }
    if (!allowedGStreamerPlugins.has(entry.name)) {
      removeIfExists(full);
    }
  }
}

function patchFile(file) {
  if (!existsSync(file)) return;
  const realFile = safeRealpath(file);
  if (scanned.has(realFile)) return;
  scanned.add(realFile);

  const deps = listDependencies(file);
  for (const dep of deps) {
    if (!shouldRewriteDependency(dep)) continue;
    const bundledTarget = resolveBundledTarget(dep);
    if (!bundledTarget) continue;
    if (!existsSync(bundledTarget)) continue;
    const desired = installNameForConsumer(file, bundledTarget);
    run('install_name_tool', ['-change', dep, desired, file], { allowFailure: false });
    if (!allTargets.has(bundledTarget)) {
      allTargets.add(bundledTarget);
      patchFile(bundledTarget);
    }
  }
}

function resolveBundledTarget(dep) {
  const inFrameworkPrefixes = [
    '/Library/Frameworks/GStreamer.framework/Versions/Current/lib/',
    '@rpath/GStreamer.framework/Versions/Current/lib/',
    '@executable_path/../Resources/gstreamer/macos/GStreamer.framework/Versions/Current/lib/',
    '@executable_path/../Frameworks/GStreamer.framework/Versions/Current/lib/',
  ];
  for (const prefix of inFrameworkPrefixes) {
    if (dep.startsWith(prefix)) {
      const suffix = dep.slice(prefix.length);
      const candidate = join(frameworkLibDir, suffix);
      if (existsSync(candidate)) return candidate;
    }
  }

  const libSuffix = dep.includes('/lib/') ? dep.split('/lib/')[1] : null;
  if (libSuffix) {
    const candidate = join(frameworkLibDir, libSuffix);
    if (existsSync(candidate)) return candidate;
  }

  if (frameworkIndex.has(basename(dep))) {
    return frameworkIndex.get(basename(dep));
  }

  if (externalLibs.has(dep)) {
    return externalLibs.get(dep);
  }

  if (!existsSync(dep)) {
    return null;
  }

  const dest = join(frameworkLibDir, basename(dep));
  if (!existsSync(dest)) {
    mkdirSync(dirname(dest), { recursive: true });
    copyFileSync(dep, dest);
    try {
      chmodSync(dest, statSync(dep).mode);
    } catch {}
  }
  externalLibs.set(dep, dest);
  frameworkIndex.set(basename(dest), dest);
  return dest;
}

function installNameForConsumer(consumer, target) {
  if (consumer === sidecarPath) {
    return target;
  }
  const consumerDir = dirname(consumer);
  const rel = relative(consumerDir, target);
  return `@loader_path/${toPosix(rel)}`;
}

function listDependencies(file) {
  const output = run('otool', ['-L', file], { allowFailure: true });
  if (output.status !== 0) return [];
  return output.stdout
    .split(/\r?\n/u)
    .slice(1)
    .map(parseOtoolDependencyLine)
    .filter(Boolean);
}

function parseOtoolDependencyLine(line) {
  const trimmed = line.trim();
  const metadataIndex = trimmed.lastIndexOf(' (compatibility version ');
  return metadataIndex >= 0 ? trimmed.slice(0, metadataIndex) : null;
}

function setInstallId(file, id) {
  if (!file.endsWith('.dylib')) return;
  run('install_name_tool', ['-id', id, file], { allowFailure: true });
}

function listRpaths(file) {
  const output = run('otool', ['-l', file], { allowFailure: true });
  if (output.status !== 0) return [];
  const lines = output.stdout.split(/\r?\n/u);
  const rpaths = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (!lines[index].includes('LC_RPATH')) continue;
    for (let offset = index + 1; offset < Math.min(index + 5, lines.length); offset += 1) {
      const match = lines[offset].match(/^\s+path\s+(.+?)\s+\(offset/u);
      if (match) {
        rpaths.push(match[1].trim());
        break;
      }
    }
  }
  return rpaths;
}

function ensureRpath(file, rpath) {
  if (listRpaths(file).includes(rpath)) return;
  run('install_name_tool', ['-add_rpath', rpath, file], { allowFailure: false });
  if (!listRpaths(file).includes(rpath)) {
    throw new Error(`Failed to add required runtime search path ${rpath} to ${file}`);
  }
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
  return dep.startsWith('/opt/homebrew/')
    || dep.startsWith('/usr/local/')
    || isManagedNativeDependency(dep)
    || dep.startsWith('/Library/Frameworks/GStreamer.framework/')
    || dep.startsWith('@rpath/GStreamer.framework/')
    || dep.startsWith('@executable_path/../Frameworks/GStreamer.framework/')
    || dep.startsWith('@executable_path/../Resources/gstreamer/macos/GStreamer.framework/');
}

function adhocSign(files) {
  const uniqueFiles = Array.from(new Set(files.map(safeRealpath))).filter((file) => existsSync(file));
  run('xattr', ['-cr', sidecarPath], { allowFailure: true });
  run('xattr', ['-cr', frameworkDir], { allowFailure: true });
  for (const file of uniqueFiles.sort((a, b) => b.length - a.length)) {
    const args = ['--force', '--sign', '-', '--timestamp=none'];
    if (safeRealpath(file) === safeRealpath(sidecarPath) && existsSync(sidecarEntitlements)) {
      args.push('--entitlements', sidecarEntitlements);
    }
    args.push(file);
    run('codesign', args, { allowFailure: false });
  }
}

function copyDirResolved(src, dst) {
  mkdirSync(dst, { recursive: true });
  for (const entry of readdirSync(src, { withFileTypes: true })) {
    const source = join(src, entry.name);
    const destination = join(dst, entry.name);

    let resolvedSource;
    try {
      resolvedSource = safeRealpath(source);
      lstatSync(resolvedSource);
    } catch {
      continue;
    }

    let resolvedStats;
    try {
      resolvedStats = statSync(resolvedSource);
    } catch {
      continue;
    }

    if (resolvedStats.isDirectory()) {
      copyDirResolved(resolvedSource, destination);
      continue;
    }

    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(resolvedSource, destination);
    try {
      chmodSync(destination, resolvedStats.mode);
    } catch {}
  }
}

function createRelativeSymlinkOrCopy(target, destination) {
  removeIfExists(destination);
  mkdirSync(dirname(destination), { recursive: true });
  try {
    symlinkSync(target, destination);
  } catch {
    const source = resolve(dirname(destination), target);
    if (existsSync(source)) {
      if (statSync(source).isDirectory()) {
        copyDirResolved(source, destination);
      } else {
        copyFileSync(source, destination);
      }
    }
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
      } else if (entry.isFile()) {
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
    if ((mode & 0o111) === 0) return false;
    const info = run('file', ['-b', file], { allowFailure: true });
    return info.status === 0 && info.stdout.includes('Mach-O');
  } catch {
    return false;
  }
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

function removeIfExists(path) {
  if (!existsSync(path)) return;
  rmSync(path, { recursive: true, force: true });
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
