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
  const scoped = /^(stage|restore|verify)-(sidecars|app)$/u.exec(action);
  const scope = scoped?.[2] ?? 'all';
  action = scoped?.[1] ?? action;
  if (!['stage', 'restore', 'verify'].includes(action)) throw new Error('Expected stage, restore or verify');
  const allInputs = windowsSigningInputs(root, target);
  const inputs = scope === 'sidecars' ? allInputs.slice(1) : scope === 'app' ? allInputs.slice(0, 1) : allInputs;
  const receipt = join(root, 'src-tauri', '.native-deps', 'signing-receipts', `${target}.json`);
  if (scope === 'app' && action !== 'verify') {
    manageSigningInputs('verify-sidecars', root, target);
  }
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
    // Each release phase signs a disjoint set. Preserve the sidecar receipt
    // when the final desktop executable is signed after embedding its digest.
    const previous = scope === 'app' ? JSON.parse(readFileSync(receipt, 'utf8')) : {};
    writeFileSync(receipt, `${JSON.stringify({
      ...previous,
      ...Object.fromEntries(inputs.map(source => [relative(root, source), digest(source)])),
    }, null, 2)}\n`);
  }
  return inputs;
}

/** Installed app/sidecars must be the exact signed bytes, not merely present. */
export function verifyInstalledSigningInputs(root, target, { appExecutable, helper, micSender }) {
  const inputs = windowsSigningInputs(root, target);
  const receipt = JSON.parse(readFileSync(join(root, 'src-tauri', '.native-deps', 'signing-receipts', `${target}.json`), 'utf8'));
  const installed = [appExecutable, helper, micSender];
  for (let index = 0; index < inputs.length; index++) {
    const path = installed[index];
    if (!path) throw new Error('Missing installed Windows signing input');
    requireFile(path);
    if (receipt[relative(root, inputs[index])] !== digest(path)) {
      throw new Error(`Installed payload differs from signed release input: ${basename(path)}`);
    }
  }
  return installed;
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
