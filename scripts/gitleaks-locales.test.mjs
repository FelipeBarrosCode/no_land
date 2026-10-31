import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const config = join(root, '.gitleaks.toml');
const gitleaks = process.env.GITLEAKS_BIN || 'gitleaks';
const generatedKey = (text) => `generated.${createHash('sha256').update(text).digest('hex').slice(0, 16)}`;
const nextKey = generatedKey('Next screen');
// Synthetic, deterministic test data, never a service-issued credential.
const syntheticSecret = createHash('sha256').update('gitleaks locale regression fixture').digest('hex');

function withScanner(run) {
  const temporary = mkdtempSync(join(tmpdir(), 'gitleaks-locales-'));
  const baseline = join(temporary, 'baseline.toml');
  writeFileSync(baseline, '[extend]\nuseDefault = true\n', { mode: 0o600 });
  let sequence = 0;
  try {
    run((files, useBaseline = false) => {
      const directory = join(temporary, `scan-${sequence++}`);
      mkdirSync(directory);
      for (const [path, content] of Object.entries(files)) {
        const destination = join(directory, path);
        mkdirSync(dirname(destination), { recursive: true });
        writeFileSync(destination, content, { mode: 0o600 });
      }
      const report = join(temporary, `report-${sequence}.json`);
      const result = spawnSync(gitleaks, [
        'dir', directory, '--config', useBaseline ? baseline : config,
        '--report-format=json', '--report-path', report,
        '--no-banner', '--no-color', '--redact=100', '--log-level=error',
        '--ignore-gitleaks-allow',
      ], { cwd: directory, encoding: 'utf8', timeout: 35_000, maxBuffer: 4 * 1024 * 1024 });
      // Never include scanner output or finding objects in assertion diagnostics.
      assert.ok(!result.error && [0, 1].includes(result.status),
        'Gitleaks must run successfully (install gitleaks or set GITLEAKS_BIN); output withheld');
      const findings = JSON.parse(readFileSync(report, 'utf8'));
      assert.equal(result.status, findings.length > 0 ? 1 : 0, 'Scanner status must agree with report');
      return findings;
    });
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

function assertGenericCount(findings, expected) {
  assert.equal(findings.filter((finding) => finding.RuleID === 'generic-api-key').length,
    expected, 'Unexpected generic-api-key finding count (values withheld)');
}

test('only cross-property generated IDs in locale JSON are allowlisted, with LF and CRLF', () => {
  withScanner((scan) => {
    const files = {};
    for (const [locale, label] of [['en-US', 'API key'], ['pt-BR', 'Chave API'], ['de', 'API-Token']]) {
      for (const [suffix, newline] of [['lf', '\n'], ['crlf', '\r\n']]) {
        files[`src/locales/${locale}-${suffix}.json`] = [
          '{', `  "label": "${label}",`, `  "${nextKey}": "Next screen"`, '}', '',
        ].join(newline);
      }
    }
    assertGenericCount(scan(files, true), 6);
    assert.equal(scan(files).length, 0, 'Verified cross-property IDs should not be findings');
  });
});

test('real locale findings capture deterministic translation IDs, not secret values', () => {
  const locales = join(root, 'src/locales');
  const files = Object.fromEntries(readdirSync(locales).filter((name) => name.endsWith('.json'))
    .map((name) => [`src/locales/${name}`, readFileSync(join(locales, name), 'utf8')]));
  const english = JSON.parse(files['src/locales/en-US.json']);
  withScanner((scan) => {
    const findings = scan(files, true);
    assert.ok(findings.length > 0, 'Default rules must reproduce the real locale false positives');
    for (const finding of findings) {
      assert.equal(finding.RuleID, 'generic-api-key', 'Unexpected rule in locale baseline');
      assert.equal(finding.EndLine, finding.StartLine + 1, 'Expected adjacent JSON properties');
      const path = Object.keys(files).find((candidate) => finding.File.endsWith(candidate));
      assert.ok(path, 'Expected a known locale file');
      const lines = files[path].split(/\r?\n/);
      const property = lines[finding.EndLine - 1].match(/^\s*"(generated\.[0-9a-f]{16})":/);
      assert.ok(property, 'Captured text must be a generated property name');
      const key = property[1];
      assert.ok(typeof english[key] === 'string' && generatedKey(english[key]) === key,
        'Generated ID must equal the SHA-256 prefix of its English UI text');
    }
    assert.equal(scan(files).length, 0, 'All verified locale false positives should be suppressed');
  });
});

test('realistic secret assignments remain detected inside locale JSON, including generated entries', () => {
  const values = {
    api_key: syntheticSecret,
    access_token: syntheticSecret,
    client_secret: syntheticSecret,
    password: syntheticSecret,
    [generatedKey('API credential instructions')]: `api_key = ${syntheticSecret}`,
  };
  const files = Object.fromEntries(Object.entries(values).map(([key, value], index) => [
    `src/locales/credential-${index}.json`,
        JSON.stringify({ label: 'API key', [nextKey]: 'Next screen', [key]: value }, null, 2),
  ]));
  withScanner((scan) => {
    assertGenericCount(scan(files, true), 10);
    assertGenericCount(scan(files), 5);
  });
});

test('a generated-ID-shaped API key value is not allowlisted', () => {
  const files = { 'src/locales/en-US.json': JSON.stringify({ api_key: nextKey }, null, 2) };
  withScanner((scan) => {
    assertGenericCount(scan(files, true), 1);
    assertGenericCount(scan(files), 1);
  });
});

test('path and exact match structure are both required', () => {
  const crossProperty = `  "label": "API key",\n  "${nextKey}": "Next screen"\n`;
  const files = {
    'src/settings.json': `{\n${crossProperty}}\n`,
    'src/locales/nested/settings.json': `{\n${crossProperty}}\n`,
    'src/locales/settings.txt': crossProperty,
    'src/locales/same-line.json': `{"label": "API key", "${nextKey}": "Next screen"}\n`,
    'src/locales/short-id.json': `{"label": "API key",\n  "generated.0123456789abcde": "Next screen"}\n`,
    'src/locales/long-id.json': `{"label": "API key",\n  "generated.0123456789abcdef0": "Next screen"}\n`,
    'src/locales/nonhex-id.json': `{"label": "API key",\n  "generated.0123456789abcdeg": "Next screen"}\n`,
  };
  withScanner((scan) => {
    assertGenericCount(scan(files, true), 7);
    assertGenericCount(scan(files), 7);
  });
});

test('provider-specific rules still detect synthetic tokens adjacent to allowlisted matches', () => {
  const providerToken = ['ghp', syntheticSecret.slice(0, 36)].join('_');
  const files = {
    'src/locales/en-US.json': `{"label": "API key",\n  "${nextKey}": "${providerToken}"}\n`,
  };
  withScanner((scan) => {
    const baseline = scan(files, true);
    const configured = scan(files);
    assertGenericCount(baseline, 1);
    assertGenericCount(configured, 0);
    for (const findings of [baseline, configured]) {
      assert.equal(findings.filter((finding) => finding.RuleID === 'github-pat').length, 1,
        'The provider-specific rule must remain enabled for locale content');
    }
  });
});
