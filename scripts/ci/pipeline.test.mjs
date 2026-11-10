import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const workflow = name => readFileSync(new URL(`../../.github/workflows/${name}.yml`, import.meta.url), 'utf8');
const job = (source, name) => {
  const body = source.split(`\n  ${name}:\n`)[1];
  assert.ok(body, `${name} exists`);
  return body.split(/\n  [\w-]+:\n/u)[0];
};
const step = (source, name) => {
  const body = source.split(`      - name: ${name}\n`)[1];
  assert.ok(body, `${name} exists`);
  return body.split('\n      - ')[0];
};

test('package extraction tools are installed in the consuming release job before any filesystem scan', () => {
  const release = workflow('release');
  const packages = job(release, 'package-validation');
  const install = step(packages, 'Install package extraction tools');
  assert.match(install, /apt-get install .*squashfs-tools zstd/u);
  assert.ok(packages.indexOf('Install package extraction tools') < packages.indexOf('Extract scannable package filesystems'));
  assert.doesNotMatch(job(release, 'validation'), /Install package extraction tools/u);
});

test('all six platforms have a required, unprivileged no-build regression lane', () => {
  const ci = workflow('ci');
  const scripts = job(ci, 'script-tests');
  for (const os of ['ubuntu-22.04', 'ubuntu-22.04-arm', 'macos-latest', 'macos-15-intel', 'windows-latest', 'windows-11-arm']) {
    assert.ok(scripts.includes(os), os);
  }
  assert.match(scripts, /fail-fast: false/u);
  assert.match(scripts, /run: npm run test:scripts/u);
  assert.doesNotMatch(scripts, /cargo |cmake |npm run (?:build|tauri)|secrets\.|environment:|contents: write/u);
  assert.match(job(ci, 'required'), /needs: \[.*script-tests/u);
  const runner = readFileSync(new URL('./test-scripts.mjs', import.meta.url), 'utf8');
  assert.match(runner, /spawnSync\(process\.execPath, \['--test', \.\.\.files\]/u);
  assert.doesNotMatch(runner, /spawnSync\(['"](?:cargo|npm|npx|cmake|tauri)/u);
  const attributes = readFileSync(new URL('../../.gitattributes', import.meta.url), 'utf8');
  for (const extension of ['mjs', 'ts', 'tsx', 'json', 'yml', 'yaml']) {
    assert.ok(attributes.includes(`*.${extension} text eol=lf`), extension);
  }
});

test('runtime code and contracts are covered by formatting, linting and dependency security', () => {
  const ci = workflow('ci');
  const formatting = job(ci, 'rust-format');
  for (const manifest of ['runtime-agent/Cargo.toml', 'runtime-contracts/Cargo.toml']) assert.ok(formatting.includes(manifest));
  assert.match(job(ci, 'rust-check'), /manifest: runtime-agent\/Cargo\.toml/u);
  assert.match(job(workflow('_security'), 'dependency-security'), /directory: runtime-agent/u);
  const dependabot = readFileSync(new URL('../../.github/dependabot.yml', import.meta.url), 'utf8');
  for (const directory of ['runtime-agent', 'runtime-contracts']) assert.ok(dependabot.includes(`directory: /${directory}\n`));
});

test('only complete SDK sets survive later build failures; partial files and signing material are not cached', () => {
  const build = workflow('_build');
  for (const platform of ['macOS', 'Windows']) {
    const restore = step(build, `Cache ${platform} GStreamer downloads`);
    const save = step(build, `Save completed ${platform} GStreamer downloads`);
    assert.match(restore, /actions\/cache\/restore@[a-f0-9]{40}/u);
    assert.match(save, /actions\/cache\/save@[a-f0-9]{40}/u);
    assert.match(save, /always\(\)/u);
    assert.match(save, /cache-hit != 'true'/u);
    assert.match(save, /cache-primary-key/u);
    assert.equal([...save.matchAll(/hashFiles\(format\(/gu)].length, 2, 'runtime and development packages are both required');
    assert.match(save, /gstreamer-1\.0-devel-/u);
    assert.doesNotMatch(restore + save, /\.partial|restore-keys:|\.p12|\.keychain|release\/bundle/u);
  }
});

test('Windows native setup failures stop before staging or changing the release version', () => {
  const windows = job(workflow('_build'), 'build-windows');
  const setup = step(windows, 'Install dependencies and stage managed tools');
  const commands = setup.split('\n').map(line => line.trim()).filter(Boolean);
  for (let index = 0; index < commands.length; index += 1) {
    if (/^(npm ci|node )/u.test(commands[index])) {
      assert.equal(commands[index + 1], 'if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }', commands[index]);
    }
  }
  const credentials = step(windows, 'Require Artifact Signing credentials');
  assert.match(credentials, /TAURI_SIGNING_PRIVATE_KEY/u);
  assert.ok(windows.indexOf('Require Artifact Signing credentials') < windows.indexOf('Build Windows Tauri application'));
});
