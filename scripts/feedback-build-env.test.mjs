import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';

const directory = new URL('../.github/workflows/', import.meta.url);
const workflows = readdirSync(directory).filter((name) => name.endsWith('.yml'));

test('every distributable app build receives the feedback API secret at compile time', () => {
  let builds = 0;
  for (const name of workflows) {
    const source = readFileSync(new URL(name, directory), 'utf8');
    if (!/(?:npm run tauri:(?:ios:)?build|npx tauri android build)/.test(source)) continue;
    builds++;
    const globalEnv = source.match(/^env:\n((?:[ \t]+[^\n]*\n|\n)*)/m)?.[1];
    assert.ok(globalEnv, `${name}: workflow-level build environment exists`);
    assert.match(globalEnv, /^  NOLAND_FEEDBACK_API_URL: \$\{\{ secrets\.NOLAND_FEEDBACK_API_URL \}\}$/m, name);
  }
  assert.ok(builds > 0, 'at least one app build was checked');
});

test('release and nightly callers pass secrets to the reusable build', () => {
  for (const name of ['release.yml', 'nightly.yml']) {
    const source = readFileSync(new URL(name, directory), 'utf8');
    const job = source.match(/^  build:\n([\s\S]*?)(?=^  [\w-]+:|$(?![\s\S]))/m)?.[1];
    assert.ok(job, `${name}: build job exists`);
    assert.match(job, /uses: \.\/\.github\/workflows\/_build\.yml/, name);
    assert.match(job, /secrets: inherit/, name);
  }
});
