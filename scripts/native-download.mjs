import { spawnSync } from 'node:child_process';
import { rmSync } from 'node:fs';

// Keep partial bytes between attempts against the same URL. curl's built-in
// retry rewinds the output file, wasting progress on large SDK downloads.
export function downloadWithResume(url, destination, {
  run = spawnSync,
  http1 = process.env.NOLAND_NATIVE_DOWNLOAD_HTTP1 === '1',
} = {}) {
  for (let attempt = 1; attempt <= 4; attempt += 1) {
    const result = run('curl', [
      '-fL',
      ...(http1 ? ['--http1.1'] : []),
      '--connect-timeout', '30',
      '--max-time', '900',
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
    if (result.status === 16 || result.status === 92) {
      http1 = true;
    }

    // Servers without range support must get a clean, non-resumed retry.
    // Other failures (timeouts, disconnects, TLS errors) retain progress.
    if (result.status === 33 || result.status === 36) {
      rmSync(destination, { force: true });
    }
    if (attempt === 4) {
      throw new Error(`curl download failed after ${attempt} attempts (exit ${result.status}): ${url}`);
    }
    console.warn(`[bootstrap-native-deps] Retrying download (${attempt}/4): ${url}`);
  }
}
