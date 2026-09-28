#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { relative, resolve } from 'node:path';

const root = resolve(process.argv[2] ?? '');
const output = resolve(process.argv[3] ?? 'SHA256SUMS');
if (!process.argv[2] || !existsSync(root) || !statSync(root).isDirectory()) {
  console.error('Usage: node scripts/ci/hash-artifacts.mjs <artifact-directory> [output-file]');
  process.exit(2);
}

const files = [];
const pending = [root];
while (pending.length > 0) {
  const current = pending.pop();
  for (const entry of readdirSync(current, { withFileTypes: true })) {
    const path = resolve(current, entry.name);
    if (entry.isDirectory()) pending.push(path);
    else if (entry.isFile() && path !== output) files.push(path);
  }
}
files.sort();
if (files.length === 0) {
  console.error(`No files found under ${root}`);
  process.exit(1);
}

const lines = [];
for (const file of files) {
  const digest = createHash('sha256');
  await new Promise((accept, reject) => {
    const stream = createReadStream(file);
    stream.on('data', (chunk) => digest.update(chunk));
    stream.on('error', reject);
    stream.on('end', accept);
  });
  lines.push(`${digest.digest('hex')}  ${relative(root, file).replaceAll('\\', '/')}`);
}
writeFileSync(output, `${lines.join('\n')}\n`);
console.log(`Hashed ${files.length} artifact files into ${output}`);
