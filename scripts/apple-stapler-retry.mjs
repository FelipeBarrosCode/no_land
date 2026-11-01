#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export function redactAppleCredentials(text, env = process.env) {
  let output = String(text ?? '');
  const secrets = ['APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID']
    .map((key) => env[key]?.trim()).filter(Boolean).sort((a, b) => b.length - a.length);
  for (const secret of secrets) output = output.split(secret).join('[REDACTED]');
  return output;
}

export function isTransientAppleNetworkFailure(output) {
  // A transport error must never override an explicit trust, ticket or notary rejection.
  if (/(?:invalid|rejected|revoked|unsigned|not signed|not notarized|not accepted|unauthorized|forbidden|authentication failed|invalid credentials|ticket (?:not found|missing)|no (?:notarization |stapled )?ticket|could not find.*ticket|signature.*(?:invalid|failed)|(?:invalid|failed).*signature)/iu.test(output)) {
    return false;
  }
  // Do not retry arbitrary NSURLErrorDomain/CloudKit messages or stapler exit 68:
  // TLS/certificate failures, missing tickets and unknown errors must fail closed.
  return /NSURLErrorDomain\s+Code\s*=\s*-(?:1001|1003|1004|1005|1006|1009)\b/iu.test(output)
    || /(?:the network connection was lost|network connection (?:was )?lost|connection (?:reset|timed out)|could not connect to (?:the )?server|could not resolve host|a server with the specified hostname could not be found|request timed out|HTTP(?: status code:|\/\S+)?\s+(?:429|500|502|503|504)\b|service unavailable)/iu.test(output);
}

export function runStaplerWithRetry(action, artifactPath, {
  spawn = spawnSync,
  sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms),
  logger = console,
  env = process.env,
} = {}) {
  if (!['staple', 'validate'].includes(action) || !artifactPath) {
    throw new Error('Usage: node scripts/apple-stapler-retry.mjs <staple|validate> <artifact-path>');
  }
  const maxAttempts = 3;
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    const result = spawn('xcrun', ['stapler', action, '-v', artifactPath], {
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
      encoding: 'utf8',
      timeout: 120_000,
      killSignal: 'SIGKILL',
    });
    const output = redactAppleCredentials(
      [result.stdout?.trim(), result.stderr?.trim()].filter(Boolean).join('\n'), env,
    );
    if (output) logger.log(output);
    if (!result.error && !result.signal && result.status === 0) return;

    // Spawn errors/signals (including our timeout) are not evidence of an Apple outage.
    const transient = !result.error && !result.signal && isTransientAppleNetworkFailure(output);
    if (!transient || attempt === maxAttempts) {
      const reason = result.error?.code ?? result.signal ?? `exit ${result.status ?? 'unknown'}`;
      throw new Error(`[apple-stapler-retry] stapler ${action} failed (${reason}) after ${attempt}/${maxAttempts} attempts`);
    }
    const delayMs = attempt * 15_000;
    logger.warn(`[apple-stapler-retry] Transient Apple network failure during ${action} on attempt ${attempt}/${maxAttempts}; retrying in ${delayMs / 1_000}s`);
    sleep(delayMs);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 4) throw new Error('Usage: node scripts/apple-stapler-retry.mjs <staple|validate> <artifact-path>');
    runStaplerWithRetry(process.argv[2], process.argv[3]);
  } catch (error) {
    console.error(redactAppleCredentials(error.message));
    process.exitCode = 1;
  }
}
