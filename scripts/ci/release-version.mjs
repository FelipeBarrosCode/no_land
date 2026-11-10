#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { appendFileSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const stableVersion = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$(?![\s\S])/u;

export function nextReleaseVersion(configured, tags) {
  if (!stableVersion.test(configured)) throw new Error(`Expected a stable configured release version: ${configured}`);
  let latest = configured.split('.').map(BigInt);
  for (const tag of tags) {
    if (!tag.startsWith('v') || !stableVersion.test(tag.slice(1))) continue;
    const version = tag.slice(1).split('.').map(BigInt);
    const difference = version.findIndex((part, index) => part !== latest[index]);
    if (difference >= 0 && version[difference] > latest[difference]) latest = version;
  }
  latest[2] += 1n;
  return latest.join('.');
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { version: configured } = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
  // Filter exact stable tags in JavaScript. Shell globs also match prereleases,
  // while `git tag | head` can fail with SIGPIPE under Actions' pipefail.
  const tags = execFileSync('git', ['tag', '--list'], { encoding: 'utf8' }).trim().split(/\r?\n/u);
  const version = nextReleaseVersion(configured, tags);
  const output = `release_tag=v${version}\nrelease_name=v${version}\nversion=${version}\n`;
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, output);
  else process.stdout.write(output);
}
