import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { promisify } from 'node:util';
import { downloadWithResume } from '../native-download.mjs';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'noland-download-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return join(root, 'sdk.partial');
}

test('timeout retries preserve partial bytes and use a longer bounded timeout', t => {
  const destination = fixture(t);
  let calls = 0;
  downloadWithResume('https://example.test/sdk', destination, {
    run(command, args) {
      calls += 1;
      assert.equal(command, 'curl');
      assert.equal(args[args.indexOf('--max-time') + 1], '900');
      assert.equal(args[args.indexOf('--continue-at') + 1], '-');
      assert.ok(args.includes('--speed-time'));
      assert.ok(!args.includes('--retry'), 'curl retries must not rewind the file');
      if (calls === 1) {
        writeFileSync(destination, 'partial bytes');
        return { status: 28 };
      }
      assert.equal(readFileSync(destination, 'utf8'), 'partial bytes');
      return { status: 0 };
    },
  });
  assert.equal(calls, 2);
});

for (const status of [33, 36]) {
  test(`range failure ${status} retries from an empty file`, t => {
    const destination = fixture(t);
    writeFileSync(destination, 'partial');
    let calls = 0;
    downloadWithResume('https://example.test/sdk', destination, {
      run() {
        if (++calls === 1) return { status };
        assert.equal(existsSync(destination), false);
        return { status: 0 };
      },
    });
    assert.equal(calls, 2);
  });
}

for (const status of [16, 92]) {
  test(`HTTP/2 failure ${status} resumes over HTTP/1.1 without discarding bytes`, t => {
    const destination = fixture(t);
    let calls = 0;
    downloadWithResume('https://example.test/sdk', destination, {
      http1: false,
      run(_command, args) {
        calls += 1;
        assert.equal(args.includes('--http1.1'), calls > 1);
        assert.ok(!args.includes('--insecure'));
        if (calls === 1) {
          writeFileSync(destination, 'partial HTTP/2 bytes');
          return { status };
        }
        assert.equal(readFileSync(destination, 'utf8'), 'partial HTTP/2 bytes');
        return { status: 0 };
      },
    });
    assert.equal(calls, 2);
  });
}

test('HTTP/1.1 can be selected before the first attempt', t => {
  downloadWithResume('https://example.test/sdk', fixture(t), {
    http1: true,
    run(_command, args) {
      assert.ok(args.includes('--http1.1'));
      return { status: 0 };
    },
  });
});

test('repeated failures stop after four attempts', t => {
  let calls = 0;
  assert.throws(() => downloadWithResume('https://example.test/sdk', fixture(t), {
    run() { calls += 1; return { status: 28 }; },
  }), /after 4 attempts \(exit 28\)/);
  assert.equal(calls, 4);
});

test('missing curl fails immediately', t => {
  assert.throws(() => downloadWithResume('https://example.test/sdk', fixture(t), {
    run() { return { error: new Error('spawn curl ENOENT') }; },
  }), /ENOENT/);
});

test('release caches contain only completed SDK downloads with versioned target keys', () => {
  const workflow = readFileSync(new URL('../../.github/workflows/_build.yml', import.meta.url), 'utf8');
  for (const name of ['macOS', 'Windows']) {
    const step = workflow.split(`- name: Cache ${name} GStreamer downloads\n`)[1]?.split('\n      - ')[0];
    assert.ok(step, `${name} download cache is required`);
    assert.match(step, /actions\/cache@[a-f0-9]{40}/);
    assert.match(step, /key: native-download-v1-.*matrix\.target.*env\.GSTREAMER_VERSION/);
    assert.match(step, /path: src-tauri\/\.native-deps\/cache\/gstreamer-.*\/\*\.(pkg|msi)\n/);
    assert.doesNotMatch(step, /\.partial|restore-keys:/);
  }
});

test('real curl resumes an interrupted HTTP response without duplicating bytes', async t => {
  const destination = fixture(t);
  const requests = [];
  const server = createServer((request, response) => {
    requests.push(request.headers.range);
    if (requests.length === 1) {
      response.writeHead(200, { 'Content-Length': '10', 'Accept-Ranges': 'bytes' });
      response.end('abc');
    } else {
      response.writeHead(206, { 'Content-Length': '7', 'Content-Range': 'bytes 3-9/10' });
      response.end('defghij');
    }
  });
  t.after(() => server.close());
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const moduleUrl = new URL('../native-download.mjs', import.meta.url).href;
  await promisify(execFile)(process.execPath, ['--input-type=module', '-e',
    `import { downloadWithResume } from ${JSON.stringify(moduleUrl)};
     downloadWithResume(process.argv[1], process.argv[2]);`,
    `http://127.0.0.1:${server.address().port}/sdk`, destination,
  ], { timeout: 15000 });
  assert.deepEqual(requests, [undefined, 'bytes=3-']);
  assert.equal(readFileSync(destination, 'utf8'), 'abcdefghij');
});
