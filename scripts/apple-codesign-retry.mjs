#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, writeFileSync } from 'node:fs';
import { delimiter, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { isTransientAppleNetworkFailure, redactAppleCredentials } from './apple-stapler-retry.mjs';

export function isTransientTimestampFailure(output) {
  // A timestamp outage must never hide an invalid identity or trust failure.
  if (/\b(?:invalid|revoked|expired|unauthorized|forbidden|unsigned)\b|not signed|no identity found|errSecInternalComponent|could not be found in the keychain/iu.test(output)) return false;
  return /a timestamp was expected but was not found|timestamp(?:ing)? (?:service|server) (?:is |was )?(?:not available|unavailable)|unable to contact (?:the )?timestamp/iu.test(output)
    || isTransientAppleNetworkFailure(output);
}

export function runCodesignWithRetry(args, {
  spawn = spawnSync,
  sleep = milliseconds => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds),
  logger = console,
  env = process.env,
} = {}) {
  if (!Array.isArray(args) || args.some(arg => typeof arg !== 'string')) throw new Error('codesign arguments must be strings');
  const sign = args.findIndex(arg => arg === '--sign' || arg === '-s');
  const timestampedSigning = sign >= 0 && args[sign + 1] !== '-'
    && !args.includes('--timestamp=none')
    && args.some(arg => arg === '--timestamp' || (arg.startsWith('--timestamp=') && arg !== '--timestamp=none'));
  const maxAttempts = timestampedSigning ? 3 : 1;
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    // Absolute path is essential: invoking the PATH shim here would recurse.
    const result = spawn('/usr/bin/codesign', args, {
      env,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      timeout: 120_000,
      killSignal: 'SIGKILL',
      maxBuffer: 4 * 1024 * 1024,
    });
    const output = redactAppleCredentials([result.stdout, result.stderr].filter(Boolean).join('\n').trim(), env);
    if (output) logger.log(output);
    if (!result.error && !result.signal && result.status === 0) return result;
    const transient = !result.error && !result.signal && isTransientTimestampFailure(output);
    if (!transient || attempt === maxAttempts) {
      const reason = result.error?.code ?? result.signal ?? `exit ${result.status ?? 'unknown'}`;
      throw new Error(`[codesign-retry] codesign failed (${reason}) after ${attempt}/${maxAttempts} attempts${output ? `\n${output}` : ''}`);
    }
    const delay = attempt * 15_000;
    logger.warn(`[codesign-retry] Apple timestamp transport failed; retrying the unchanged signing request in ${delay / 1000}s (${attempt}/${maxAttempts})`);
    sleep(delay);
  }
}

export function codesignRetryEnv(repoRoot, env, { helper = fileURLToPath(import.meta.url) } = {}) {
  const tools = join(repoRoot, 'src-tauri', '.native-deps', 'codesign-retry-tools');
  const shim = join(tools, 'codesign');
  mkdirSync(tools, { recursive: true });
  writeFileSync(join(tools, 'package.json'), '{"type":"module"}\n');
  // JSON-encoded file URLs preserve spaces, apostrophes and every argv boundary.
  writeFileSync(shim, `#!/usr/bin/env node\nimport { codesignCli } from ${JSON.stringify(pathToFileURL(helper).href)};\ncodesignCli(process.argv.slice(2));\n`);
  chmodSync(shim, 0o755);
  return { ...env, PATH: [tools, ...(env.PATH ?? '').split(delimiter).filter(path => path && path !== tools)].join(delimiter) };
}

export function codesignCli(args) {
  try {
    runCodesignWithRetry(args);
  } catch (error) {
    console.error(redactAppleCredentials(error.message));
    process.exitCode = 1;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) codesignCli(process.argv.slice(2));
