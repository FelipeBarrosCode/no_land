import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';

const configDir = new URL('../src-tauri/', import.meta.url);
const configNames = [
  'tauri.conf.json',
  'tauri.windows.conf.json',
  'tauri.windows.x64.conf.json',
  'tauri.windows.arm64.conf.json',
  'tauri.microsoftstore.conf.json',
];
const configs = Object.fromEntries(configNames.map((name) => [
  name, JSON.parse(readFileSync(new URL(name, configDir), 'utf8')),
]));
const targets = [
  'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu',
  'x86_64-apple-darwin', 'aarch64-apple-darwin',
  'x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc',
];
const tools = ['ssh', 'scp', 'ssh-keygen'];
const runtimeFiles = [
  ...tools.map((tool) => `${tool}.exe`), 'ssh-sk-helper.exe',
  'libcrypto.dll', 'vcruntime140.dll', 'LICENSE.txt', 'ThirdPartyNotices.txt',
  'licenses/openssh/LICENSE',
];
const plugins = [
  'gstcoreelements', 'gstapp', 'gstaudioconvert', 'gstaudioresample',
  'gstopus', 'gstrtp', 'gstudp', 'gstaudiorate', 'gsttypefindfunctions',
  'gstvolume', 'gstwasapi',
];

// Tauri uses JSON Merge Patch (RFC 7396), not concatenation of resource arrays:
// base -> automatic platform config -> CLI --config overlays / TAURI_CONFIG.
function mergePatch(base, patch) {
  if (patch === null || typeof patch !== 'object' || Array.isArray(patch)) {
    return structuredClone(patch);
  }
  const merged = base && typeof base === 'object' && !Array.isArray(base) ? { ...base } : {};
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete merged[key];
    else merged[key] = mergePatch(merged[key], value);
  }
  return merged;
}

function effectiveConfig(overlays = []) {
  return overlays.reduce((config, name) => mergePatch(config, configs[name]), configs['tauri.conf.json']);
}

// Model the Rust glob::glob subset used by these configs, without a dependency,
// native compilation, or running the sidecar/build scripts. Unlike shell/minimatch,
// Rust glob has no brace expansion or extglobs. * is component-local; ** recurses.
function rustGlob(pattern) {
  assert.doesNotMatch(pattern, /[{}()\\]/u, `Unsupported Rust glob syntax: ${pattern}`);
  const components = pattern.split('/');
  const source = components.map((component, index) => {
    if (component === '**') return index === components.length - 1 ? '.*' : '(?:[^/]+/)*';
    assert.ok(!component.includes('**'), `Invalid recursive wildcard: ${pattern}`);
    let result = '';
    for (let i = 0; i < component.length; i += 1) {
      const char = component[i];
      if (char === '*') result += '[^/]*';
      else if (char === '?') result += '[^/]';
      else if (char === '[') {
        const end = component.indexOf(']', i + 1);
        assert.ok(end > i + 1, `Invalid character class: ${pattern}`);
        const members = component.slice(i + 1, end);
        result += `[${members.startsWith('!') ? `^${members.slice(1)}` : members}]`;
        i = end;
      } else result += char.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&');
    }
    return result + (index === components.length - 1 ? '' : '/');
  }).join('');
  return new RegExp(`^${source}$`, 'u');
}

function destination(path) {
  return path.split('/').filter((part) => part !== '.').map((part) => part === '..' ? '_up_' : part).join('/');
}

function expandResources(patterns, files) {
  assert.ok(Array.isArray(patterns), 'Exercise the shipping resource-list destination semantics');
  const destinations = new Map();
  for (const pattern of patterns) {
    const glob = rustGlob(pattern);
    // ResourcePaths skips globbed directories; only a literal directory uses WalkDir.
    const matches = files.filter((path) => glob.test(path)
      || (!pattern.includes('*') && path.startsWith(`${pattern}/`)));
    assert.ok(matches.length > 0, `Unmatched resource pattern: ${pattern}`);
    for (const path of matches) {
      const dest = destination(path);
      assert.ok(!destinations.has(dest),
        `Duplicate destination ${dest}: ${destinations.get(dest)} and ${pattern}`);
      destinations.set(dest, pattern);
    }
  }
  return destinations;
}

function fixture(target) {
  const windows = target.includes('windows');
  const files = tools.map((tool) => `binaries/${tool}-${target}${windows ? '.exe' : ''}`);
  for (const crate of ['vm-cloud-mic-agent', 'network-agent', 'network-contracts']) {
    files.push(`../${crate}/Cargo.toml`, `../${crate}/Cargo.lock`,
      `../${crate}/src/main.rs`, `../${crate}/src/nested/module.rs`);
  }
  files.push('../state-agent/Cargo.toml', '../state-agent/Cargo.lock', '../state-agent/rust-toolchain.toml',
    '../state-agent/crates/agent/src/main.rs', '../state-agent/scripts/install.sh',
    '../state-agent/systemd/agent.service');
  for (const arch of ['x86_64', 'aarch64']) {
    files.push(`../state-agent/prebuilt/${arch}/agents.tar.gz`, `../state-agent/prebuilt/${arch}/SHA256SUMS`);
  }
  if (windows) {
    files.push(`binaries/wintun-${target}.dll`, 'binaries/wintun-LICENSE.txt',
      ...runtimeFiles.map((name) => `binaries/ssh-runtime/${target}/${name}`));
    if (target.startsWith('x86_64-')) {
      const gst = `binaries/gstreamer/${target}`;
      files.push(`${gst}/bin/gstreamer-1.0-0.dll`, `${gst}/libexec/gstreamer-1.0/gst-plugin-scanner.exe`,
        ...plugins.map((plugin) => `${gst}/lib/gstreamer-1.0/${plugin}.dll`));
    }
  }
  return files;
}

function checkBundle(config, target) {
  const bundled = expandResources(config.bundle.resources, fixture(target));
  const windows = target.includes('windows');
  for (const tool of tools) {
    assert.ok(bundled.has(`binaries/${tool}-${target}${windows ? '.exe' : ''}`), `Missing ${tool} for ${target}`);
  }
  if (windows) {
    for (const name of runtimeFiles) {
      assert.ok(bundled.has(`binaries/ssh-runtime/${target}/${name}`), `Missing private runtime file: ${name}`);
    }
    assert.ok(bundled.has('binaries/wintun-LICENSE.txt'));
    assert.ok(bundled.has(`binaries/wintun-${target}.dll`));
  }
  return bundled;
}

test('fixtures cover every Tauri config and all six release targets', () => {
  assert.deepEqual(readdirSync(configDir).filter((name) => /^tauri(?:\..+)?\.conf\.json$/u.test(name)).sort(),
    [...configNames].sort());
  const workflow = readFileSync(new URL('../.github/workflows/_build.yml', import.meta.url), 'utf8');
  assert.deepEqual([...new Set([...workflow.matchAll(/^\s+target: (\S+)$/gmu)].map((match) => match[1]))].sort(),
    [...targets].sort());
});

test('merge patches replace arrays while preserving unrelated bundle settings', () => {
  const windows = effectiveConfig(['tauri.windows.conf.json', 'tauri.windows.x64.conf.json']);
  assert.deepEqual(windows.bundle.resources, configs['tauri.windows.x64.conf.json'].bundle.resources);
  assert.deepEqual(windows.bundle.externalBin, configs['tauri.conf.json'].bundle.externalBin);
  assert.deepEqual(mergePatch(windows, { bundle: { resources: [] } }).bundle.resources, []);
});

test('glob model uses Rust character classes, component separators and recursive matching', () => {
  assert.ok(rustGlob('binaries/ssh-[ax]*').test('binaries/ssh-aarch64-apple-darwin'));
  assert.ok(rustGlob('binaries/ssh-[ax]*').test('binaries/ssh-x86_64-unknown-linux-gnu'));
  assert.ok(!rustGlob('binaries/ssh-[ax]*').test('binaries/ssh-keygen-x86_64-unknown-linux-gnu'));
  assert.ok(!rustGlob('binaries/ssh-*').test('binaries/ssh-runtime/target/ssh.exe'));
  assert.ok(rustGlob('binaries/ssh-runtime/**/*').test('binaries/ssh-runtime/target/licenses/LICENSE'));
  assert.ok(rustGlob('binaries/ssh-runtime/**/*').test('binaries/ssh-runtime/LICENSE'));
  assert.throws(() => rustGlob('binaries/ssh-{aarch64,x86_64}*'), /Unsupported Rust glob syntax/);
  assert.throws(() => expandResources(['binaries/ssh-x86_64-*'], fixture('aarch64-unknown-linux-gnu')),
    /Unmatched resource pattern/);
});

test('regression guard detects overlapping ssh/keygen and duplicate plugin destinations', () => {
  assert.throws(() => expandResources(['binaries/ssh-*', 'binaries/ssh-keygen-*'], fixture(targets[0])),
    /Duplicate destination binaries\/ssh-keygen-/);
  assert.throws(() => expandResources(['plugins/*gstopus*.dll', 'plugins/*gstopus*.dll'], ['plugins/gstopus.dll']),
    /Duplicate destination plugins\/gstopus.dll/);
});

for (const target of targets.filter((target) => !target.includes('windows'))) {
  test(`${target}: base resources match a single staged target exactly once without a Windows runtime`, () => {
    checkBundle(effectiveConfig(), target);
  });
}

for (const target of targets.filter((target) => target.includes('windows'))) {
  const archConfig = target.startsWith('aarch64-') ? 'tauri.windows.arm64.conf.json' : 'tauri.windows.x64.conf.json';
  for (const overlays of [
    ['tauri.windows.conf.json'],
    ['tauri.windows.conf.json', archConfig],
    ['tauri.windows.conf.json', 'tauri.microsoftstore.conf.json'],
    ['tauri.windows.conf.json', archConfig, 'tauri.microsoftstore.conf.json'],
    ['tauri.windows.conf.json', 'tauri.microsoftstore.conf.json', archConfig],
  ]) {
    test(`${target}: ${overlays.join(' -> ')} preserves tools, runtime and licenses without duplicates`, () => {
      const bundled = checkBundle(effectiveConfig(overlays), target);
      if (overlays.at(-1) === 'tauri.windows.x64.conf.json') {
        for (const plugin of plugins) {
          assert.ok(bundled.has(`binaries/gstreamer/${target}/lib/gstreamer-1.0/${plugin}.dll`), `Missing ${plugin}`);
        }
      }
    });
  }
}
