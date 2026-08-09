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
  const name = basename(source);
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

const windowsStoreInstaller = publishedAssets.find((file) => /_x64-store-setup\.exe$/iu.test(basename(file)));
if (windowsStoreInstaller) {
  const packageUrl = `https://github.com/${repositoryOwner}/${repositoryName}/releases/download/${releaseTag}/${basename(windowsStoreInstaller)}`;
  const metadata = {
    product_name: productName,
    app_version: appVersion,
    release_tag: releaseTag,
    package_url: packageUrl,
    architecture: 'x64',
    installer_type: 'exe',
    silent_install_args: '/S',
  };

  writeFileSync(join(publishRoot, 'windows-store-x64-submission.json'), `${JSON.stringify(metadata, null, 2)}\n`);
  writeFileSync(join(publishRoot, 'windows-store-x64-package-url.txt'), `${packageUrl}\n`);
  writeFileSync(
    join(publishRoot, 'windows-store-x64-submission.md'),
    [
      '# Windows Store submission (x64)',
      '',
      `- Product: ${productName}`,
      `- App version: ${appVersion}`,
      `- Release tag: ${releaseTag}`,
      `- Package URL: ${packageUrl}`,
      '- Architecture: x64',
      '- Installer type: exe',
      '- Silent install args: `/S`',
      '',
      'Use the package URL above in Microsoft Partner Center for the traditional desktop app submission.',
      '',
    ].join('\n'),
  );
}

console.log(`[prepare-release-assets] Prepared ${publishedAssets.length} publishable assets in ${publishRoot}`);

function shouldPublish(relativePath, baseName) {
  if (baseName.endsWith('.dmg')) return true;
  if (baseName.endsWith('.AppImage')) return true;
  if (baseName.endsWith('.deb')) return true;
  if (baseName.endsWith('.rpm')) return true;
  if (baseName.endsWith('.msi')) return true;
  if (/_(x64|arm64)\.app\.zip$/iu.test(baseName)) return true;
  if (/_x64-store-setup\.exe$/iu.test(baseName)) return true;
  if (/_(x64|arm64)-setup\.exe$/iu.test(baseName)) return true;

  console.log(`[prepare-release-assets] Skipped ${relativePath}`);
  return false;
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
