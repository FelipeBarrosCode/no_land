import { spawnSync } from 'node:child_process';
import { rmSync } from 'node:fs';

const sleepSync = milliseconds => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds);

// Keep partial bytes between attempts against the same URL. curl's built-in
// retry rewinds the output file, wasting progress on large SDK downloads.
export function downloadWithResume(url, destination, {
  run = spawnSync,
  http1 = process.env.NOLAND_NATIVE_DOWNLOAD_HTTP1 === '1',
  sleep = sleepSync,
  now = Date.now,
} = {}) {
  const attempts = 12;
  const deadline = now() + 30 * 60 * 1000;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const remaining = deadline - now();
    if (remaining < 1000) throw new Error(`curl download exceeded its 30-minute retry budget: ${url}`);
    const result = run('curl', [
      '-fL',
      ...(http1 ? ['--http1.1'] : []),
      '--connect-timeout', '30',
      '--max-time', String(Math.min(900, Math.floor(remaining / 1000))),
      '--speed-limit', '1024',
      '--speed-time', '60',
      '--continue-at', '-',
      '-o', destination,
      url,
    ], { stdio: 'inherit' });
    if (result.error) throw result.error;
    if (result.status === 0) return;

    // Some CDN/proxy paths negotiate HTTP/2 but reset large SDK streams.
    // Resume the same artifact over HTTP/1.1 without weakening TLS checks.
    if ([16, 18, 92].includes(result.status)) {
      http1 = true;
    }

    // Servers without range support must get a clean, non-resumed retry.
    // Other failures (timeouts, disconnects, TLS errors) retain progress.
    if (result.status === 33 || result.status === 36) {
      rmSync(destination, { force: true });
    }
    if (result.signal || result.status === null) {
      throw new Error(`curl download interrupted (${result.signal || 'no exit status'}): ${url}`);
    }
    if (attempt === attempts) {
      throw new Error(`curl download failed after ${attempt} attempts (exit ${result.status}): ${url}`);
    }
    const delay = Math.min(30_000, 1000 * 2 ** (attempt - 1));
    if (deadline - now() <= delay) throw new Error(`curl download exceeded its 30-minute retry budget: ${url}`);
    console.warn(`[bootstrap-native-deps] Retrying download (${attempt}/${attempts}) in ${delay / 1000}s: ${url}`);
    sleep(delay);
  }
}
