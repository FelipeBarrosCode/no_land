#!/usr/bin/env node
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const target = process.argv[2] ?? 'aarch64-apple-darwin';
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

console.log(`[notarize-macos-bundle] Preparing notarization for ${target}`);
console.log(`[notarize-macos-bundle] App bundle: ${appPath}`);

const tempDir = mkdtempSync(join(tmpdir(), 'noland-notary-'));
const zipPath = join(tempDir, `${productName}-${target}.zip`);
try {
  console.log('[notarize-macos-bundle] Creating app zip for notary submission');
  run('ditto', ['-c', '-k', '--keepParent', appPath, zipPath]);

  console.log('[notarize-macos-bundle] Submitting app zip to Apple notary service');
  run('xcrun', [
    'notarytool',
    'submit',
    zipPath,
    '--apple-id', appleId,
    '--password', applePassword,
    '--team-id', appleTeamId,
    '--wait',
  ]);

  console.log('[notarize-macos-bundle] Stapling notarization ticket to app bundle');
  run('xcrun', ['stapler', 'staple', '-v', appPath]);

  console.log('[notarize-macos-bundle] Rebuilding DMG from stapled app bundle');
  rebuildDmg(appPath, dmgPath, productName);

  if (existsSync(dmgPath)) {
    console.log('[notarize-macos-bundle] Submitting DMG to Apple notary service');
    run('xcrun', [
      'notarytool',
      'submit',
      dmgPath,
      '--apple-id', appleId,
      '--password', applePassword,
      '--team-id', appleTeamId,
      '--wait',
    ]);

    console.log('[notarize-macos-bundle] Stapling notarization ticket to DMG');
    run('xcrun', ['stapler', 'staple', '-v', dmgPath]);
  }

  console.log(`[notarize-macos-bundle] Notarization complete for ${appPath}`);
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

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    env: process.env,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    throw new Error(`Command failed: ${command} ${args.join(' ')}`);
  }
}
