#!/usr/bin/env node
import { accessSync, constants, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { verifyPackagedRemoteAgents } from './remote-agent-artifacts.mjs';
import { basename, dirname, join, normalize, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';
import { cleanWindowsEnvironment, readPeRuntime, smokeTestOpenSsh, verifyOpenSshRuntime } from './windows-openssh.mjs';

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
  if (appDir) {
    verifyBundleTree(appDir, targetTriple, 'linux AppDir');
    verifyLinuxExecutableSmokeTests(appDir, targetTriple, 'linux AppDir');
  }

  const appImage = findFirstPath(bundleRoot, (path) => path.endsWith('.AppImage'));
  if (appImage) {
    if (!isExecutableFile(appImage)) {
      fail(`AppImage is not executable: ${appImage}`);
    }
    withExtractedTemp('linux-appimage-', (extractRoot) => {
      run(appImage, ['--appimage-extract'], { cwd: extractRoot });
      const extractedAppDir = join(extractRoot, 'squashfs-root');
      const appRun = join(extractedAppDir, 'AppRun');
      if (!isExecutableFile(appRun)) {
        fail(`AppImage ${basename(appImage)} contains a non-executable AppRun entry point`);
      }
      const dirIcon = join(extractedAppDir, '.DirIcon');
      if (!existsSync(dirIcon) || !statSync(dirIcon).isFile() || statSync(dirIcon).size === 0) {
        fail(`AppImage ${basename(appImage)} is missing a usable root .DirIcon`);
      }
      verifyBundleTree(extractedAppDir, targetTriple, `AppImage ${basename(appImage)}`);
      verifyLinuxExecutableSmokeTests(extractedAppDir, targetTriple, `AppImage ${basename(appImage)}`);
    });
  }

  let verifiedNativePackage = false;
  const deb = findFirstPath(bundleRoot, (path) => path.endsWith('.deb'));
  if (deb) {
    verifiedNativePackage = true;
    withExtractedTemp('linux-deb-', (extractRoot) => {
      run('dpkg-deb', ['-x', deb, extractRoot]);
      const label = `deb package ${basename(deb)}`;
      verifyBundleTree(extractRoot, targetTriple, label);
      verifyDebSystemRuntimeDependencies(deb, label);
      verifyLinuxLinkage(extractRoot, targetTriple, label);
    });
  }

  const rpm = findFirstPath(bundleRoot, (path) => path.endsWith('.rpm'));
  if (rpm) {
    verifiedNativePackage = true;
    withExtractedTemp('linux-rpm-', (extractRoot) => {
      runShell(`rpm2cpio '${escapeForSingleQuotes(rpm)}' | cpio -idm --quiet`, extractRoot);
      const label = `rpm package ${basename(rpm)}`;
      verifyBundleTree(extractRoot, targetTriple, label);
      verifyLinuxLinkage(extractRoot, targetTriple, label);
    });
  }

  if (!appDir && !appImage && !verifiedNativePackage) {
    fail(`Could not locate any Linux bundle output under ${bundleRoot}`);
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
  verifyBundledNetworkAgentSource(root, label);
}

function verifyBundleTree(root, targetTriple, label) {
  verifyRequiredSidecars(root, targetTriple, label);
  verifyRequiredRuntimeFiles(root, targetTriple, label);
  verifyBundledMicReceiverSource(root, label);
  verifyBundledNetworkAgentSource(root, label);
}

function verifyMacExecutableSmokeTests(appBundle, targetTriple, label) {
  run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', appBundle]);

  const executable = findMacAppExecutable(appBundle);
  if (!executable) {
    fail(`Could not locate the primary macOS executable in ${label}`);
  }

  const cleanEnv = cleanUnixRuntimeEnv();
  const framework = findFirstPath(appBundle, (path) => basename(path) === 'GStreamer.framework');
  if (!framework) fail(`Could not locate bundled GStreamer.framework in ${label}`);
  const frameworkCurrent = join(framework, 'Versions', 'Current');
  const frameworkLib = join(frameworkCurrent, 'lib');
  cleanEnv.DYLD_FRAMEWORK_PATH = dirname(framework);
  cleanEnv.DYLD_FALLBACK_LIBRARY_PATH = frameworkLib;
  cleanEnv.GST_PLUGIN_SYSTEM_PATH_1_0 = join(frameworkLib, 'gstreamer-1.0');
  cleanEnv.GST_PLUGIN_PATH_1_0 = cleanEnv.GST_PLUGIN_SYSTEM_PATH_1_0;
  const pluginScanner = join(frameworkCurrent, 'libexec', 'gstreamer-1.0', 'gst-plugin-scanner');
  if (existsSync(pluginScanner)) cleanEnv.GST_PLUGIN_SCANNER_1_0 = pluginScanner;
  smokeTestUnixOpenSsh(appBundle, targetTriple, label, cleanEnv);
  smokeTestNetHelper(appBundle, targetTriple, label, cleanEnv);
  smokeTestMicSender(appBundle, targetTriple, label, cleanEnv);
  verifyAllMacNativeFiles(appBundle, label);
  launchAndRequireFrontendReady(executable, [], cleanEnv, `${label} GUI`);
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

function verifyAllMacNativeFiles(root, label) {
  let checked = 0;
  for (const path of collectFiles(root).filter(isPotentialUnixNativeFile)) {
    const kind = runCapture('file', ['-b', path], { allowFailure: true });
    if (kind.status !== 0 || !kind.stdout.includes('Mach-O')) continue;
    verifyMacLinkage(path, label);
    run('codesign', ['--verify', '--strict', '--verbose=2', path]);
    checked += 1;
  }
  if (checked === 0) fail(`No Mach-O files were dependency-checked in ${label}`);
  console.log(`Verified linkage and signatures for ${checked} Mach-O files in ${label}`);
}

function parseOtoolDependencyLine(line) {
  const trimmed = line.trim();
  const metadataIndex = trimmed.lastIndexOf(' (compatibility version ');
  return metadataIndex >= 0 ? trimmed.slice(0, metadataIndex) : null;
}

function verifyLinuxExecutableSmokeTests(root, targetTriple, label) {
  verifyLinuxLinkage(root, targetTriple, label);
  const cleanEnv = cleanLinuxRuntimeEnv();
  smokeTestUnixOpenSsh(root, targetTriple, label, cleanEnv);
  smokeTestNetHelper(root, targetTriple, label, cleanEnv);
  smokeTestMicSender(root, targetTriple, label, cleanEnv);

  const appRun = join(root, 'AppRun');
  const executable = isExecutableFile(appRun) ? appRun : findLinuxAppExecutable(root);
  if (!executable) {
    fail(`Could not locate the Linux application executable in ${label}`);
  }
  launchAndRequireFrontendReady(
    'dbus-run-session',
    ['--', 'xvfb-run', '-a', executable],
    cleanEnv,
    `${label} GUI`,
  );
}

function verifyLinuxLinkage(root, targetTriple, label) {
  const cleanEnv = cleanLinuxRuntimeEnv();
  const appExecutable = findLinuxAppExecutable(root);
  const isPortableAppImageTree = basename(root).endsWith('.AppDir')
    || existsSync(join(root, 'AppRun'))
    || existsSync(join(root, '.DirIcon'));
  let checked = 0;
  for (const path of collectFiles(root).filter(isPotentialUnixNativeFile)) {
    const kind = runCapture('file', ['-b', path], { allowFailure: true });
    if (!kind.stdout.includes('ELF')) continue;
    const name = basename(path);
    const wrappedTool = linuxWrappedToolNameForSidecar(name);
    if (wrappedTool) {
      verifyLinuxSystemToolWrapper(path, wrappedTool, label);
      continue;
    }
    const linkage = runCapture('ldd', [path], { env: cleanEnv, allowFailure: true });
    if (linkage.status !== 0 || /not found/u.test(linkage.stdout) || /not found/u.test(linkage.stderr)) {
      fail(`Unresolved Linux runtime dependencies in ${label}: ${path}\n${linkage.stdout}\n${linkage.stderr}`);
    }
    checked += 1;
  }
  if (checked === 0 || !appExecutable) fail(`No packaged Linux ELF files were dependency-checked in ${label}`);
  console.log(`Verified runtime linkage for ${checked} ELF files in ${label}`);

  // AppImage deliberately carries its desktop runtime and launches through
  // AppRun. Native deb/rpm packages must continue using one coherent distro
  // GTK/WebKit/GStreamer stack to avoid host/bundled ABI collisions.
  if (!isPortableAppImageTree) {
    verifyLinuxSystemGstreamer(root, label, appExecutable, cleanEnv);
    verifyNoBundledLinuxDesktopPlatformLibraries(root, label);
  }
}

function verifyLinuxSystemGstreamer(root, label, appExecutable, cleanEnv) {
  const bundledGstreamerArtifact = findFirstPath(root, (path) => {
    const normalizedPath = normalize(path).split('\\').join('/');
    const name = basename(path);
    return normalizedPath.includes('/binaries/gstreamer/')
      || /^libgstreamer-1\.0\.so/u.test(name)
      || name === 'gst-plugin-scanner';
  });
  if (bundledGstreamerArtifact) {
    fail(`Linux package contains a bundled GStreamer runtime artifact in ${label}: ${bundledGstreamerArtifact}`);
  }
  if (!appExecutable) {
    fail(`Could not locate the Linux application executable while verifying GStreamer linkage in ${label}`);
  }

  const appDynamic = runCapture('readelf', ['-d', appExecutable]);
  const dynamicSearchPaths = appDynamic.stdout
    .split(/\r?\n/u)
    .filter((line) => line.includes('(RPATH)') || line.includes('(RUNPATH)'))
    .join('\n');
  if (/gstreamer/iu.test(dynamicSearchPaths)) {
    fail(`Linux application contains a GStreamer-specific RPATH/RUNPATH in ${label}\n${dynamicSearchPaths}`);
  }

  const linkage = runCapture('ldd', [appExecutable], { env: cleanEnv, allowFailure: true });
  const normalizedPackageRoot = normalize(root).split('\\').join('/');
  for (const library of ['libgstreamer-1.0.so', 'libgstapp-1.0.so', 'libgstvideo-1.0.so']) {
    const line = linkage.stdout.split(/\r?\n/u).find((candidate) => candidate.includes(library));
    const resolvedPath = line?.match(/=>\s+(.+)\s+\(0x[0-9a-f]+\)$/iu)?.[1]?.trim();
    const normalizedResolvedPath = resolvedPath
      ? normalize(resolvedPath).split('\\').join('/')
      : '';
    if (!line || !resolvedPath) {
      fail(`Linux application did not resolve required system library ${library} in ${label}\n${linkage.stdout}`);
    }
    if (normalizedResolvedPath === normalizedPackageRoot || normalizedResolvedPath.startsWith(`${normalizedPackageRoot}/`)) {
      fail(`Linux application resolved ${library} from inside the package in ${label}: ${resolvedPath}`);
    }
  }
}

function verifyNoBundledLinuxDesktopPlatformLibraries(root, label) {
  const offenders = [];
  const packageLibraryDirs = [
    join(root, 'usr', 'lib'),
    join(root, 'usr', 'lib64'),
  ];

  // AppImage/linuxdeploy-style usr/lib injection is the most dangerous place for
  // these libraries: AppRun adds it to the dynamic loader path, then host GIO
  // modules can accidentally bind against bundled GLib/libcurl/nghttp2 versions.
  // Native .deb/.rpm builds should depend on distro GTK/WebKit/GIO instead.
  for (const libDir of packageLibraryDirs) {
    collectForbiddenLinuxLibraries(libDir, offenders, linuxDistroOwnedRuntimeLibraryPatterns(), { recursive: false });
  }

  if (offenders.length > 0) {
    fail(`Linux bundle contains distro-owned desktop/system libraries in ${label}. These can break Ubuntu/Zorin LTS with GLib/GIO/libcurl symbol lookup errors; use system GTK/WebKit/AT-SPI/GLib/curl/nghttp2 instead.\n${offenders.join('\n')}`);
  }
}

function collectForbiddenLinuxLibraries(libDir, offenders, patterns, { recursive = false } = {}) {
  if (!existsSync(libDir)) return;
  for (const entry of readdirSync(libDir, { withFileTypes: true })) {
    const path = join(libDir, entry.name);
    if (entry.isDirectory()) {
      if (recursive) collectForbiddenLinuxLibraries(path, offenders, patterns, { recursive });
      continue;
    }
    if ((entry.isFile() || entry.isSymbolicLink()) && patterns.some((pattern) => pattern.test(entry.name))) {
      offenders.push(path);
    }
  }
}

function linuxDistroOwnedRuntimeLibraryPatterns() {
  return [
    /^libgstreamer-1\.0\.so/u,
    /^libgst/u,
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
    /^libcurl/u,
    /^libnghttp2/u,
    /^libpsl/u,
    /^libssh2/u,
    /^libgnutls/u,
    /^libudev\.so/u,
    /^libgudev/u,
    /^libva/u,
    /^libvdpau/u,
    /^libdrm/u,
    /^libgbm/u,
    /^libGL\.so/u,
    /^libGLX/u,
    /^libEGL/u,
    /^libGLESv/u,
    /^libOpenGL\.so/u,
    /^libglapi/u,
    /^libopengl\.so/u,
    /^libpipewire/u,
    /^libspa/u,
    /^libpulse/u,
    /^libasound/u,
    /^libjack/u,
    /^libxkbcommon-x11/u,
    /^libxshmfence/u,
  ];
}

function cleanLinuxRuntimeEnv() {
  const env = cleanUnixRuntimeEnv();
  return env;
}

function cleanUnixRuntimeEnv() {
  const env = { ...process.env };
  for (const name of [
    'LD_LIBRARY_PATH', 'DYLD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH', 'DYLD_FRAMEWORK_PATH',
    'NOLAND_GSTREAMER_ROOT', 'GST_PLUGIN_PATH_1_0', 'GST_PLUGIN_SYSTEM_PATH_1_0',
    'GST_PLUGIN_SCANNER_1_0', 'GST_REGISTRY_1_0', 'GI_TYPELIB_PATH',
  ]) {
    delete env[name];
  }
  return env;
}

function smokeTestUnixOpenSsh(root, targetTriple, label, env) {
  withExtractedTemp('noland-openssh-check-', (tempRoot) => {
    const ssh = findRequiredSidecar(root, targetTriple, 'ssh');
    const scp = findRequiredSidecar(root, targetTriple, 'scp');
    const keygen = findRequiredSidecar(root, targetTriple, 'ssh-keygen');
    if (targetTriple.includes('linux')) {
      verifyLinuxSystemToolWrapper(ssh, 'ssh', label);
      verifyLinuxSystemToolWrapper(scp, 'scp', label);
      verifyLinuxSystemToolWrapper(keygen, 'ssh-keygen', label);
    }
    run(ssh, ['-V'], { cwd: tempRoot, env });
    const scpUsage = runCapture(scp, [], { cwd: tempRoot, env, allowFailure: true });
    if (scpUsage.status !== 1 || !/usage:/iu.test(`${scpUsage.stdout}\n${scpUsage.stderr}`)) {
      fail(`Packaged scp did not execute its usage path in ${label}\n${scpUsage.stdout}\n${scpUsage.stderr}`);
    }
    const key = join(tempRoot, 'test key');
    run(keygen, ['-t', 'ed25519', '-f', key, '-N', '', '-C', 'package-test'], { cwd: tempRoot, env });
    const derived = runCapture(keygen, ['-y', '-f', key], { cwd: tempRoot, env }).stdout.trim();
    const stored = readFileSync(`${key}.pub`, 'utf8').trim().split(/\s+/u).slice(0, 2).join(' ');
    if (derived !== stored) fail(`Packaged ssh-keygen public-key round trip failed in ${label}`);
  });
}

function smokeTestNetHelper(root, targetTriple, label, env) {
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
    run(helper, ['check', '--config', configPath], { cwd: tempRoot, env });
  });
}

function smokeTestMicSender(root, targetTriple, label, env, { exercisePipeline = true } = {}) {
  const micSender = findRequiredSidecar(root, targetTriple, 'noland-mic-sender');
  withExtractedTemp('noland-mic-check-', (tempRoot) => {
    const runtimeEnv = { ...env, GST_REGISTRY_FORK: 'no', GST_REGISTRY_1_0: join(tempRoot, 'registry.bin') };
    const requests = exercisePipeline ? [
      { id: 'status-before', command: 'getStatus' },
      { id: 'start', command: 'startSession', config: {
        sessionId: 'package-test', host: '127.0.0.1', rtpPort: 49152, source: 'sine',
      } },
      { id: 'metrics', command: 'getMetrics' },
      { id: 'stop', command: 'stopSession' },
      { id: 'shutdown', command: 'shutdown' },
    ] : [
      { id: 'status', command: 'getStatus' },
      { id: 'shutdown', command: 'shutdown' },
    ];
    const result = spawnSync(micSender, ['daemon'], {
      cwd: tempRoot,
      env: runtimeEnv,
      encoding: 'utf8',
      timeout: 30_000,
      windowsHide: true,
      input: `${requests.map((request) => JSON.stringify(request)).join('\n')}\n`,
    });
    let responses = [];
    try {
      responses = (result.stdout || '').split(/\r?\n/u).filter(Boolean).map((line) => JSON.parse(line));
    } catch (error) {
      fail(`Packaged microphone sidecar emitted invalid JSON in ${label}: ${error}\n${result.stdout || ''}`);
    }
    if (result.error || result.status !== 0 || requests.some((request) =>
      !responses.some((response) => response.type === 'response' && response.id === request.id && response.ok))) {
      fail(`Packaged microphone pipeline failed in ${label}: ${result.error?.message || ''}\n${result.stdout || ''}\n${result.stderr || ''}`);
    }
  });
}

function linuxWrappedToolNameForSidecar(name) {
  if (/^ssh-keygen(?:-|$)/u.test(name)) return 'ssh-keygen';
  if (/^ssh(?:-|$)/u.test(name)) return 'ssh';
  if (/^scp(?:-|$)/u.test(name)) return 'scp';
  return null;
}

function verifyLinuxSystemToolWrapper(path, expectedTool, label) {
  const content = runCapture('sed', ['-n', '1,3p', path], { allowFailure: true });
  if (content.status !== 0 || !content.stdout.includes(`exec /usr/bin/${expectedTool}`)) {
    fail(`Linux ${expectedTool} sidecar must be a system wrapper in ${label}, not a copied build-host binary: ${path}`);
  }
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

function launchAndRequireFrontendReady(command, args, env, label) {
  withExtractedTemp('noland-app-smoke-', (tempRoot) => {
    const home = join(tempRoot, 'home');
    const dataHome = join(tempRoot, 'data');
    const configHome = join(tempRoot, 'config');
    const cacheHome = join(tempRoot, 'cache');
    for (const path of [home, dataHome, configHome, cacheHome]) mkdirSync(path, { recursive: true });

    for (const attempt of ['first-run', 'relaunch']) {
      const readyFile = join(tempRoot, `frontend-ready-${attempt}.json`);
      const runtimeEnv = {
        ...process.env,
        ...env,
        HOME: home,
        XDG_DATA_HOME: dataHome,
        XDG_CONFIG_HOME: configHome,
        XDG_CACHE_HOME: cacheHome,
        NOLAND_UI_READY_FILE: readyFile,
      };
      const child = spawn(command, args, { cwd: repoRoot, env: runtimeEnv, stdio: 'ignore' });
      if (!child.pid) fail(`Failed to launch ${label} during ${attempt}: ${command}`);

      let marker = null;
      const deadline = Date.now() + 40_000;
      while (Date.now() < deadline) {
        if (!processIsAlive(child.pid)) {
          fail(`${label} exited before frontend readiness during ${attempt}`);
        }
        if (existsSync(readyFile)) {
          try {
            marker = JSON.parse(readFileSync(readyFile, 'utf8'));
          } catch {
            marker = null;
          }
          if (marker?.ready === true && Number.isInteger(marker.pid) && marker.pid > 0 && marker.route) break;
        }
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 250);
      }

      if (!marker?.ready || !marker.route || !processIsAlive(marker.pid)) {
        const startupLog = join(tmpdir(), 'noland-connect-startup.log');
        const diagnostics = existsSync(startupLog) ? readFileSync(startupLog, 'utf8') : 'No startup diagnostic log was produced.';
        terminateProcess(child.pid, marker?.pid);
        fail(`${label} did not render a working frontend during ${attempt}\n${diagnostics}`);
      }

      // Keep the rendered app alive briefly to catch post-initialization crashes.
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 2_000);
      if (!processIsAlive(marker.pid)) {
        terminateProcess(child.pid, marker.pid);
        fail(`${label} crashed immediately after frontend readiness during ${attempt}`);
      }
      terminateProcess(child.pid, marker.pid);
    }
  });
}

function processIsAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function terminateProcess(launcherPid, appPid) {
  for (const pid of new Set([appPid, launcherPid].filter((value) => Number.isInteger(value) && value > 0))) {
    try { process.kill(pid, 'SIGTERM'); } catch { /* already exited */ }
  }
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 1_000);
  if (process.platform !== 'win32') {
    spawnSync('pkill', ['-TERM', '-P', String(launcherPid)], { stdio: 'ignore' });
  }
  for (const pid of new Set([appPid, launcherPid].filter((value) => Number.isInteger(value) && value > 0))) {
    if (!processIsAlive(pid)) continue;
    try { process.kill(pid, 'SIGKILL'); } catch { /* already exited */ }
  }
}

function verifyRequiredRuntimeFiles(root, targetTriple, label) {
  if (targetTriple.includes('windows')) {
    const runtime = findFirstPath(root, (path) => basename(path) === targetTriple
      && basename(dirname(path)) === 'ssh-runtime');
    if (!runtime) fail(`Missing private OpenSSH distribution in ${label}`);
    verifyOpenSshRuntime(runtime, targetTriple);
  }
  for (const candidates of requiredRuntimeFileCandidates(targetTriple)) {
    const found = findFirstPath(root, (path) => candidates.includes(basename(path)));
    if (!found) {
      fail(`Missing required bundled runtime file (${candidates.join(' or ')}) in ${label}`);
    }
  }
}

function verifyDebSystemRuntimeDependencies(deb, label) {
  const depends = runCapture('dpkg-deb', ['-f', deb, 'Depends']).stdout.trim();
  const declaredPackages = new Set(
    depends
      .split(',')
      .flatMap((group) => group.split('|'))
      .map((entry) => entry.trim().split(/[\s(]/u)[0].split(':')[0])
      .filter(Boolean),
  );
  const requiredPackages = [
    'libgstreamer1.0-0',
    'gstreamer1.0-plugins-base',
    'gstreamer1.0-plugins-good',
    'gstreamer1.0-plugins-bad',
    'gstreamer1.0-gl',
    'gstreamer1.0-libav',
    'gstreamer1.0-pipewire',
    'gstreamer1.0-x',
  ];
  const missing = requiredPackages.filter((packageName) => !declaredPackages.has(packageName));
  if (missing.length > 0) {
    fail(`Linux package does not declare required system runtime dependencies in ${label}: ${missing.join(', ')}\nDepends: ${depends}`);
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

function verifyBundledNetworkAgentSource(root, label) {
  verifyBundledRemoteAgents(root, label);
  const agentDir = findFirstPath(
    root,
    (path) => basename(path) === 'network-agent' && existsSync(join(path, 'Cargo.toml')),
  );
  if (!agentDir) {
    fail(`Missing bundled network-agent source directory in ${label}`);
  }

  for (const relativePath of ['Cargo.toml', 'Cargo.lock', 'src/main.rs']) {
    if (!existsSync(join(agentDir, relativePath))) {
      fail(`Missing bundled network-agent file '${relativePath}' in ${label}`);
    }
  }

  const contractsDir = join(dirname(agentDir), 'network-contracts');
  for (const relativePath of ['Cargo.toml', 'Cargo.lock', 'src/lib.rs']) {
    if (!existsSync(join(contractsDir, relativePath))) {
      fail(`Missing bundled network-contracts sibling file '${relativePath}' in ${label}`);
    }
  }
}

function verifyBundledRemoteAgents(root, label) {
  const source = findFirstPath(root, (path) => basename(path) === 'state-agent' && existsSync(join(path, 'prebuilt')));
  if (!source) fail(`Missing prebuilt remote agents in ${label}`);
  verifyPackagedRemoteAgents(source);
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
  verifyAllWindowsPeDependencies(root, targetTriple, label);
  const runtime = findFirstPath(root, (path) => basename(path) === targetTriple
    && basename(dirname(path)) === 'ssh-runtime');
  if (!runtime) fail(`Missing private OpenSSH distribution in ${label}`);
  withExtractedTemp('noland windows ssh ', (tempRoot) => smokeTestOpenSsh(runtime, tempRoot));

  const helper = findRequiredSidecar(root, targetTriple, 'noland-net-helper');
  smokeTestNetHelper(root, targetTriple, label, cleanWindowsEnvironment());
  const wintun = findFirstPath(root, (path) => /^wintun(?:-.+)?\.dll$/iu.test(basename(path)));
  if (!wintun) fail(`Could not locate packaged Wintun DLL for executable probing in ${label}`);
  run(helper, ['probe-wintun', '--wintun', wintun], { env: cleanWindowsEnvironment() });

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

  const appDirectory = normalize(resolve(dirname(appExecutable))).toLocaleLowerCase();
  const micSender = findRequiredSidecar(root, targetTriple, 'noland-mic-sender');
  const micEnv = cleanWindowsEnvironment();
  if (!targetTriple.includes('aarch64')) {
    const gstRoot = findFirstPath(root, (path) => basename(path) === targetTriple
      && basename(dirname(path)) === 'gstreamer');
    if (!gstRoot) fail(`Missing private GStreamer runtime in ${label}`);
    micEnv.PATH = `${join(gstRoot, 'bin')};${micEnv.PATH}`;
    micEnv.GST_PLUGIN_SYSTEM_PATH_1_0 = join(gstRoot, 'lib', 'gstreamer-1.0');
    micEnv.GST_PLUGIN_PATH_1_0 = micEnv.GST_PLUGIN_SYSTEM_PATH_1_0;
  }
  smokeTestMicSender(root, targetTriple, label, micEnv, { exercisePipeline: !targetTriple.includes('aarch64') });
  for (const [name, sidecar] of [
    ['noland-net-helper', helper],
    ['noland-mic-sender', micSender],
  ]) {
    const sidecarDirectory = normalize(resolve(dirname(sidecar))).toLocaleLowerCase();
    if (sidecarDirectory !== appDirectory) {
      fail(
        `${name} must be installed beside the Noland executable for the production runtime trust check in ${label}: app=${appExecutable}, sidecar=${sidecar}`,
      );
    }
  }

  withExtractedTemp('noland windows app ', (tempRoot) => {
    const quotePowerShell = (value) => `'${String(value).replaceAll("'", "''")}'`;
    const readyFile = join(tempRoot, 'frontend-ready.json');
    const startupLog = join(tmpdir(), 'noland-connect-startup.log');
    const appData = join(tempRoot, 'appdata');
    const localAppData = join(tempRoot, 'localappdata');
    const script = [
      '$ErrorActionPreference = "Stop"',
      `New-Item -ItemType Directory -Force -Path ${quotePowerShell(appData)}, ${quotePowerShell(localAppData)} | Out-Null`,
      'function Invoke-NolandLaunchSmoke([string]$Attempt) {',
      `  Remove-Item -LiteralPath ${quotePowerShell(readyFile)} -Force -ErrorAction SilentlyContinue`,
      `  $process = Start-Process -FilePath ${quotePowerShell(appExecutable)} -PassThru`,
      '  try {',
      '    $deadline = [DateTime]::UtcNow.AddSeconds(40)',
      '    $windowReady = $false',
      '    $frontendReady = $false',
      '    while ([DateTime]::UtcNow -lt $deadline) {',
      '      Start-Sleep -Milliseconds 250',
      '      $process.Refresh()',
      '      if ($process.HasExited) { throw "Noland exited during $Attempt launch with code $($process.ExitCode)" }',
      '      $windowReady = $process.MainWindowHandle -ne 0 -and $process.MainWindowTitle -eq "Noland Connect"',
      `      $frontendReady = Test-Path -LiteralPath ${quotePowerShell(readyFile)} -PathType Leaf`,
      '      if ($windowReady -and $frontendReady) { break }',
      '    }',
      '    if (-not $windowReady) { throw "Noland did not create its visible main window during $Attempt launch (title: $($process.MainWindowTitle))" }',
      '    if (-not $frontendReady) { throw "Noland frontend did not finish rendering during $Attempt launch" }',
      `    $ready = Get-Content -LiteralPath ${quotePowerShell(readyFile)} -Raw | ConvertFrom-Json`,
      '    if ($ready.ready -ne $true -or $ready.pid -ne $process.Id -or [string]::IsNullOrWhiteSpace($ready.route)) { throw "Invalid frontend readiness marker during $Attempt launch" }',
      '    if (-not $process.CloseMainWindow()) { throw "Noland main window rejected a normal close during $Attempt launch" }',
      '    if (-not $process.WaitForExit(10000)) { throw "Noland did not exit cleanly after $Attempt launch" }',
      '  } finally {',
      '    $process.Refresh()',
      '    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }',
      '  }',
      '}',
      'try {',
      '  Invoke-NolandLaunchSmoke "first-run"',
      '  Invoke-NolandLaunchSmoke "relaunch"',
      '} catch {',
      `  if (Test-Path -LiteralPath ${quotePowerShell(startupLog)}) { Write-Host '--- Noland startup diagnostics ---'; Get-Content -LiteralPath ${quotePowerShell(startupLog)} -Tail 200 }`,
      '  throw',
      '}',
    ].join('\n');
    const env = cleanWindowsEnvironment();
    env.NOLAND_UI_READY_FILE = readyFile;
    env.APPDATA = appData;
    env.LOCALAPPDATA = localAppData;
    run(join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe'),
      ['-NoProfile', '-NonInteractive', '-Command', script], { env });
  });
}

function verifyAllWindowsPeDependencies(root, targetTriple, label) {
  const expectedMachine = targetTriple.startsWith('aarch64-') ? 0xaa64 : 0x8664;
  const nativeFiles = collectFiles(root).filter((path) => /\.(?:exe|dll)$/iu.test(path));
  const packagedNames = new Set(nativeFiles.map((path) => basename(path).toLocaleLowerCase()));
  const system32 = join(process.env.SystemRoot || 'C:\\Windows', 'System32');
  let checked = 0;
  for (const path of nativeFiles) {
    const info = readPeRuntime(path);
    const normalizedPath = normalize(path).split('\\').join('/').toLocaleLowerCase();
    const name = basename(path).toLocaleLowerCase();
    const mustMatchTarget = name.includes('noland')
      || name.startsWith('wintun')
      || normalizedPath.includes('/gstreamer/')
      || normalizedPath.includes('/ssh-runtime/');
    if (mustMatchTarget && info.machine !== expectedMachine) {
      fail(`Wrong PE architecture in ${label}: ${path} (machine 0x${info.machine.toString(16)}, expected ${targetTriple})`);
    }
    for (const imported of info.imports) {
      const importName = imported.toLocaleLowerCase();
      if (!packagedNames.has(importName) && !existsSync(join(system32, imported))) {
        fail(`Unresolved Windows import in ${label}: ${path} requires ${imported}`);
      }
    }
    checked += 1;
  }
  if (checked === 0) fail(`No Windows PE files were dependency-checked in ${label}`);
  console.log(`Verified architecture and direct/delay imports for ${checked} PE files in ${label}`);
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
      ['gstapp.dll', 'libgstapp.dll'],
      ['gstaudioconvert.dll', 'libgstaudioconvert.dll'],
      ['gstaudioresample.dll', 'libgstaudioresample.dll'],
      ['gstopus.dll', 'libgstopus.dll'],
      ['gstrtp.dll', 'libgstrtp.dll'],
      ['gstrtpmanager.dll', 'libgstrtpmanager.dll'],
      ['gstudp.dll', 'libgstudp.dll'],
    ];
  }

  // Linux packages intentionally use the distro's GStreamer/WebKitGTK stack.
  // Their runtime coverage is verified through package dependencies and ldd,
  // not by requiring copied shared libraries inside the application bundle.
  return [];
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
    runAllowFailure('hdiutil', ['detach', mountPoint, '-force']);
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

function collectFiles(root) {
  if (!existsSync(root)) return [];
  const files = [];
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) stack.push(path);
      else if (entry.isFile()) files.push(path);
    }
  }
  return files;
}

function isPotentialUnixNativeFile(path) {
  if (/\.(?:dylib|bundle|so(?:\.[0-9.]+)?)$/iu.test(path)) return true;
  try {
    return (statSync(path).mode & 0o111) !== 0;
  } catch {
    return false;
  }
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
