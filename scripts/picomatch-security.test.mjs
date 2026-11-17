import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';

const require = createRequire(import.meta.url);
const patchedName = '@ryotsu/picomatch';
const patchedVersion = '4.0.7-a';
const override = `npm:${patchedName}@${patchedVersion}`;
const manifest = JSON.parse(readFileSync(new URL('../package.json', import.meta.url)));
const lock = JSON.parse(readFileSync(new URL('../package-lock.json', import.meta.url)));

test('every locked picomatch copy uses the exact reviewed security fork', () => {
  assert.equal(manifest.overrides.picomatch, override);
  const copies = Object.entries(lock.packages)
    .filter(([path]) => path.endsWith('/node_modules/picomatch') || path === 'node_modules/picomatch');
  assert.ok(copies.length > 0);
  for (const [path, pkg] of copies) {
    assert.equal(pkg.name, patchedName, path);
    assert.equal(pkg.version, patchedVersion, path);
    assert.equal(pkg.resolved,
      'https://registry.npmjs.org/@ryotsu/picomatch/-/picomatch-4.0.7-a.tgz', path);
    assert.equal(pkg.integrity,
      'sha512-JroBFEmaejC7szGSfqOhF1QZHLKvX2wTA1+fWyfkpy3BjzdQKNPLKa2FtgeKVUDy5/UPUNkXEeCHJ5ZRBMLTVA==', path);
  }
});

for (const consumer of ['vite', 'tinyglobby']) {
  const consumerRequire = createRequire(require.resolve(consumer));
  const fdirRequire = createRequire(consumerRequire.resolve('fdir'));
  for (const [label, resolveFrom] of [[consumer, consumerRequire], [`${consumer}/fdir`, fdirRequire]]) {
    test(`${label} resolves the patched implementation and preserves ordinary globs`, () => {
      const pkg = resolveFrom('picomatch/package.json');
      assert.equal(pkg.name, patchedName);
      assert.equal(pkg.version, patchedVersion);
      for (const entry of ['picomatch', 'picomatch/posix']) {
        const pm = resolveFrom(entry);
        assert.equal(pm.isMatch('src/components/App.tsx', '**/*.{ts,tsx}'), true);
        assert.equal(pm.isMatch('src/components/App.css', '**/*.{ts,tsx}'), false);
        assert.equal(pm.isMatch('node_modules/foo/index.js', '**/node_modules/**'), true);
        assert.equal(pm.isMatch('src/.hidden.ts', '**/*.ts'), false);
        assert.equal(pm.isMatch('src/.hidden.ts', '**/*.ts', { dot: true }), true);
        assert.equal(pm.isMatch('aaa', '+(*a)'), true);
        assert.equal(pm.isMatch('ab', '+(*a)'), false);
        assert.equal(pm.isMatch('*a*a', '+("*a")'), true);
        assert.equal(pm.isMatch('*a*a', '+(\\*a)'), true);
      }
    });

    test(`${label} rejects ReDoS inputs within a killable subprocess`, () => {
      // An in-process timing assertion cannot interrupt exponential backtracking.
      // Exercise the packages each real consumer loads, not a test-only import.
      const entries = ['picomatch', 'picomatch/posix'].map(entry => resolveFrom.resolve(entry));
      const script = `
        const assert = require('node:assert/strict');
        for (const entry of ${JSON.stringify(entries)}) {
          const pm = require(entry);
          for (const options of [{}, { capture: true }, { dot: true }, { windows: true }, { bash: true }]) {
            for (const pattern of ['+(*a)', '*(*a)', '+(*a)c', 'x+(*a)', '+(a*)', '+(@(*a))']) {
              assert.equal(pm.isMatch('a'.repeat(64) + '!', pattern + 'z', options), false);
            }
            for (const pattern of ['+(*a)', '*(*a)', '+(*a)c', 'x+(*a)', '+(@(*a))']) {
              assert.equal(pm.isMatch('a'.repeat(64) + 'b', pattern, options), false);
            }
          }
        }
      `;
      const result = spawnSync(process.execPath, ['-e', script], {
        encoding: 'utf8',
        timeout: 5000,
        windowsHide: true,
      });
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stderr || result.stdout);
    });
  }
}
