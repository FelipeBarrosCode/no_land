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
const frameworkRoot = join(frameworkDir, 'Versions', 'Current');
const frameworkLibDir = join(frameworkRoot, 'lib');
const frameworkLibexecDir = join(frameworkRoot, 'libexec');
const frameworkPluginDir = join(frameworkLibDir, 'gstreamer-1.0');
const frameworkPluginValidateDir = join(frameworkPluginDir, 'validate');
const frameworkShareValidateDir = join(frameworkRoot, 'share', 'gstreamer-1.0', 'validate');
const frameworkSourceCandidates = [
  process.env.NOLAND_GSTREAMER_FRAMEWORK?.trim(),
  '/Library/Frameworks/GStreamer.framework',
].filter(Boolean);
const homebrewPrefixCandidates = [
  process.env.NOLAND_GSTREAMER_HOMEBREW_PREFIX?.trim(),
  '/opt/homebrew/opt/gstreamer',
  '/usr/local/opt/gstreamer',
].filter(Boolean);
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

adhocSign([sidecarPath, ...frameworkFiles, ...libexecFiles, ...externalLibs.values()]);
console.log(`Patched macOS dev sidecar runtime: ${sidecarPath}`);

function prepareBundledFramework() {
  removeIfExists(frameworkDir);

  const sourceFramework = resolveFrameworkSource();
  if (sourceFramework) {
    copyDirResolved(sourceFramework, frameworkDir);
  } else {
    const prefix = resolveHomebrewPrefix();
    if (!prefix) {
      console.error('Unable to locate a macOS GStreamer runtime source. Install the official GStreamer.framework or ensure Homebrew gstreamer is installed for local development.');
      process.exit(1);
    }
    synthesizeFrameworkFromHomebrew(prefix, frameworkDir);
  }

  pruneBundledPlugins();
}

function resolveFrameworkSource() {
  for (const candidate of frameworkSourceCandidates) {
    if (!candidate) continue;
    const path = resolve(candidate);
    if (existsSync(join(path, 'Versions', 'Current', 'lib'))) {
      return path;
    }
  }
  return null;
}

function resolveHomebrewPrefix() {
  for (const candidate of homebrewPrefixCandidates) {
    if (!candidate) continue;
    const path = resolve(candidate);
    if (hasGstreamerLib(path)) {
      return path;
    }
  }

  const output = run('brew', ['--prefix', 'gstreamer'], { allowFailure: true });
  if (output.status === 0) {
    const path = output.stdout.trim();
    if (path && hasGstreamerLib(path)) {
      return path;
    }
  }

  return null;
}

function hasGstreamerLib(prefix) {
  return existsSync(join(prefix, 'lib', 'libgstreamer-1.0.0.dylib'))
    || existsSync(join(prefix, 'lib', 'libgstreamer-1.0.dylib'));
}

function synthesizeFrameworkFromHomebrew(prefix, bundledFramework) {
  const versionsDir = join(bundledFramework, 'Versions');
  const currentDir = join(versionsDir, 'Current');
  mkdirSync(currentDir, { recursive: true });

  copyDirResolved(join(prefix, 'lib'), join(currentDir, 'lib'));

  const libexec = join(prefix, 'libexec');
  if (existsSync(libexec)) {
    copyDirResolved(libexec, join(currentDir, 'libexec'));
  }

  const share = join(prefix, 'share');
  if (existsSync(share)) {
    copyDirResolved(share, join(currentDir, 'share'));
  }

  createRelativeSymlinkOrCopy('Versions/Current/lib', join(bundledFramework, 'lib'));
  createRelativeSymlinkOrCopy('Versions/Current/libexec', join(bundledFramework, 'libexec'));
  createRelativeSymlinkOrCopy('Versions/Current/share', join(bundledFramework, 'share'));
  createRelativeSymlinkOrCopy('Current', join(versionsDir, 'A'));
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

function shouldRewriteDependency(dep) {
  return dep.startsWith('/opt/homebrew/')
    || dep.startsWith('/usr/local/')
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
    run('codesign', ['--force', '--sign', '-', '--timestamp=none', file], { allowFailure: true });
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
