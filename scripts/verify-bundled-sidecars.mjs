#!/usr/bin/env node
import { existsSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const productName = 'Noland Connect';
const target = readTarget(process.argv.slice(2)) ?? defaultHostTarget();
const releaseDir = chooseTargetReleaseDir(target);
const bundleDir = join(releaseDir, 'bundle');

if (!target) {
  fail('Unable to determine target triple. Pass --target <triple>.');
}
if (!existsSync(bundleDir)) {
  fail(`Bundle directory not found: ${bundleDir}`);
}

if (target.includes('apple-darwin')) {
  verifyMacBundles(target, bundleDir);
  process.exit(0);
}

if (target.includes('linux')) {
  verifyLinuxBundles(target, bundleDir);
  process.exit(0);
}

if (target.includes('windows')) {
  verifyWindowsBundles(target, bundleDir);
  process.exit(0);
}

console.log(`No bundled sidecar verification required for ${target}`);

function verifyMacBundles(targetTriple, bundleRoot) {
  const appBundle = join(bundleRoot, 'macos', `${productName}.app`);
  if (!existsSync(appBundle)) {
    fail(`Could not locate macOS app bundle at ${appBundle}`);
  }

  verifyMacBundleTree(appBundle, targetTriple, 'macOS app bundle');

  const dmg = findFirstPath(bundleRoot, (path) => path.endsWith('.dmg'));
  if (!dmg) {
    fail(`Could not locate DMG bundle under ${bundleRoot}`);
  }

  withMountedDmg(dmg, productName, (mountedApp) => {
    verifyMacBundleTree(mountedApp, targetTriple, `DMG payload ${basename(dmg)}`);
  });

  console.log(`Verified bundled macOS sidecars/runtime/resources for ${targetTriple}`);
}

function verifyLinuxBundles(targetTriple, bundleRoot) {
  const appDir = findFirstPath(bundleRoot, (path) => path.endsWith('.AppDir'));
  if (!appDir) {
    fail(`Could not locate AppDir under ${bundleRoot}`);
  }
  verifyBundleTree(appDir, targetTriple, 'linux AppDir');

  const deb = findFirstPath(bundleRoot, (path) => path.endsWith('.deb'));
  if (deb) {
    withExtractedTemp('linux-deb-', (extractRoot) => {
      run('dpkg-deb', ['-x', deb, extractRoot]);
      verifyBundleTree(extractRoot, targetTriple, `deb package ${basename(deb)}`);
    });
  }

  const rpm = findFirstPath(bundleRoot, (path) => path.endsWith('.rpm'));
  if (rpm) {
    withExtractedTemp('linux-rpm-', (extractRoot) => {
      runShell(`rpm2cpio '${escapeForSingleQuotes(rpm)}' | cpio -idm --quiet`, extractRoot);
      verifyBundleTree(extractRoot, targetTriple, `rpm package ${basename(rpm)}`);
    });
  }

  console.log(`Verified bundled Linux sidecars/runtime for ${targetTriple}`);
}

function verifyWindowsBundles(targetTriple, bundleRoot) {
  const msi = findFirstPath(bundleRoot, (path) => path.endsWith('.msi'));
  if (msi) {
    withExtractedTemp('windows-msi-', (extractRoot) => {
      run('msiexec', ['/a', msi, '/qn', `TARGETDIR=${extractRoot}`]);
      verifyBundleTree(extractRoot, targetTriple, `MSI package ${basename(msi)}`);
    });
    console.log(`Verified bundled Windows sidecars/runtime for ${targetTriple}`);
    return;
  }

  verifyBundleTree(bundleRoot, targetTriple, 'Windows bundle output');
  console.log(`Verified bundled Windows sidecars/runtime/resources in bundle output for ${targetTriple}`);
}

function verifyMacBundleTree(root, targetTriple, label) {
  verifyRequiredSidecars(root, targetTriple, label);

  const frameworkFound = findFirstPath(root, (path) => basename(path) === 'GStreamer.framework');
  if (!frameworkFound) {
    fail(`Missing bundled GStreamer.framework in ${label}`);
  }

  verifyBundledMicReceiverSource(root, label);
}

function verifyBundleTree(root, targetTriple, label) {
  verifyRequiredSidecars(root, targetTriple, label);
  verifyRequiredRuntimeFiles(root, targetTriple, label);
  verifyBundledMicReceiverSource(root, label);
}

function verifyRequiredRuntimeFiles(root, targetTriple, label) {
  for (const candidates of requiredRuntimeFileCandidates(targetTriple)) {
    const found = findFirstPath(root, (path) => candidates.includes(basename(path)));
    if (!found) {
      fail(`Missing required bundled runtime file (${candidates.join(' or ')}) in ${label}`);
    }
  }
}

function verifyBundledMicReceiverSource(root, label) {
  const receiverDir = findFirstPath(root, (path) => basename(path) === 'vm-cloud-mic-agent' && existsSync(join(path, 'Cargo.toml')));
  if (!receiverDir) {
    fail(`Missing bundled vm-cloud-mic-agent source directory in ${label}`);
  }

  for (const relativePath of ['Cargo.toml', 'src/main.rs', 'src/receiver.rs']) {
    const candidate = join(receiverDir, relativePath);
    if (!existsSync(candidate)) {
      fail(`Missing bundled vm-cloud-mic-agent file '${relativePath}' in ${label}`);
    }
  }
}

function verifyRequiredSidecars(root, targetTriple, label) {
  for (const candidates of requiredSidecarCandidates(targetTriple)) {
    const found = findFirstPath(root, (path) => candidates.includes(basename(path)));
    if (!found) {
      fail(`Missing required bundled sidecar (${candidates.join(' or ')}) in ${label}`);
    }
  }
}

function requiredSidecarCandidates(targetTriple) {
  const windows = targetTriple.includes('windows');
  const suffix = windows ? '.exe' : '';
  const withTarget = (stem) => `${stem}-${targetTriple}${suffix}`;
  const plain = (stem) => `${stem}${suffix}`;

  const groups = [
    [withTarget('noland-mic-sender'), plain('noland-mic-sender')],
    [withTarget('gotatun'), plain('gotatun')],
    [withTarget('wg'), plain('wg')],
    [withTarget('ssh'), plain('ssh')],
    [withTarget('scp'), plain('scp')],
    [withTarget('ssh-keygen'), plain('ssh-keygen')],
  ];

  if (windows) {
    groups.push([withTarget('wireguard'), plain('wireguard')]);
  } else {
    groups.push([withTarget('wg-quick'), plain('wg-quick')]);
  }

  return groups;
}

function requiredRuntimeFileCandidates(targetTriple) {
  if (targetTriple.includes('windows')) {
    return [
      ['gstreamer-1.0-0.dll'],
      ['gst-plugin-scanner.exe'],
      ['libgstwasapi.dll', 'libgstwasapi2.dll'],
      ['libgstaudioconvert.dll'],
      ['libgstaudioresample.dll'],
      ['libgstopus.dll'],
      ['libgstrtp.dll'],
      ['libgstudp.dll'],
    ];
  }

  return [
    ['gst-plugin-scanner'],
    ['libgstreamer-1.0.so.0', 'libgstreamer-1.0.so'],
    ['libgstpipewire.so'],
    ['libgstaudioconvert.so'],
    ['libgstaudioresample.so'],
    ['libgstopus.so'],
    ['libgstrtp.so'],
    ['libgstudp.so'],
  ];
}

function chooseTargetReleaseDir(targetTriple) {
  const tripleDir = join(repoRoot, 'src-tauri', 'target', targetTriple, 'release');
  if (existsSync(tripleDir)) {
    return tripleDir;
  }
  return join(repoRoot, 'src-tauri', 'target', 'release');
}

function withMountedDmg(dmg, volumeName, fn) {
  const mountPoint = mkdtempSync(join(tmpdir(), 'noland-dmg-mount-'));
  try {
    run('hdiutil', ['attach', dmg, '-mountpoint', mountPoint, '-nobrowse', '-readonly']);
    fn(join(mountPoint, `${volumeName}.app`));
  } finally {
    runAllowFailure('hdiutil', ['detach', mountPoint]);
    rmSync(mountPoint, { recursive: true, force: true });
  }
}

function withExtractedTemp(prefix, fn) {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  try {
    fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function findFirstPath(root, predicate) {
  if (!existsSync(root)) {
    return null;
  }

  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    if (predicate(current)) {
      return current;
    }
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      if (predicate(full)) {
        return full;
      }
      if (entry.isDirectory()) {
        stack.push(full);
      }
    }
  }
  return null;
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    fail(`Command failed: ${command} ${args.join(' ')}`);
  }
}

function runAllowFailure(command, args) {
  spawnSync(command, args, {
    cwd: repoRoot,
    stdio: 'inherit',
  });
}

function runShell(command, cwd) {
  const result = spawnSync('sh', ['-c', command], {
    cwd,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    fail(`Command failed: ${command}`);
  }
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

function escapeForSingleQuotes(value) {
  return String(value).replace(/'/g, `"'"'`);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
