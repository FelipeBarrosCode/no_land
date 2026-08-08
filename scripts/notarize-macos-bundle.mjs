#!/usr/bin/env node
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const target = process.argv[2] ?? 'aarch64-apple-darwin';
const scriptRevision = '2026-08-08-notary-log-v2';
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

const tempDir = mkdtempSync(join(tmpdir(), 'noland-notary-'));
const zipPath = join(tempDir, `${productName}-${target}.zip`);
try {
  console.log('[notarize-macos-bundle] Creating app zip for notary submission');
  run('ditto', ['-c', '-k', '--keepParent', appPath, zipPath]);

  console.log('[notarize-macos-bundle] Submitting app zip to Apple notary service');
  const appSubmission = submitForNotarization(zipPath, 'app zip');

  console.log('[notarize-macos-bundle] Stapling notarization ticket to app bundle');
  run('xcrun', ['stapler', 'staple', '-v', appPath]);

  console.log('[notarize-macos-bundle] Rebuilding DMG from stapled app bundle');
  rebuildDmg(appPath, dmgPath, productName);

  if (existsSync(dmgPath)) {
    console.log('[notarize-macos-bundle] Submitting DMG to Apple notary service');
    submitForNotarization(dmgPath, 'dmg');

    console.log('[notarize-macos-bundle] Stapling notarization ticket to DMG');
    run('xcrun', ['stapler', 'staple', '-v', dmgPath]);
  }

  console.log(`[notarize-macos-bundle] Notarization complete for ${appPath} (submission ${appSubmission.id})`);
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
    run('hdiutil', ['create', '-volname', volumeName, '-srcfolder', stagedApp, '-ov', '-format', 'UDZO', tempDmg]);
    copyFileSync(tempDmg, dmg);
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
    rmSync(tempRoot, { recursive: true, force: true });
  }
}

function notarizationAuthArgs() {
  return [
    '--apple-id', appleId,
    '--password', applePassword,
    '--team-id', appleTeamId,
  ];
}

function submitForNotarization(path, label) {
  const result = run('xcrun', [
    'notarytool',
    'submit',
    path,
    ...notarizationAuthArgs(),
    '--wait',
    '--output-format', 'json',
  ], { captureOutput: true });

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
  if (!allowFailure && result.status !== 0) {
    throw new Error(`Command failed: ${command} ${args.join(' ')}`);
  }
  return {
    status: result.status ?? 0,
    stdout: typeof result.stdout === 'string' ? result.stdout : '',
    stderr: typeof result.stderr === 'string' ? result.stderr : '',
  };
}
