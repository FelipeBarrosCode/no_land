import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { linuxPublicKey, linuxSigningFingerprint, signLinuxRelease, verifyLinuxRelease } from '../linux-gpg-signatures.mjs';
import { verifyReleaseProvenance } from '../verify-release-provenance.mjs';

const fixturePassphrase = 'local-test-fixture-only';
const temporary = t => {
  const path = mkdtempSync(join(tmpdir(), 'noland-pgp-test-'));
  t.after(() => rmSync(path, { recursive: true, force: true }));
  return path;
};

test('real OpenPGP signing and public-only verification fail closed without touching the user keyring', async t => {
  const home = mkdtempSync(join(tmpdir(), 'noland-pgp-test-'));
  t.after(() => {
    spawnSync('gpgconf', ['--homedir', home, '--kill', 'gpg-agent'], { stdio: 'ignore' });
    rmSync(home, { recursive: true, force: true });
  });
  const gpg = args => {
    const result = spawnSync('gpg', ['--no-options', '--homedir', home, '--batch', '--pinentry-mode', 'loopback', '--passphrase-fd', '0', ...args], {
      input: `${fixturePassphrase}\n`, encoding: 'utf8', timeout: 120_000, maxBuffer: 1024 * 1024,
    });
    assert.equal(result.status, 0, 'Isolated fixture GPG operation must succeed');
    return result.stdout;
  };
  // This is an ephemeral test key, never the user's production signing key.
  gpg(['--quick-generate-key', 'No Land CI fixture <ci@example.invalid>', 'ed25519', 'sign', '1d']);
  const fingerprint = gpg(['--with-colons', '--list-secret-keys']).split('\n').find(line => line.startsWith('fpr:')).split(':')[9];
  gpg(['--quick-add-key', fingerprint, 'cv25519', 'encr', '1d']);
  const privateKey = gpg(['--armor', '--export-secret-keys', fingerprint]);
  const credentials = { privateKey, passphrase: fixturePassphrase, fingerprint };
  const root = temporary(t);
  for (const name of ['Noland-x64.AppImage', 'Noland-arm64.AppImage', 'Noland.deb', 'Noland.rpm', 'Noland.pkg.tar.zst', 'windows-setup.exe', 'latest.json', 'SBOM.cdx.json']) {
    writeFileSync(join(root, name), `fixture bytes for ${name}`);
  }

  await t.test('signs all four package formats, publishes the public key, and authenticates the manifest', async () => {
    const signed = await signLinuxRelease(root, credentials);
    assert.equal(signed.length, 5);
    assert.deepEqual(await verifyLinuxRelease(root, { fingerprint }), signed);
    assert.match(readFileSync(join(root, linuxPublicKey), 'utf8'), /^-----BEGIN PGP PUBLIC KEY BLOCK-----/u);
    const manifest = readFileSync(join(root, 'SHA256SUMS'), 'utf8');
    assert.match(manifest, /  linux-signing-key\.asc\n/u);
    assert.match(manifest, /  Noland\.rpm\.asc\n/u);
    assert.match(manifest, /  SBOM\.cdx\.json\n/u);
    assert.doesNotMatch(manifest, /  SHA256SUMS(?:\.asc)?\n/u);
  });

  await t.test('production verification refuses a different valid key', async () => {
    assert.notEqual(fingerprint, linuxSigningFingerprint);
    await assert.rejects(() => verifyLinuxRelease(root), /pinned release fingerprint/u);
  });

  await t.test('GPG outputs remain covered by the complete Sigstore verification policy', async () => {
    const attested = temporary(t);
    cpSync(root, attested, { recursive: true });
    writeFileSync(join(attested, 'provenance.sigstore.jsonl'), 'orchestration fixture only');
    const verified = await verifyReleaseProvenance(attested, {
      repository: 'FelipeBarrosCode/no_land', sourceRef: 'refs/heads/main', sourceDigest: 'a'.repeat(40), run: () => ({ status: 0 }),
    });
    assert.ok(verified.includes('SHA256SUMS.asc'));
    assert.ok(verified.includes(linuxPublicKey));
    assert.ok(verified.includes('Noland.rpm.asc'));
    await verifyLinuxRelease(attested, { fingerprint });
  });

  for (const [label, mutate] of [
    ['changed Linux package', path => writeFileSync(join(path, 'Noland.rpm'), 'modified')],
    ['changed non-Linux artifact', path => writeFileSync(join(path, 'windows-setup.exe'), 'modified')],
    ['changed checksum manifest', path => writeFileSync(join(path, 'SHA256SUMS'), 'forged manifest')],
    ['changed package signature', path => writeFileSync(join(path, 'Noland.rpm.asc'), 'forged signature')],
    ['missing manifest signature', path => rmSync(join(path, 'SHA256SUMS.asc'))],
    ['missing public key', path => rmSync(join(path, linuxPublicKey))],
    ['missing Linux package signature', path => rmSync(join(path, 'Noland.deb.asc'))],
    ['extra unsigned file', path => writeFileSync(join(path, 'extra.bin'), 'unsigned')],
  ]) {
    await t.test(`rejects ${label}`, async () => {
      const corrupted = temporary(t);
      cpSync(root, corrupted, { recursive: true });
      mutate(corrupted);
      await assert.rejects(() => verifyLinuxRelease(corrupted, { fingerprint }));
    });
  }

  await t.test('missing secrets, wrong passphrases, and wrong private keys never produce a successful signing result', async () => {
    const empty = temporary(t);
    cpSync(root, empty, { recursive: true });
    await assert.rejects(() => signLinuxRelease(empty, { privateKey: '', passphrase: '' }), /missing or invalid/u);
    await assert.rejects(() => signLinuxRelease(empty, { ...credentials, passphrase: 'wrong-fixture-passphrase' }), /OpenPGP operation failed/u);
    await assert.rejects(() => signLinuxRelease(empty, { ...credentials, fingerprint: linuxSigningFingerprint }), /pinned release fingerprint/u);
    writeFileSync(join(empty, 'provenance.sigstore.jsonl'), 'already attested');
    await assert.rejects(() => signLinuxRelease(empty, credentials), /must precede Sigstore/u);
  });
});

test('protected release steps require GPG verification before attestation and publication', () => {
  const workflow = readFileSync(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8').replace(/\r\n/gu, '\n');
  const packaging = workflow.split('\n  package-validation:\n')[1].split('\n  release:\n')[0];
  assert.match(packaging, /environment: Secrets/u);
  assert.match(packaging, /LINUX_GPG_PRIVATE_KEY: \$\{\{ secrets\.LINUX_GPG_PRIVATE_KEY \}\}/u);
  assert.match(packaging, /LINUX_GPG_PASSPHRASE: \$\{\{ secrets\.LINUX_GPG_PASSPHRASE \}\}/u);
  assert.ok(packaging.indexOf('linux-gpg-signatures.mjs sign') < packaging.indexOf('Sign exact release artifacts with Sigstore'));
  assert.ok(packaging.indexOf('linux-gpg-signatures.mjs verify') < packaging.indexOf('Upload validated production release set'));
  const publication = workflow.split('\n  release:\n')[1];
  assert.ok(publication.indexOf('linux-gpg-signatures.mjs verify') < publication.indexOf('Create and push production release tag'));
  assert.doesNotMatch(publication, /LINUX_GPG_PRIVATE_KEY/u);
  const script = readFileSync(new URL('../linux-gpg-signatures.mjs', import.meta.url), 'utf8');
  assert.match(script, /--passphrase-fd/u);
  assert.doesNotMatch(script, /stdio: 'inherit'/u);
});
