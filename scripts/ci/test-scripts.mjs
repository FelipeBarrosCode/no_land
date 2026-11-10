#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
// These suites execute Linux/POSIX shell templates or check Unix permissions.
// They run on both Linux and macOS architectures, not via Windows' Git Bash.
const posixOnly = new Set([
  'scripts/appimage-metadata.test.mjs',
  'scripts/desktop-password.test.mjs',
  'scripts/linux-native-deps.test.mjs',
  'scripts/platform-credentials.test.mjs',
  'scripts/provisioning-packages.test.mjs',
  'scripts/remote-agent-artifacts.test.mjs',
  'scripts/sunshine-auth.test.mjs',
]);
const files = ['scripts', 'scripts/ci'].flatMap(directory =>
  readdirSync(new URL(`../../${directory}/`, import.meta.url))
    .filter(name => name.endsWith('.test.mjs'))
    .map(name => `${directory}/${name}`))
  // Gitleaks requires its pinned scanner and stays mandatory in _security.yml.
  .filter(path => path !== 'scripts/gitleaks-locales.test.mjs')
  .filter(path => process.platform !== 'win32' || !posixOnly.has(path))
  .sort();

if (files.length === 0) throw new Error('No script regression tests were discovered');
console.log(`Running ${files.length} no-build script suites on ${process.platform}/${process.arch}`);
const result = spawnSync(process.execPath, ['--test', ...files], { cwd: root, stdio: 'inherit' });
if (result.error) throw result.error;
process.exit(result.status ?? 1);
