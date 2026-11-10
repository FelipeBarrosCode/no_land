#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { chmodSync, createReadStream, lstatSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Public trust anchor, deliberately not learned from the downloaded release key.
export const linuxSigningFingerprint = 'C30ADA959FA25A245A1E902D7EA0DDFF6AD77028';
export const linuxPublicKey = 'linux-signing-key.asc';
const manifest = 'SHA256SUMS';
const manifestSignature = `${manifest}.asc`;
const provenance = 'provenance.sigstore.jsonl';
const packageExtensions = ['.AppImage', '.deb', '.rpm', '.pkg.tar.zst'];

function files(root) {
  if (!lstatSync(root).isDirectory()) throw new Error('Expected a release directory');
  const names = readdirSync(root).sort();
  for (const name of names) {
    if (/[\\\r\n]/u.test(name) || !lstatSync(join(root, name)).isFile()) throw new Error('Unsafe release file');
  }
  return names;
}

function packages(root) {
  const names = files(root);
  for (const extension of packageExtensions) {
    if (!names.some(name => name.endsWith(extension))) throw new Error(`Missing Linux ${extension} package`);
  }
  return names.filter(name => packageExtensions.some(extension => name.endsWith(extension)));
}

async function digest(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

function cleanEnvironment() {
  const env = { ...process.env };
  delete env.LINUX_GPG_PRIVATE_KEY;
  delete env.LINUX_GPG_PASSPHRASE;
  delete env.GNUPGHOME;
  return env;
}

async function withKeyring(callback) {
  const home = mkdtempSync(join(tmpdir(), 'noland-gpg-'));
  chmodSync(home, 0o700);
  const env = cleanEnvironment();
  const gpg = (args, input) => {
    const result = spawnSync('gpg', ['--no-options', '--homedir', home, '--batch', '--no-tty', ...args], {
      input, env, encoding: 'utf8', maxBuffer: 4 * 1024 * 1024, timeout: 120_000,
    });
    // Never echo GPG diagnostics, exported private keys, passphrases, or argv.
    if (result.error || result.signal || result.status !== 0) throw new Error('OpenPGP operation failed; check the signing key, passphrase, and signature validity');
    return result.stdout;
  };
  try {
    return await callback(gpg);
  } finally {
    spawnSync('gpgconf', ['--homedir', home, '--kill', 'gpg-agent'], { env, stdio: 'ignore', timeout: 10_000 });
    rmSync(home, { recursive: true, force: true });
  }
}

function requireFingerprint(gpg, fingerprint, secret = false) {
  if (!/^[A-F0-9]{40}$/u.test(fingerprint)) throw new Error('Expected a full OpenPGP fingerprint');
  const primaries = [];
  let primary = false;
  for (const line of gpg(['--with-colons', secret ? '--list-secret-keys' : '--list-keys']).split('\n')) {
    const fields = line.split(':');
    if (['pub', 'sec'].includes(fields[0])) {
      if (['r', 'e', 'd'].includes(fields[1])) throw new Error('OpenPGP release key is revoked, expired, or disabled');
      primary = true;
    }
    if (['sub', 'ssb'].includes(fields[0])) primary = false;
    if (fields[0] === 'fpr' && primary) { primaries.push(fields[9]); primary = false; }
  }
  if (primaries.length !== 1 || primaries[0] !== fingerprint) throw new Error('OpenPGP key does not match the pinned release fingerprint');
}

function verifySignature(gpg, root, name, fingerprint) {
  const status = gpg(['--status-fd', '1', '--verify', join(root, `${name}.asc`), join(root, name)]);
  if (/^\[GNUPG:\] (?:REVKEYSIG|EXPKEYSIG|KEYREVOKED|KEYEXPIRED|SIGEXPIRED|BADSIG|ERRSIG)\b/mu.test(status)) {
    throw new Error('OpenPGP signature or signing key is invalid, revoked, or expired');
  }
  if (!status.split('\n').some(line => line.startsWith(`[GNUPG:] VALIDSIG ${fingerprint} `))) {
    throw new Error('OpenPGP signature does not match the pinned release fingerprint');
  }
}

export async function verifyLinuxRelease(root, { fingerprint = linuxSigningFingerprint } = {}) {
  const signedPackages = packages(root);
  await withKeyring(async gpg => {
    gpg(['--import'], readFileSync(join(root, linuxPublicKey)));
    requireFingerprint(gpg, fingerprint);
    verifySignature(gpg, root, manifest, fingerprint);
    const expected = new Map();
    for (const line of readFileSync(join(root, manifest), 'utf8').trim().split('\n')) {
      const match = /^([a-f0-9]{64})  (.+)$/u.exec(line);
      if (!match) throw new Error('Malformed signed checksum manifest');
      const [, hash, name] = match;
      if (basename(name) !== name || /[\\\r\n]/u.test(name) || ['.', '..', manifest, manifestSignature, provenance].includes(name) || expected.has(name)) {
        throw new Error('Unsafe signed checksum entry');
      }
      expected.set(name, hash);
    }
    const actual = files(root).filter(name => ![manifest, manifestSignature, provenance].includes(name));
    if (actual.length !== expected.size || actual.some(name => !expected.has(name))) throw new Error('Release contains files outside the signed checksum manifest');
    for (const [name, hash] of expected) {
      if (await digest(join(root, name)) !== hash) throw new Error(`Signed checksum mismatch: ${name}`);
    }
    for (const name of signedPackages) verifySignature(gpg, root, name, fingerprint);
  });
  return signedPackages;
}

export async function signLinuxRelease(root, {
  privateKey = process.env.LINUX_GPG_PRIVATE_KEY,
  passphrase = process.env.LINUX_GPG_PASSPHRASE,
  fingerprint = linuxSigningFingerprint,
} = {}) {
  if (!privateKey?.trim() || typeof passphrase !== 'string' || /[\r\n]/u.test(passphrase)) throw new Error('Linux GPG signing secrets are missing or invalid');
  const signedPackages = packages(root);
  if (files(root).includes(provenance)) throw new Error('OpenPGP signing must precede Sigstore attestation');
  await withKeyring(async gpg => {
    gpg(['--import'], privateKey);
    requireFingerprint(gpg, fingerprint, true);
    writeFileSync(join(root, linuxPublicKey), gpg(['--armor', '--export', fingerprint]));
    const sign = name => gpg(['--yes', '--pinentry-mode', 'loopback', '--passphrase-fd', '0',
      '--local-user', `${fingerprint}!`, '--digest-algo', 'SHA256', '--armor',
      '--output', join(root, `${name}.asc`), '--detach-sign', join(root, name)], `${passphrase}\n`);
    for (const name of signedPackages) sign(name);
    const checksums = [];
    for (const name of files(root).filter(name => ![manifest, manifestSignature].includes(name))) {
      checksums.push(`${await digest(join(root, name))}  ${name}\n`);
    }
    writeFileSync(join(root, manifest), checksums.join(''));
    sign(manifest);
  });
  // Verify from a fresh public-only keyring, not the signing keyring's trustdb.
  return verifyLinuxRelease(root, { fingerprint });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [action, directory] = process.argv.slice(2);
    if (!['sign', 'verify'].includes(action) || !directory) throw new Error('Usage: node scripts/linux-gpg-signatures.mjs <sign|verify> <release-directory>');
    const result = await (action === 'sign' ? signLinuxRelease : verifyLinuxRelease)(resolve(directory));
    console.log(`OpenPGP ${action} completed for ${result.length} Linux packages and the release checksum manifest.`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
