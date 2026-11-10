import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const workflow = readFileSync(new URL('../../.github/workflows/_build.yml', import.meta.url), 'utf8');
const windows = workflow.split('\n  build-windows:\n')[1];
const step = (name) => {
  const start = workflow.indexOf(`      - name: ${name}\n`);
  assert.ok(start >= 0, `Missing required step: ${name}`);
  const end = workflow.indexOf('\n      - ', start + 1);
  return workflow.slice(start, end === -1 ? undefined : end);
};

test('both Windows targets freshly install the pinned signing action dependencies', () => {
  assert.ok(windows);
  for (const target of ['x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc']) {
    assert.ok(windows.includes(`target: ${target}`));
  }
  const signing = step('Sign Windows installers with Azure Artifact Signing');
  assert.match(signing, /uses: azure\/artifact-signing-action@[a-f0-9]{40}/u);
  // The upstream composite action skips Install-Module on a cache hit even if
  // the restored module is unusable. false is its supported fresh-install path.
  assert.match(signing, /^          cache-dependencies: false$/mu);
  assert.doesNotMatch(signing, /continue-on-error:/u);
  for (const credential of ['tenant-id', 'client-id', 'client-secret']) {
    assert.match(signing, new RegExp(`azure-${credential}: \\$\\{\\{ secrets\\.AZURE_`));
  }
  assert.match(signing, /exclude-environment-credential: false/u);
  assert.match(signing, /files-folder-filter: exe,msi/u);
  assert.match(signing, /file-digest: SHA256/u);
  assert.match(signing, /timestamp-digest: SHA256/u);
});

test('signature validation and updater re-signing remain mandatory and ordered', () => {
  const verification = step('Verify Windows Authenticode signatures');
  assert.match(verification, /Get-AuthenticodeSignature -LiteralPath/u);
  assert.match(verification, /if \(\$signature\.Status -ne 'Valid'\) \{ throw/u);
  assert.doesNotMatch(verification, /continue-on-error:/u);
  const updater = step('Regenerate Tauri updater signatures after Authenticode signing');
  assert.match(updater, /tauri\.js signer sign/u);
  assert.match(updater, /if \(\$LASTEXITCODE -ne 0/u);
  const orderedSteps = ['Sign Windows installers with Azure Artifact Signing',
    'Verify Windows Authenticode signatures',
    'Regenerate Tauri updater signatures after Authenticode signing'];
  for (let i = 1; i < orderedSteps.length; i++) {
    assert.ok(workflow.indexOf(`- name: ${orderedSteps[i - 1]}\n`)
      < workflow.indexOf(`- name: ${orderedSteps[i]}\n`));
  }
});

test('downloadable macOS archives use bounded ticket-validation retries without skipping trust checks', () => {
  const archive = step('Create and validate macOS downloadable archives');
  assert.match(archive, /node scripts\/apple-stapler-retry\.mjs validate "\$extract_dir\/\$\{product_name\}\.app"/u);
  assert.match(archive, /node scripts\/apple-stapler-retry\.mjs validate "\$updater_dir\/\$\{product_name\}\.app"/u);
  assert.equal([...archive.matchAll(/codesign --verify --deep --strict/g)].length, 2);
  assert.equal([...archive.matchAll(/spctl -a -vvv -t exec/g)].length, 2);
  assert.doesNotMatch(archive, /xcrun stapler|continue-on-error|\|\| true/u);
});

test('release suite runs the offline Apple retry regression tests', () => {
  const suite = readFileSync(new URL('../../.github/workflows/_tests.yml', import.meta.url), 'utf8');
  assert.match(suite, /run: node --test .*scripts\/apple-stapler-retry\.test\.mjs/u);
});
