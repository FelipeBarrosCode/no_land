import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { test } from 'node:test';
import { manageSigningInputs, windowsSigningInputs } from '../windows-signing-inputs.mjs';
import { provenanceBundle, verifyReleaseProvenance } from '../verify-release-provenance.mjs';

const read = path => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
const sha = value => createHash('sha256').update(value).digest('hex');
const fixture = t => {
  const root = mkdtempSync(join(tmpdir(), 'noland-signature-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
};

for (const target of ['x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc']) {
  test(`${target}: app and sidecars are signed before bundling; mutations are detected`, t => {
    const root = fixture(t);
    const stage = join(root, 'signing');
    const inputs = windowsSigningInputs(root, target);
    for (const file of inputs) { mkdirSync(dirname(file), { recursive: true }); writeFileSync(file, 'unsigned fixture'); }
    manageSigningInputs('stage', root, target, stage);
    for (const file of inputs) writeFileSync(join(stage, basename(file)), 'signed fixture');
    manageSigningInputs('restore', root, target, stage);
    assert.ok(inputs.every(path => readFileSync(path, 'utf8') === 'signed fixture'));
    assert.deepEqual(manageSigningInputs('verify', root, target), inputs);
    writeFileSync(inputs[0], 'bundler patched signed bytes');
    assert.throws(() => manageSigningInputs('verify', root, target), /modified a signed executable/);
  });
}

test('missing/empty signing inputs and unsupported targets fail before staging', t => {
  const root = fixture(t);
  assert.throws(() => windowsSigningInputs(root, '../../other'), /Unsupported/);
  assert.throws(() => manageSigningInputs('stage', root, 'x86_64-pc-windows-msvc', join(root, 'stage')), /ENOENT/);
  const inputs = windowsSigningInputs(root, 'x86_64-pc-windows-msvc');
  for (const file of inputs) { mkdirSync(dirname(file), { recursive: true }); writeFileSync(file, ''); }
  assert.throws(() => manageSigningInputs('stage', root, 'x86_64-pc-windows-msvc', join(root, 'stage')), /unsafe/);
});

function release(t) {
  const root = fixture(t);
  const files = { 'Noland.AppImage': 'appimage bytes', 'Noland.deb': 'deb bytes', 'Noland.rpm': 'rpm bytes', 'latest.json': '{}' };
  for (const [name, content] of Object.entries(files)) writeFileSync(join(root, name), content);
  writeFileSync(join(root, 'SHA256SUMS'), Object.entries(files).map(([name, content]) => `${sha(content)}  ${name}\n`).join(''));
  writeFileSync(join(root, provenanceBundle), 'offline orchestration fixture, not a valid signature');
  return root;
}
const policy = { repository: 'FelipeBarrosCode/no_land', sourceRef: 'refs/heads/main', sourceDigest: 'a'.repeat(40) };

test('every exact final artifact and its checksum manifest require cryptographic verification', async t => {
  const root = release(t);
  const calls = [];
  const files = await verifyReleaseProvenance(root, { ...policy, run(command, args) { calls.push([command, args]); return { status: 0 }; } });
  assert.equal(files.length, 5);
  assert.ok(files.includes('SHA256SUMS'));
  for (const [command, args] of calls) {
    assert.equal(command, 'gh');
    assert.deepEqual(args.slice(0, 2), ['attestation', 'verify']);
    for (const [flag, value] of [['--repo', policy.repository], ['--source-ref', policy.sourceRef], ['--source-digest', policy.sourceDigest], ['--bundle', join(root, provenanceBundle)], ['--signer-workflow', 'FelipeBarrosCode/no_land/.github/workflows/release.yml']]) {
      assert.equal(args[args.indexOf(flag) + 1], value);
    }
    assert.ok(args.includes('--deny-self-hosted-runners'));
  }
});

test('verification failure blocks publication, even when checksums match', async t => {
  for (const result of [{ status: 1 }, { status: null, signal: 'SIGTERM' }, { status: null, error: new Error('gh unavailable') }]) {
    await assert.rejects(() => verifyReleaseProvenance(release(t), { ...policy, run: () => result }), /provenance verification failed/);
  }
});

test('missing package formats and symlinked signing bundles fail closed', async t => {
  const incomplete = release(t);
  rmSync(join(incomplete, 'Noland.rpm'));
  writeFileSync(join(incomplete, 'SHA256SUMS'), readFileSync(join(incomplete, 'SHA256SUMS'), 'utf8').split('\n').filter(line => !line.endsWith('Noland.rpm')).join('\n'));
  await assert.rejects(() => verifyReleaseProvenance(incomplete, { ...policy, run: () => ({ status: 0 }) }), /No signed Linux \.rpm/);

  const root = release(t);
  const target = join(fixture(t), 'other-bundle.jsonl');
  writeFileSync(target, 'untrusted bundle');
  rmSync(join(root, provenanceBundle));
  try {
    symlinkSync(target, join(root, provenanceBundle));
  } catch (error) {
    if (process.platform === 'win32' && error.code === 'EPERM') return;
    throw error;
  }
  await assert.rejects(() => verifyReleaseProvenance(root, { ...policy, run: () => ({ status: 0 }) }), /Missing Sigstore/);
});

test('corrupt artifacts, extra unsigned files and unsafe manifest paths fail closed', async t => {
  for (const mutate of [
    root => writeFileSync(join(root, 'Noland.deb'), 'corrupted'),
    root => writeFileSync(join(root, 'unsigned.bin'), 'extra'),
    root => writeFileSync(join(root, 'SHA256SUMS'), `${'a'.repeat(64)}  ../escape.deb\n`),
  ]) {
    const root = release(t);
    mutate(root);
    await assert.rejects(() => verifyReleaseProvenance(root, { ...policy, run: () => ({ status: 0 }) }));
  }
});

test('verification requires the workflow, source commit and ref rather than repository trust alone', async t => {
  for (const override of [{ sourceRef: '' }, { sourceDigest: '' }, { repository: '' }]) {
    await assert.rejects(() => verifyReleaseProvenance(release(t), { ...policy, ...override }), /exact source ref/);
  }
});

test('Windows uses configured signing secrets and retains pre-bundle/installer verification order', () => {
  const workflow = read('.github/workflows/_build.yml');
  const windows = workflow.split('\n  build-windows:\n')[1];
  assert.match(windows, /--no-bundle/);
  assert.match(windows, /bundle --target.*--no-binary-patching --ci/);
  assert.deepEqual(JSON.parse(read('src-tauri/tauri.windows.conf.json')).bundle.targets, ['nsis']);
  assert.deepEqual(JSON.parse(read('src-tauri/tauri.windows.arm64.conf.json')).bundle.targets, ['nsis']);
  assert.equal([...windows.matchAll(/endpoint: \$\{\{ secrets\.TRUSTED_SIGNING_ENDPOINT \}\}/g)].length, 2);
  assert.equal([...windows.matchAll(/signing-account-name: \$\{\{ secrets\.TRUSTED_SIGNING_ACCOUNT_NAME \}\}/g)].length, 2);
  const names = ['Build Windows Tauri application', 'Stage application and sidecars for Authenticode signing', 'Sign Windows application and sidecars with Azure Artifact Signing', 'Verify and restore signed application inputs', 'Bundle the signed application without mutating its signature', 'Sign Windows installers with Azure Artifact Signing', 'Verify Windows Authenticode signatures', 'Regenerate Tauri updater signatures after Authenticode signing'];
  for (let index = 1; index < names.length; index++) assert.ok(windows.indexOf(`- name: ${names[index - 1]}\n`) < windows.indexOf(`- name: ${names[index]}\n`));
  const verifier = read('scripts/verify-windows-signatures.ps1');
  assert.match(verifier, /Status -ne 'Valid'/);
  assert.match(verifier, /TimeStamperCertificate/);
  const smoke = read('scripts/verify-bundled-sidecars.mjs');
  assert.match(smoke, /for \(const executable of \[appExecutable, helper, micSender\]\)/);
  assert.match(smoke, /verify-windows-signatures\.ps1.*'-FilePath', executable/);
});

test('release attestations bind all final bytes and are re-verified before a public tag is created', () => {
  const workflow = read('.github/workflows/release.yml');
  const packaging = workflow.split('\n  package-validation:\n')[1].split('\n  release:\n')[0];
  assert.match(packaging, /id-token: write/);
  assert.match(packaging, /attestations: write/);
  assert.match(packaging, /actions\/attest-build-provenance@[a-f0-9]{40}/);
  assert.match(packaging, /subject-path: release-publish\/\*/);
  assert.ok(packaging.indexOf('Recompute checksums including SBOM') < packaging.indexOf('Sign exact release artifacts'));
  assert.ok(packaging.indexOf('Verify every release signature') < packaging.indexOf('Upload validated production release set'));
  const publish = workflow.split('\n  release:\n')[1];
  assert.ok(publish.indexOf('Verify signed artifacts again') < publish.indexOf('Create and push production release tag'));
  assert.doesNotMatch(packaging, /continue-on-error:/);
});
