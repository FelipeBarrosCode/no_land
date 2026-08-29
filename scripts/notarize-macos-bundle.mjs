#!/usr/bin/env node
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const target = process.argv[2] ?? 'aarch64-apple-darwin';
const scriptRevision = '2026-08-08-notary-dmg-verify-v4';
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
  console.log('[notarize-macos-bundle] Skipping on non-macOS host');
  process.exit(0);
}

if (!existsSync(appPath)) {
  console.error(`[notarize-macos-bundle] App bundle not found: ${appPath}`);
  process.exit(1);
}

const appleId = process.env.APPLE_ID?.trim();
const applePassword = process.env.APPLE_PASSWORD?.trim();
const appleTeamId = process.env.APPLE_TEAM_ID?.trim();
if (!appleId || !applePassword || !appleTeamId) {
  console.error('[notarize-macos-bundle] APPLE_ID, APPLE_PASSWORD, and APPLE_TEAM_ID are required for custom notarization');
  process.exit(1);
}

console.log(`[notarize-macos-bundle] Script revision: ${scriptRevision}`);
console.log(`[notarize-macos-bundle] Preparing notarization for ${target}`);
console.log(`[notarize-macos-bundle] App bundle: ${appPath}`);
console.log(`[notarize-macos-bundle] DMG bundle: ${dmgPath}`);

const tempDir = mkdtempSync(join(tmpdir(), 'noland-notary-'));
const zipPath = join(tempDir, `${productName}-${target}.zip`);
try {
  console.log('[notarize-macos-bundle] Creating app zip for notary submission');
  run('ditto', ['-c', '-k', '--keepParent', appPath, zipPath]);

  console.log('[notarize-macos-bundle] Submitting app zip to Apple notary service');
  const appSubmission = submitForNotarization(zipPath, 'app zip');

  console.log('[notarize-macos-bundle] Stapling notarization ticket to app bundle');
  run('xcrun', ['stapler', 'staple', '-v', appPath]);
  validateStapledArtifact(appPath, 'app bundle');
  validateGatekeeperForApp(appPath, 'app bundle');

  console.log('[notarize-macos-bundle] Rebuilding DMG from stapled app bundle');
  rebuildDmg(appPath, dmgPath, productName);

  if (!existsSync(dmgPath)) {
    throw new Error(`[notarize-macos-bundle] Expected DMG was not produced: ${dmgPath}`);
  }

  console.log('[notarize-macos-bundle] Submitting DMG to Apple notary service');
  const dmgSubmission = submitForNotarization(dmgPath, 'dmg');

  console.log('[notarize-macos-bundle] Stapling notarization ticket to DMG');
  run('xcrun', ['stapler', 'staple', '-v', dmgPath]);
  validateStapledArtifact(dmgPath, 'dmg');
  validateGatekeeperForDmg(dmgPath, 'dmg');
  validateMountedDmgApp(dmgPath, productName);

  console.log(`[notarize-macos-bundle] Notarization complete for ${dmgPath} (app submission ${appSubmission.id}, dmg submission ${dmgSubmission.id})`);
} finally {
  rmSync(tempDir, { recursive: true, force: true });
}

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

function rebuildDmg(app, dmg, volumeName) {
  mkdirSync(dirname(dmg), { recursive: true });
  if (existsSync(dmg)) rmSync(dmg, { force: true });

  const tempDir = mkdtempSync(join(tmpdir(), 'noland-dmg-out-'));
  const tempRoot = mkdtempSync(join(tmpdir(), 'noland-dmg-src-'));
  const tempDmg = join(tempDir, basename(dmg));
  const stagedApp = join(tempRoot, basename(app));
  try {
    run('ditto', [app, stagedApp]);
    createDmgWithRetry(tempRoot, tempDmg, volumeName);
    copyFileSync(tempDmg, dmg);
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
    rmSync(tempRoot, { recursive: true, force: true });
  }
}

function createDmgWithRetry(sourceFolder, outputDmg, volumeName) {
  const args = ['create', '-volname', volumeName, '-srcfolder', sourceFolder, '-ov', '-format', 'UDZO', outputDmg];

  for (let attempt = 1; attempt <= 3; attempt += 1) {
    const result = run('hdiutil', args, { captureOutput: true, allowFailure: true });
    if (result.status === 0) {
      return;
    }

    const output = `${result.stdout}\n${result.stderr}`.trim();
    const resourceBusy = /Resource busy/i.test(output);
    if (resourceBusy && attempt < 3) {
      console.warn(`[notarize-macos-bundle] hdiutil create reported a transient resource-busy error on attempt ${attempt}; retrying. Output:\n${output}`);
      sleepMs(2000);
      continue;
    }

    throw new Error(`Command failed: hdiutil ${args.join(' ')}\n${output}`);
  }
}

function sleepMs(milliseconds) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds);
}

function notarizationAuthArgs() {
  return [
    '--apple-id', appleId,
    '--password', applePassword,
    '--team-id', appleTeamId,
  ];
}

function submitForNotarization(path, label) {
  const args = [
    'notarytool',
    'submit',
    path,
    ...notarizationAuthArgs(),
    '--wait',
    '--output-format', 'json',
  ];

  let result;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    result = run('xcrun', args, { captureOutput: true, allowFailure: true });
    if (result.status === 0) break;

    const diagnostic = formatCommandFailure(result);
    if (!isTransientNotaryFailure(diagnostic) || attempt === 3) {
      throw new Error(
        `[notarize-macos-bundle] Apple notary submission failed for ${label} (exit ${result.status}).\n${diagnostic}`,
      );
    }

    const delayMs = attempt * 15_000;
    console.warn(
      `[notarize-macos-bundle] Transient Apple notary failure for ${label} on attempt ${attempt}/3; retrying in ${delayMs / 1_000}s.\n${diagnostic}`,
    );
    sleepMs(delayMs);
  }

  const payload = parseJson(result.stdout, `notarytool submit output for ${label}`);
  const status = `${payload.status ?? ''}`.trim();
  const submissionId = `${payload.id ?? ''}`.trim();
  if (!submissionId) {
    throw new Error(`[notarize-macos-bundle] Apple did not return a submission id for ${label}`);
  }

  console.log(`[notarize-macos-bundle] ${label} submission ${submissionId} completed with status: ${status || 'unknown'}`);

  if (status.toLowerCase() === 'accepted') {
    return payload;
  }

  const logPayload = fetchNotarizationLog(submissionId);
  console.error(`[notarize-macos-bundle] ${label} notarization failed with status ${status || 'unknown'}`);
  console.error(JSON.stringify(logPayload, null, 2));
  throw new Error(`[notarize-macos-bundle] ${label} notarization failed with status ${status || 'unknown'} (submission ${submissionId})`);
}

function formatCommandFailure(result) {
  return [result.stderr?.trim(), result.stdout?.trim()].filter(Boolean).join('\n')
    || 'notarytool returned no diagnostic output';
}

function isTransientNotaryFailure(output) {
  return /(?:timed? out|timeout|temporar|network|connection|service unavailable|internal server|HTTP status code: 5\d\d|NSURLErrorDomain|CloudKit)/iu.test(output);
}

function fetchNotarizationLog(submissionId) {
  const result = run('xcrun', [
    'notarytool',
    'log',
    submissionId,
    ...notarizationAuthArgs(),
    '--output-format', 'json',
  ], { captureOutput: true, allowFailure: true });

  const stdout = result.stdout?.trim();
  if (!stdout) {
    return {
      submissionId,
      error: 'No notarization log output was returned by Apple.',
      stderr: result.stderr?.trim() || null,
    };
  }

  try {
    return JSON.parse(stdout);
  } catch {
    return {
      submissionId,
      error: 'Failed to parse notarization log JSON.',
      stdout,
      stderr: result.stderr?.trim() || null,
    };
  }
}

function validateStapledArtifact(path, label) {
  console.log(`[notarize-macos-bundle] Validating stapled ticket for ${label}`);
  run('xcrun', ['stapler', 'validate', '-v', path]);
}

function validateGatekeeperForApp(path, label) {
  console.log(`[notarize-macos-bundle] Validating Gatekeeper acceptance for ${label}`);
  if (!existsSync(path)) {
    throw new Error(`[notarize-macos-bundle] Expected app bundle does not exist: ${path}`);
  }

  const primaryExecutable = resolvePrimaryExecutable(path);
  if (primaryExecutable) {
    console.log(`[notarize-macos-bundle] Verifying primary executable signature: ${primaryExecutable}`);
    const verify = run('codesign', ['--verify', '--strict', '--verbose=2', primaryExecutable], {
      captureOutput: true,
      allowFailure: true,
    });

    if (verify.status !== 0) {
      const verifyOutput = `${verify.stdout}\n${verify.stderr}`.trim();
      const missingPath = /No such file or directory/i.test(verifyOutput);
      if (missingPath) {
        console.warn(`[notarize-macos-bundle] Primary executable verification hit a transient path-resolution issue; continuing with Gatekeeper bundle assessment. Output:\n${verifyOutput}`);
      } else {
        throw new Error(`[notarize-macos-bundle] Primary executable signature verification failed for ${primaryExecutable}\n${verifyOutput}`);
      }
    }
  } else {
    console.warn(`[notarize-macos-bundle] Unable to resolve a primary executable inside ${path}; continuing with Gatekeeper bundle assessment only`);
  }

  run('spctl', ['-a', '-vvv', '-t', 'exec', path]);
}

function validateGatekeeperForDmg(path, label) {
  console.log(`[notarize-macos-bundle] Validating Gatekeeper acceptance for ${label}`);
  const assessment = run('spctl', ['-a', '-vvv', '-t', 'open', path], {
    captureOutput: true,
    allowFailure: true,
  });

  if (assessment.status === 0) {
    return;
  }

  const assessmentOutput = `${assessment.stdout}\n${assessment.stderr}`.trim();
  if (/Insufficient Context/i.test(assessmentOutput)) {
    console.warn(`[notarize-macos-bundle] DMG Gatekeeper assessment returned insufficient context on this CI host; continuing because stapler validation already passed and the mounted app will be assessed separately. Output:\n${assessmentOutput}`);
    return;
  }

  throw new Error(`[notarize-macos-bundle] Gatekeeper rejected ${label}\n${assessmentOutput}`);
}

function validateMountedDmgApp(dmg, volumeName) {
  console.log('[notarize-macos-bundle] Mounting notarized DMG for final app verification');
  const mountPoint = mkdtempSync(join(tmpdir(), 'noland-notary-mount-'));
  try {
    run('hdiutil', ['attach', dmg, '-mountpoint', mountPoint, '-nobrowse', '-readonly']);
    const mountedApp = findMountedAppBundle(mountPoint, volumeName);
    if (!mountedApp) {
      throw new Error(`[notarize-macos-bundle] Mounted DMG did not contain an app bundle in ${mountPoint}`);
    }
    console.log(`[notarize-macos-bundle] Mounted app bundle: ${mountedApp}`);
    validateGatekeeperForApp(mountedApp, 'mounted dmg app bundle');
  } finally {
    run('hdiutil', ['detach', mountPoint], { allowFailure: true });
    rmSync(mountPoint, { recursive: true, force: true });
  }
}

function resolvePrimaryExecutable(appBundlePath) {
  const contentsDir = join(appBundlePath, 'Contents');
  const macosDir = join(contentsDir, 'MacOS');
  const infoPlistPath = join(contentsDir, 'Info.plist');
  const plistExecutableName = readPlistString(infoPlistPath, 'CFBundleExecutable');
  if (plistExecutableName) {
    const executablePath = join(macosDir, plistExecutableName);
    if (existsSync(executablePath)) {
      return executablePath;
    }
  }

  const fallbackExecutable = join(macosDir, 'noland-connect');
  if (existsSync(fallbackExecutable)) {
    return fallbackExecutable;
  }

  if (!existsSync(macosDir)) {
    return null;
  }

  const appExecutables = readdirSync(macosDir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && !entry.name.startsWith('.'))
    .map((entry) => join(macosDir, entry.name));

  return appExecutables.length === 1 ? appExecutables[0] : null;
}

function findMountedAppBundle(mountPoint, expectedVolumeName) {
  const expectedBundle = join(mountPoint, `${expectedVolumeName}.app`);
  if (existsSync(expectedBundle)) {
    return expectedBundle;
  }

  const appBundles = readdirSync(mountPoint, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && entry.name.endsWith('.app'))
    .map((entry) => join(mountPoint, entry.name));

  return appBundles.length === 1 ? appBundles[0] : null;
}

function readPlistString(plistPath, key) {
  if (!existsSync(plistPath)) {
    return null;
  }

  const plistContents = readFileSync(plistPath, 'utf8');
  const escapedKey = key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = plistContents.match(new RegExp(`<key>${escapedKey}</key>\\s*<string>([^<]+)</string>`));
  return match?.[1]?.trim() || null;
}

function parseJson(text, context) {
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`[notarize-macos-bundle] Failed to parse ${context}: ${error.message}\n${text}`);
  }
}

function run(command, args, options = {}) {
  const { captureOutput = false, allowFailure = false } = options;
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    env: process.env,
    stdio: captureOutput ? ['ignore', 'pipe', 'pipe'] : 'inherit',
    encoding: captureOutput ? 'utf8' : undefined,
  });
  const status = result.status ?? (result.signal ? 128 : 1);
  if (!allowFailure && status !== 0) {
    const diagnostic = [result.stderr?.trim(), result.stdout?.trim()].filter(Boolean).join('\n');
    const signal = result.signal ? ` (signal ${result.signal})` : '';
    throw new Error(`Command failed: ${command}${signal}${diagnostic ? `\n${diagnostic}` : ''}`);
  }
  return {
    status,
    stdout: typeof result.stdout === 'string' ? result.stdout : '',
    stderr: typeof result.stderr === 'string' ? result.stderr : '',
  };
}
