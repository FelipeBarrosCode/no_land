import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { windowsZipExtractionScript } from './windows-zip.mjs';

const quote = value => `'${String(value).replaceAll("'", "''")}'`;

function powershell(script) {
  return spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
    encoding: 'utf8',
    timeout: 30_000,
    windowsHide: true,
  });
}

test('ZIP extraction uses only the Windows PowerShell 5.1-compatible overload', () => {
  assert.equal(windowsZipExtractionScript("C:\\cache's [arm64]\\ssh.zip", "C:\\out's [arm64]"),
    "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.IO.Compression.FileSystem; " +
    "[System.IO.Compression.ZipFile]::ExtractToDirectory('C:\\cache''s [arm64]\\ssh.zip', 'C:\\out''s [arm64]')");
});

test('every managed Windows ZIP extractor uses the same compatible API', () => {
  for (const name of ['windows-openssh-install.mjs', 'stage-ci-managed-tools.mjs', 'prepare-mic-sidecar.mjs']) {
    const source = readFileSync(new URL(name, import.meta.url), 'utf8');
    assert.match(source, /import \{ windowsZipExtractionScript \} from '\.\/windows-zip\.mjs'/u);
    assert.match(source, /windowsZipExtractionScript\(/u);
    assert.doesNotMatch(source, /Expand-Archive/u);
  }
});

test('Windows PowerShell extracts real ZIP bytes and rejects corrupt archives', {
  skip: process.platform !== 'win32',
}, () => {
  const root = mkdtempSync(join(tmpdir(), "noland-zip's [fixture]-"));
  try {
    const source = join(root, 'source');
    const archive = join(root, 'runtime.zip');
    const output = join(root, 'extracted');
    mkdirSync(join(source, 'OpenSSH-ARM64'), { recursive: true });
    writeFileSync(join(source, 'OpenSSH-ARM64', 'fixture.txt'), 'archive roundtrip');
    const created = powershell(
      "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.IO.Compression.FileSystem; " +
      `[System.IO.Compression.ZipFile]::CreateFromDirectory(${quote(source)}, ${quote(archive)})`);
    assert.ifError(created.error);
    assert.equal(created.status, 0, created.stderr || created.stdout);

    const extracted = powershell(windowsZipExtractionScript(archive, output));
    assert.ifError(extracted.error);
    assert.equal(extracted.status, 0, extracted.stderr || extracted.stdout);
    assert.equal(readFileSync(join(output, 'OpenSSH-ARM64', 'fixture.txt'), 'utf8'), 'archive roundtrip');

    // Extraction must not silently overwrite a cached runtime.
    const repeated = powershell(windowsZipExtractionScript(archive, output));
    assert.ifError(repeated.error);
    assert.notEqual(repeated.status, 0);
    assert.equal(readFileSync(join(output, 'OpenSSH-ARM64', 'fixture.txt'), 'utf8'), 'archive roundtrip');

    writeFileSync(archive, 'not a ZIP');
    const invalid = powershell(windowsZipExtractionScript(archive, join(root, 'invalid')));
    assert.ifError(invalid.error);
    assert.notEqual(invalid.status, 0, 'corrupt archives must fail the staging step');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
