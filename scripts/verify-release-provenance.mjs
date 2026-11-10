#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { createReadStream, lstatSync, readFileSync, readdirSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const provenanceBundle = 'provenance.sigstore.jsonl';
async function sha256(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

export async function verifyReleaseProvenance(root, {
  repository = process.env.GITHUB_REPOSITORY,
  sourceRef = process.env.GITHUB_REF,
  sourceDigest = process.env.GITHUB_SHA,
  run = spawnSync,
  env = process.env,
} = {}) {
  if (!/^[\w.-]+\/[\w.-]+$/u.test(repository ?? '') || !/^refs\/(?:heads|tags)\/.+/u.test(sourceRef ?? '') || !/^[a-f0-9]{40}$/u.test(sourceDigest ?? '')) {
    throw new Error('Expected the signing repository, exact source ref and full source commit');
  }
  const bundle = join(root, provenanceBundle);
  if (!lstatSync(bundle).isFile()) throw new Error('Missing Sigstore provenance bundle');
  const manifest = readFileSync(join(root, 'SHA256SUMS'), 'utf8').trim().split('\n');
  const expected = new Map();
  for (const line of manifest) {
    const match = /^([a-f0-9]{64})  (.+)$/u.exec(line);
    if (!match) throw new Error('Malformed release checksum entry');
    const [, digest, name] = match;
    if (basename(name) !== name || /[\\\r\n]/u.test(name) || ['.', '..', 'SHA256SUMS', 'SHA256SUMS.asc', provenanceBundle].includes(name) || expected.has(name)) {
      throw new Error(`Unsafe or duplicate release filename: ${name}`);
    }
    expected.set(name, digest);
  }
  for (const extension of ['.AppImage', '.deb', '.rpm']) {
    if (![...expected.keys()].some(name => name.endsWith(extension))) throw new Error(`No signed Linux ${extension} artifact in the release`);
  }
  const files = [...expected.keys(), 'SHA256SUMS', 'SHA256SUMS.asc'];
  const actual = readdirSync(root);
  if (actual.length !== files.length + 1 || actual.some(name => !files.includes(name) && name !== provenanceBundle)) {
    throw new Error('Release set contains files without signed provenance/checksum coverage');
  }
  for (const name of files) {
    const path = join(root, name);
    if (!lstatSync(path).isFile()) throw new Error(`Unsafe release file: ${name}`);
    if (expected.has(name) && await sha256(path) !== expected.get(name)) throw new Error(`Release checksum mismatch: ${name}`);
    const result = run('gh', ['attestation', 'verify', path,
      '--repo', repository,
      '--bundle', bundle,
      '--signer-workflow', `${repository}/.github/workflows/release.yml`,
      '--source-ref', sourceRef,
      '--source-digest', sourceDigest,
      '--deny-self-hosted-runners',
    ], { env, stdio: 'inherit' });
    if (result.error || result.signal || result.status !== 0) throw new Error(`Signed provenance verification failed: ${name}`);
  }
  return files;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (!process.argv[2]) throw new Error('Usage: node scripts/verify-release-provenance.mjs <release-directory>');
    const files = await verifyReleaseProvenance(resolve(process.argv[2]));
    console.log(`Verified signed provenance for ${files.length} release files.`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
