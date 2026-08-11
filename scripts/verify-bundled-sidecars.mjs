#!/usr/bin/env node
import { accessSync, constants, existsSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';

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
    const label = `DMG payload ${basename(dmg)}`;
    verifyMacBundleTree(mountedApp, targetTriple, label);
    verifyMacExecutableSmokeTests(mountedApp, targetTriple, label);
  });

  console.log(`Verified bundled macOS sidecars/runtime/resources for ${targetTriple}`);
}

function verifyLinuxBundles(targetTriple, bundleRoot) {
  const appDir = findFirstPath(bundleRoot, (path) => path.endsWith('.AppDir'));
  if (!appDir) {
    fail(`Could not locate AppDir under ${bundleRoot}`);
  }
  verifyBundleTree(appDir, targetTriple, 'linux AppDir');
  verifyLinuxExecutableSmokeTests(appDir, targetTriple, 'linux AppDir');

  const appImage = findFirstPath(bundleRoot, (path) => path.endsWith('.AppImage'));
  if (appImage) {
    withExtractedTemp('linux-appimage-', (extractRoot) => {
      run(appImage, ['--appimage-extract'], { cwd: extractRoot });
      const extractedAppDir = join(extractRoot, 'squashfs-root');
      verifyBundleTree(extractedAppDir, targetTriple, `AppImage ${basename(appImage)}`);
      verifyLinuxExecutableSmokeTests(extractedAppDir, targetTriple, `AppImage ${basename(appImage)}`);
    });
  }

  const deb = findFirstPath(bundleRoot, (path) => path.endsWith('.deb'));
  if (deb) {
    withExtractedTemp('linux-deb-', (extractRoot) => {
      run('dpkg-deb', ['-x', deb, extractRoot]);
      const label = `deb package ${basename(deb)}`;
      verifyBundleTree(extractRoot, targetTriple, label);
      verifyLinuxLinkage(extractRoot, targetTriple, label);
    });
  }

  const rpm = findFirstPath(bundleRoot, (path) => path.endsWith('.rpm'));
  if (rpm) {
    withExtractedTemp('linux-rpm-', (extractRoot) => {
      runShell(`rpm2cpio '${escapeForSingleQuotes(rpm)}' | cpio -idm --quiet`, extractRoot);
      const label = `rpm package ${basename(rpm)}`;
      verifyBundleTree(extractRoot, targetTriple, label);
      verifyLinuxLinkage(extractRoot, targetTriple, label);
    });
  }

  console.log(`Verified bundled Linux sidecars/runtime for ${targetTriple}`);
}

function verifyWindowsBundles(targetTriple, bundleRoot) {
  let verifiedInstaller = false;
  const msi = findFirstPath(bundleRoot, (path) => path.endsWith('.msi'));
  if (msi) {
    withExtractedTemp('windows-msi-', (extractRoot) => {
      run('msiexec', ['/a', msi, '/qn', `TARGETDIR=${extractRoot}`]);
      const label = `MSI package ${basename(msi)}`;
      verifyBundleTree(extractRoot, targetTriple, label);
      verifyWindowsExecutableSmokeTests(extractRoot, targetTriple, label);
    });
    console.log(`Verified bundled Windows MSI sidecars/runtime for ${targetTriple}`);
    verifiedInstaller = true;
  }

  const nsis = findFirstPath(bundleRoot, (path) => path.endsWith('-setup.exe'));
  if (nsis) {
    verifyWindowsNsisInstallation(targetTriple, nsis);
    console.log(`Verified installed Windows NSIS sidecars/runtime/resources for ${targetTriple}`);
    verifiedInstaller = true;
  }

  if (verifiedInstaller) {
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

function verifyMacExecutableSmokeTests(appBundle, targetTriple, label) {
  run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', appBundle]);

  const executable = findMacAppExecutable(appBundle);
  if (!executable) {
    fail(`Could not locate the primary macOS executable in ${label}`);
  }

  const ssh = findRequiredSidecar(appBundle, targetTriple, 'ssh');
  run(ssh, ['-V']);

  const linkageSeeds = [
    executable,
    ...['noland-net-helper', 'noland-mic-sender', 'ssh', 'scp', 'ssh-keygen']
      .map((stem) => findRequiredSidecar(appBundle, targetTriple, stem)),
    findFirstPath(appBundle, (path) => basename(path) === 'libgstreamer-1.0.dylib'),
  ].filter(Boolean);
  for (const path of linkageSeeds) {
    verifyMacLinkage(path, label);
  }

  launchAndRequireAlive(executable, [], {}, `${label} GUI`, 8_000);
}

function findMacAppExecutable(appBundle) {
  const macosDir = join(appBundle, 'Contents', 'MacOS');
  const expectedNames = new Set(['noland-connect', productName, productName.toLowerCase()]);
  return findFirstPath(
    macosDir,
    (path) => expectedNames.has(basename(path)) && isExecutableFile(path),
  );
}

function verifyMacLinkage(path, label) {
  const result = runCapture('otool', ['-L', path]);
  const invalid = result.stdout
    .split(/\r?\n/u)
    .slice(1)
    .map(parseOtoolDependencyLine)
    .filter(Boolean)
    .filter((dependency) => dependency.startsWith('/'))
    .filter((dependency) => !dependency.startsWith('/System/Library/') && !dependency.startsWith('/usr/lib/'));
  if (invalid.length > 0) {
    fail(`Unbundled absolute macOS dependencies in ${label}: ${path}\n${invalid.join('\n')}`);
  }
}

function parseOtoolDependencyLine(line) {
  const trimmed = line.trim();
  const metadataIndex = trimmed.lastIndexOf(' (compatibility version ');
  return metadataIndex >= 0 ? trimmed.slice(0, metadataIndex) : null;
}

function verifyLinuxExecutableSmokeTests(root, targetTriple, label) {
  const runtimeDir = findFirstPath(
    root,
    (path) => basename(path) === targetTriple && basename(dirname(path)) === 'ssh-runtime',
  );
  if (!runtimeDir || !readdirSync(runtimeDir).some((name) => name.includes('.so'))) {
    fail(`Missing bundled OpenSSH shared-library closure in ${label}`);
  }

  verifyLinuxLinkage(root, targetTriple, label);
  const sshEnv = linuxSshRuntimeEnv(root, targetTriple);
  const cleanEnv = cleanLinuxRuntimeEnv();
  const ssh = findRequiredSidecar(root, targetTriple, 'ssh');
  run(ssh, ['-V'], { env: sshEnv });

  const helper = findRequiredSidecar(root, targetTriple, 'noland-net-helper');
  withExtractedTemp('noland-helper-check-', (tempRoot) => {
    const configPath = join(tempRoot, 'wg.conf');
    writeFileSync(configPath, [
      '[Interface]',
      'PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
      'Address = 10.66.66.2/32',
      'MTU = 1280',
      '',
      '[Peer]',
      'PublicKey = AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=',
      'AllowedIPs = 10.66.66.1/32',
      'Endpoint = 127.0.0.1:51820',
      'PersistentKeepalive = 25',
      '',
    ].join('\n'));
    run(helper, ['check', '--config', configPath], { env: cleanEnv });
  });

  const appRun = join(root, 'AppRun');
  const executable = isExecutableFile(appRun) ? appRun : findLinuxAppExecutable(root);
  if (!executable) {
    fail(`Could not locate the Linux application executable in ${label}`);
  }
  launchAndRequireAlive(
    'dbus-run-session',
    ['--', 'xvfb-run', '-a', executable],
    cleanEnv,
    `${label} GUI`,
    8_000,
  );
}

function verifyLinuxLinkage(root, targetTriple, label) {
  const cleanEnv = cleanLinuxRuntimeEnv();
  const sshEnv = linuxSshRuntimeEnv(root, targetTriple);
  const appExecutable = findLinuxAppExecutable(root);
  const seeds = [
    appExecutable,
    ...['noland-net-helper', 'noland-mic-sender', 'ssh', 'scp', 'ssh-keygen']
      .map((stem) => findRequiredSidecar(root, targetTriple, stem)),
    findFirstPath(root, (path) => basename(path) === 'gst-plugin-scanner'),
    findFirstPath(root, (path) => basename(path) === 'libgstreamer-1.0.so.0'),
  ].filter(Boolean);

  for (const path of seeds) {
    const kind = runCapture('file', ['-b', path]);
    if (!kind.stdout.includes('ELF')) continue;
    const name = basename(path);
    const usesSshRuntime = /^(?:ssh|scp|ssh-keygen)(?:-|$)/u.test(name);
    const linkage = runCapture('ldd', [path], { env: usesSshRuntime ? sshEnv : cleanEnv, allowFailure: true });
    if (linkage.status !== 0 || /not found/u.test(linkage.stdout) || /not found/u.test(linkage.stderr)) {
      fail(`Unresolved Linux runtime dependencies in ${label}: ${path}\n${linkage.stdout}\n${linkage.stderr}`);
    }
  }

  verifyLinuxGstreamerRpaths(root, targetTriple, label, appExecutable, cleanEnv);
}

function verifyLinuxGstreamerRpaths(root, targetTriple, label, appExecutable, cleanEnv) {
  const gstreamerRoot = join(root, 'usr', 'lib', productName, 'binaries', 'gstreamer', targetTriple);
  if (!existsSync(gstreamerRoot)) {
    fail(`Linux GStreamer runtime is not installed at the Tauri resource path in ${label}: ${gstreamerRoot}`);
  }
  if (!appExecutable) {
    fail(`Could not locate the Linux application executable while verifying GStreamer RPATH in ${label}`);
  }

  const appDynamic = runCapture('readelf', ['-d', appExecutable]);
  // linuxdeploy copies direct dependencies into usr/lib and replaces the linked
  // RPATH with $ORIGIN/../lib. Debian/RPM bundles may retain our resource RPATH.
  // The clean-environment ldd checks below prove either layout stays in-package.
  const expectedResourceRpath = `$ORIGIN/../lib/${productName}/binaries/gstreamer/${targetTriple}/lib`;
  const expectedBundlerRpath = '$ORIGIN/../lib';
  const hasResourceRpath = appDynamic.stdout.includes('(RPATH)')
    && appDynamic.stdout.includes(expectedResourceRpath);
  const hasBundlerRpath = (appDynamic.stdout.includes('(RPATH)') || appDynamic.stdout.includes('(RUNPATH)'))
    && appDynamic.stdout.includes(expectedBundlerRpath);
  if (!hasResourceRpath && !hasBundlerRpath) {
    fail(`Linux application has neither the Noland resource RPATH nor Tauri's packaged-library RUNPATH in ${label}: ${expectedResourceRpath} or ${expectedBundlerRpath}\n${appDynamic.stdout}`);
  }

  const scanner = join(gstreamerRoot, 'libexec', 'gstreamer-1.0', 'gst-plugin-scanner');
  const scannerDynamic = runCapture('readelf', ['-d', scanner]);
  const scannerHasSearchPath = scannerDynamic.stdout.includes('(RPATH)')
    || scannerDynamic.stdout.includes('(RUNPATH)');
  if (!scannerHasSearchPath || !scannerDynamic.stdout.includes('$ORIGIN/../../lib')) {
    fail(`Bundled gst-plugin-scanner does not contain the required relative library search path in ${label}\n${scannerDynamic.stdout}`);
  }

  const plugin = findFirstPath(join(gstreamerRoot, 'lib', 'gstreamer-1.0'), (path) => basename(path) === 'libgstlibav.so');
  if (!plugin) {
    fail(`Could not locate bundled libgstlibav.so in ${label}`);
  }
  const pluginDynamic = runCapture('readelf', ['-d', plugin]);
  const pluginHasSearchPath = pluginDynamic.stdout.includes('(RPATH)')
    || pluginDynamic.stdout.includes('(RUNPATH)');
  if (!pluginHasSearchPath || !pluginDynamic.stdout.includes('$ORIGIN/..')) {
    fail(`Bundled GStreamer plugin does not contain the required relative library search path in ${label}: ${plugin}\n${pluginDynamic.stdout}`);
  }

  const linkage = runCapture('ldd', [appExecutable], { env: cleanEnv, allowFailure: true });
  const packagedLibraryRoots = [
    gstreamerRoot,
    join(root, 'usr', 'lib'),
  ].map((path) => path.split('\\').join('/'));
  for (const library of ['libgstreamer-1.0.so', 'libgstapp-1.0.so', 'libgstvideo-1.0.so', 'libcrypto.so']) {
    const line = linkage.stdout.split(/\r?\n/u).find((candidate) => candidate.includes(library));
    const normalizedLine = line?.split('\\').join('/') ?? '';
    if (!line || !packagedLibraryRoots.some((path) => normalizedLine.includes(path))) {
      fail(`Linux application resolved ${library} outside the extracted package in ${label}\nExpected one of: ${packagedLibraryRoots.join(', ')}\n${linkage.stdout}`);
    }
  }
}

function cleanLinuxRuntimeEnv() {
  const env = { ...process.env };
  for (const name of ['LD_LIBRARY_PATH', 'NOLAND_GSTREAMER_ROOT', 'GST_PLUGIN_PATH_1_0', 'GST_PLUGIN_SYSTEM_PATH_1_0', 'GST_PLUGIN_SCANNER_1_0']) {
    delete env[name];
  }
  return env;
}

function linuxSshRuntimeEnv(root, targetTriple) {
  const runtimeDir = findFirstPath(
    root,
    (path) => basename(path) === targetTriple && basename(dirname(path)) === 'ssh-runtime',
  );
  const env = cleanLinuxRuntimeEnv();
  if (runtimeDir) {
    env.LD_LIBRARY_PATH = runtimeDir;
  }
  return env;
}

function findLinuxAppExecutable(root) {
  return findFirstPath(root, (path) => {
    const name = basename(path).toLowerCase();
    return (name === 'noland-connect' || name === productName.toLowerCase()) && isExecutableFile(path);
  });
}

function findRequiredSidecar(root, targetTriple, stem) {
  const windows = targetTriple.includes('windows');
  const suffix = windows ? '.exe' : '';
  const names = [`${stem}-${targetTriple}${suffix}`, `${stem}${suffix}`];
  const found = findFirstPath(root, (path) => names.includes(basename(path)) && isExecutableFile(path));
  if (!found) {
    fail(`Could not locate ${stem} for executable smoke testing under ${root}`);
  }
  return found;
}

function isExecutableFile(path) {
  if (!path || !existsSync(path)) return false;
  try {
    if (!statSync(path).isFile()) return false;
    accessSync(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function launchAndRequireAlive(command, args, env, label, durationMs) {
  const child = spawn(command, args, {
    cwd: repoRoot,
    env: { ...process.env, ...env },
    stdio: 'ignore',
  });
  if (!child.pid) {
    fail(`Failed to launch ${label}: ${command}`);
  }

  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, durationMs);
  let alive = true;
  try {
    process.kill(child.pid, 0);
  } catch {
    alive = false;
  }
  if (alive) {
    child.kill('SIGTERM');
  }
  if (!alive) {
    fail(`${label} exited before the ${durationMs / 1000}-second package smoke test completed`);
  }
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

  verifyMicReceiverSourceDirectory(receiverDir, label);
}

function verifyMicReceiverSourceDirectory(receiverDir, label) {
  for (const relativePath of ['Cargo.toml', 'src/main.rs', 'src/receiver.rs']) {
    const candidate = join(receiverDir, relativePath);
    if (!existsSync(candidate)) {
      fail(`Missing bundled vm-cloud-mic-agent file '${relativePath}' in ${label}`);
    }
  }
}

function verifyWindowsNsisInstallation(targetTriple, nsisInstallerPath) {
  if (process.platform !== 'win32') {
    fail('Windows NSIS installation verification must run on a Windows host.');
  }

  withExtractedTemp('windows-nsis-install-', (installRoot) => {
    run(nsisInstallerPath, ['/S', `/D=${installRoot}`]);
    const label = `installed NSIS package ${basename(nsisInstallerPath)}`;
    verifyBundleTree(installRoot, targetTriple, label);
    verifyWindowsExecutableSmokeTests(installRoot, targetTriple, label);
  });
}

function verifyWindowsExecutableSmokeTests(root, targetTriple, label) {
  const sshNames = [
    `ssh-${targetTriple}.exe`,
    'ssh.exe',
  ];
  const ssh = findFirstPath(root, (path) => sshNames.includes(basename(path)));
  if (!ssh) {
    fail(`Could not locate bundled ssh.exe for smoke testing in ${label}`);
  }
  run(ssh, ['-V']);

  const helper = findRequiredSidecar(root, targetTriple, 'noland-net-helper');
  withExtractedTemp('noland windows helper ', (tempRoot) => {
    const configPath = join(tempRoot, 'tunnel config.conf');
    writeFileSync(configPath, [
      '[Interface]',
      'PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
      'Address = 10.66.66.2/32',
      'MTU = 1280',
      '',
      '[Peer]',
      'PublicKey = AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=',
      'AllowedIPs = 10.66.66.1/32',
      'Endpoint = 127.0.0.1:51820',
      'PersistentKeepalive = 25',
      '',
    ].join('\n'));
    run(helper, ['check', '--config', configPath]);
  });

  const expectedAppExecutableNames = new Set([
    'noland-connect.exe',
    `${productName}.exe`,
  ].map((name) => name.toLocaleLowerCase()));
  const appExecutable = findFirstPath(
    root,
    (path) => expectedAppExecutableNames.has(basename(path).toLocaleLowerCase()),
  );
  if (!appExecutable) {
    fail(`Could not locate the installed Noland executable for smoke testing in ${label}`);
  }

  const quotePowerShell = (value) => `'${String(value).replaceAll("'", "''")}'`;
  const script = [
    `$process = Start-Process -FilePath ${quotePowerShell(appExecutable)} -PassThru`,
    'Start-Sleep -Seconds 8',
    '$process.Refresh()',
    'if ($process.HasExited) { Write-Error "Noland exited during the Windows installer smoke test with code $($process.ExitCode)"; exit 1 }',
    'Stop-Process -Id $process.Id -Force',
  ].join('; ');
  run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script]);
}

function verifyRequiredSidecars(root, targetTriple, label) {
  for (const candidates of requiredSidecarCandidates(targetTriple)) {
    const found = findFirstPath(root, (path) => candidates.includes(basename(path)));
    if (!found) {
      fail(`Missing required bundled sidecar (${candidates.join(' or ')}) in ${label}`);
    }
    if (!targetTriple.includes('windows')) {
      try {
        accessSync(found, constants.X_OK);
      } catch {
        fail(`Bundled sidecar is not executable in ${label}: ${found}`);
      }
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
    [withTarget('noland-net-helper'), plain('noland-net-helper')],
    [withTarget('ssh'), plain('ssh')],
    [withTarget('scp'), plain('scp')],
    [withTarget('ssh-keygen'), plain('ssh-keygen')],
  ];

  return groups;
}

function requiredRuntimeFileCandidates(targetTriple) {
  if (targetTriple.includes('windows')) {
    if (targetTriple.includes('aarch64')) {
      return [
        ['wintun.dll', `wintun-${targetTriple}.dll`],
        ['wintun-LICENSE.txt'],
      ];
    }

    return [
      ['wintun.dll', `wintun-${targetTriple}.dll`],
      ['wintun-LICENSE.txt'],
      ['gstreamer-1.0-0.dll'],
      ['gst-plugin-scanner.exe'],
      ['gstwasapi.dll', 'libgstwasapi.dll', 'gstwasapi2.dll', 'libgstwasapi2.dll'],
      ['gstaudioconvert.dll', 'libgstaudioconvert.dll'],
      ['gstaudioresample.dll', 'libgstaudioresample.dll'],
      ['gstopus.dll', 'libgstopus.dll'],
      ['gstrtp.dll', 'libgstrtp.dll', 'gstrtpmanager.dll', 'libgstrtpmanager.dll'],
      ['gstudp.dll', 'libgstudp.dll'],
    ];
  }

  return [
    ['gst-plugin-scanner'],
    ['libgstreamer-1.0.so.0', 'libgstreamer-1.0.so'],
    ['libgstapp-1.0.so.0', 'libgstapp-1.0.so'],
    ['libgstvideo-1.0.so.0', 'libgstvideo-1.0.so'],
    ['libcrypto.so.3', 'libcrypto.so'],
    ['libgstautodetect.so'],
    ['libgstplayback.so'],
    ['libgstvideoconvertscale.so', 'libgstvideoconvert.so'],
    ['libgstvideoparsersbad.so'],
    ['libgstlibav.so'],
    ['libgstximagesink.so'],
    ['libgstwaylandsink.so'],
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

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? repoRoot,
    env: options.env ?? process.env,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    fail(`Command failed: ${command} ${args.join(' ')}`);
  }
}

function runCapture(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? repoRoot,
    env: options.env ?? process.env,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (!options.allowFailure && result.status !== 0) {
    fail(`Command failed: ${command} ${args.join(' ')}\n${result.stdout ?? ''}\n${result.stderr ?? ''}`);
  }
  return {
    status: result.status ?? -1,
    stdout: result.stdout ?? '',
    stderr: result.stderr ?? '',
  };
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
