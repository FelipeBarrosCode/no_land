#!/usr/bin/env node
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const releaseTag = process.argv[2]?.trim();

if (!releaseTag) {
  console.error('Usage: node scripts/prepare-release-assets.mjs <release-tag>');
  process.exit(1);
}

const downloadedAssetsRoot = join(repoRoot, 'release-assets');
const publishRoot = join(repoRoot, 'release-publish');
const tauriConfig = JSON.parse(readFileSync(join(repoRoot, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const productName = tauriConfig.productName ?? 'Noland Connect';
const appVersion = tauriConfig.version ?? '0.1.0';
const [repositoryOwner = 'FelipeBarrosCode', repositoryName = 'no_land'] = (process.env.GITHUB_REPOSITORY || 'FelipeBarrosCode/no_land').split('/');
const windowsStoreBaseUrl = process.env.WINDOWS_STORE_BASE_URL?.trim().replace(/\/$/u, '') || null;
const windowsStorePathPrefix = sanitizePathPrefix(process.env.WINDOWS_STORE_PATH_PREFIX?.trim() || 'no_land');

if (!existsSync(downloadedAssetsRoot)) {
  console.error(`Downloaded release assets directory does not exist: ${downloadedAssetsRoot}`);
  process.exit(1);
}

rmSync(publishRoot, { recursive: true, force: true });
mkdirSync(publishRoot, { recursive: true });

const discoveredFiles = listFiles(downloadedAssetsRoot);
const approvedFiles = discoveredFiles.filter((file) => shouldPublish(relative(downloadedAssetsRoot, file), basename(file)));

if (approvedFiles.length === 0) {
  console.error(`No approved release assets were found under ${downloadedAssetsRoot}`);
  process.exit(1);
}

const publishedAssetNames = new Set();
const publishedAssets = [];
for (const source of approvedFiles) {
  const originalName = basename(source);
  const name = sanitizePublishedAssetName(originalName);
  if (publishedAssetNames.has(name)) {
    console.error(`Duplicate published asset name detected: ${name}`);
    process.exit(1);
  }
  publishedAssetNames.add(name);

  const destination = join(publishRoot, name);
  copyFileSync(source, destination);
  publishedAssets.push(destination);
  console.log(`[prepare-release-assets] Included ${relative(repoRoot, source)} -> ${relative(repoRoot, destination)}`);
}

for (const architecture of ['x64', 'arm64']) {
  const windowsStoreInstaller = publishedAssets.find((file) => new RegExp(`_${architecture}-store-setup\\.exe$`, 'iu').test(basename(file)));
  if (!windowsStoreInstaller) continue;

  const versionedObjectKey = [windowsStorePathPrefix, releaseTag, basename(windowsStoreInstaller)].filter(Boolean).join('/');
  const stableObjectKey = [windowsStorePathPrefix, 'windows-store', `latest-${architecture}-setup.exe`].filter(Boolean).join('/');
  const versionedPackageUrl = windowsStoreBaseUrl
    ? `${windowsStoreBaseUrl}/${versionedObjectKey}`
    : `https://github.com/${repositoryOwner}/${repositoryName}/releases/download/${releaseTag}/${basename(windowsStoreInstaller)}`;
  const stablePackageUrl = windowsStoreBaseUrl ? `${windowsStoreBaseUrl}/${stableObjectKey}` : versionedPackageUrl;
  const metadata = {
    product_name: productName,
    app_version: appVersion,
    release_tag: releaseTag,
    package_url: stablePackageUrl,
    object_key: stableObjectKey,
    versioned_package_url: versionedPackageUrl,
    versioned_object_key: versionedObjectKey,
    architecture,
    installer_type: 'exe',
    silent_install_args: '/S',
  };
  const metadataPrefix = `windows-store-${architecture}`;

  writeFileSync(join(publishRoot, `${metadataPrefix}-submission.json`), `${JSON.stringify(metadata, null, 2)}\n`);
  writeFileSync(join(publishRoot, `${metadataPrefix}-package-url.txt`), `${stablePackageUrl}\n`);
  writeFileSync(join(publishRoot, `${metadataPrefix}-object-key.txt`), `${stableObjectKey}\n`);
  writeFileSync(join(publishRoot, `${metadataPrefix}-versioned-package-url.txt`), `${versionedPackageUrl}\n`);
  writeFileSync(join(publishRoot, `${metadataPrefix}-versioned-object-key.txt`), `${versionedObjectKey}\n`);
  writeFileSync(
    join(publishRoot, `${metadataPrefix}-submission.md`),
    [
      `# Windows Store submission (${architecture})`,
      '',
      `- Product: ${productName}`,
      `- App version: ${appVersion}`,
      `- Release tag: ${releaseTag}`,
      `- Stable package URL: ${stablePackageUrl}`,
      `- Stable object key: ${stableObjectKey}`,
      `- Versioned package URL: ${versionedPackageUrl}`,
      `- Versioned object key: ${versionedObjectKey}`,
      `- Architecture: ${architecture}`,
      '- Installer type: exe',
      '- Silent install args: `/S`',
      '',
      architecture === 'arm64'
        ? '- Known limitation: microphone passthrough is unavailable on Windows ARM64 because the required GStreamer SDK is not published for this target.'
        : null,
      '',
      windowsStoreBaseUrl
        ? 'Use the stable package URL above in Microsoft Partner Center for the traditional desktop app submission. The CI release job uploads both the stable object and a versioned object to Cloudflare R2 on each release.'
        : 'Use the package URL above in Microsoft Partner Center for the traditional desktop app submission.',
      '',
    ].filter((line) => line !== null).join('\n'),
  );
}

console.log(`[prepare-release-assets] Prepared ${publishedAssets.length} publishable assets in ${publishRoot}`);

function shouldPublish(relativePath, baseName) {
  if (baseName.endsWith('.dmg')) return true;
  // AppImage is intentionally not published for the Linux client right now.
  // WebKitGTK/GIO can load host modules while AppImage injects bundled usr/lib,
  // causing GLib/GIO/libcurl symbol mismatches on Ubuntu/Zorin LTS.
  if (baseName.endsWith('.AppImage')) return false;
  if (baseName.endsWith('.deb')) return true;
  if (baseName.endsWith('.rpm')) return true;
  if (baseName.endsWith('.msi')) return true;
  if (/_(x64|arm64)\.app\.zip$/iu.test(baseName)) return true;
  if (/_(x64|arm64)-store-setup\.exe$/iu.test(baseName)) return true;
  if (/_(x64|arm64)-setup\.exe$/iu.test(baseName)) return true;

  console.log(`[prepare-release-assets] Skipped ${relativePath}`);
  return false;
}

function sanitizePathPrefix(value) {
  return value.replace(/^\/+|\/+$/gu, '');
}

function sanitizePublishedAssetName(value) {
  return value.replace(/\s+/gu, '.');
}

function listFiles(root) {
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
  return results.sort();
}
