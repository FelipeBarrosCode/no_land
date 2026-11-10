#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { copyFileSync, lstatSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const targets = new Set(['x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc']);
const digest = path => createHash('sha256').update(readFileSync(path)).digest('hex');
function requireFile(path) {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.size === 0) throw new Error(`Missing or unsafe Windows signing input: ${path}`);
}

export function windowsSigningInputs(root, target) {
  if (!targets.has(target)) throw new Error(`Unsupported Windows signing target: ${target}`);
  return [
    join(root, 'src-tauri', 'target', target, 'release', 'noland-connect.exe'),
    join(root, 'src-tauri', 'binaries', `noland-net-helper-${target}.exe`),
    join(root, 'src-tauri', 'binaries', `noland-mic-sender-${target}.exe`),
  ];
}

export function manageSigningInputs(action, root, target, directory) {
  if (!['stage', 'restore', 'verify'].includes(action)) throw new Error('Expected stage, restore or verify');
  const inputs = windowsSigningInputs(root, target);
  const receipt = join(root, 'src-tauri', '.native-deps', 'signing-receipts', `${target}.json`);
  if (action === 'verify') {
    const expected = JSON.parse(readFileSync(receipt, 'utf8'));
    for (const source of inputs) {
      requireFile(source);
      if (expected[relative(root, source)] !== digest(source)) throw new Error(`Bundling modified a signed executable: ${source}`);
    }
    return inputs;
  }
  if (!directory) throw new Error('An isolated signing staging directory is required');
  const staged = inputs.map(source => join(directory, basename(source)));
  for (const source of action === 'stage' ? inputs : staged) requireFile(source);
  mkdirSync(directory, { recursive: true });
  for (let index = 0; index < inputs.length; index++) {
    copyFileSync(action === 'stage' ? inputs[index] : staged[index], action === 'stage' ? staged[index] : inputs[index]);
  }
  if (action === 'restore') {
    mkdirSync(dirname(receipt), { recursive: true });
    writeFileSync(receipt, `${JSON.stringify(Object.fromEntries(inputs.map(source => [relative(root, source), digest(source)])), null, 2)}\n`);
  }
  return inputs;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const [action, target, directory] = process.argv.slice(2);
  try {
    for (const source of manageSigningInputs(action, root, target, directory)) console.log(`[windows-signing-inputs] ${action}: ${relative(root, source)}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
