import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { verifyMacNetHelperTrust } from './net-helper-trust.mjs';
import { windowsSigningInputs, manageSigningInputs } from './windows-signing-inputs.mjs';

const metadata = (team = 'TEAM123456') => [
  'CodeDirectory v=20500 size=512 flags=0x10000(runtime) hashes=1+7 location=embedded',
  'Authority=Developer ID Application: Release Test (TEAM123456)',
  `TeamIdentifier=${team}`, 'Timestamp=Nov 13, 2026 at 12:00:00 PM', '',
].join('\n');

function fixture(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'noland helper signing ')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const app = join(root, 'Noland Connect.app');
  const macos = join(app, 'Contents/MacOS'); mkdirSync(macos, { recursive: true });
  const helper = join(macos, 'noland-net-helper'); writeFileSync(helper, 'opaque signing fixture, never executed');
  const calls = [];
  const result = { status: 0, stdout: '', stderr: '' };
  const run = (command, args) => {
    calls.push([command, args]);
    if (command === 'codesign' && args.includes('-dv')) return { ...result, stderr: metadata() };
    if (command === 'spctl') return { ...result, stderr: 'accepted\nsource=Notarized Developer ID\n' };
    return result;
  };
  return { root, app, helper, calls, run };
}

test('macOS helper has strict Developer ID verification, matching team, hardened runtime and notarized-container coverage', t => {
  const f = fixture(t);
  assert.equal(verifyMacNetHelperTrust(f.app, f.helper, { run: f.run }).notarizedContainer, true);
  assert.ok(f.calls.some(([command, args]) => command === 'codesign' && args.includes('--strict') && args.at(-1) === f.helper));
  assert.ok(f.calls.some(([, args]) => args.includes('validate') && args.at(-1) === f.app));
  assert.ok(f.calls.some(([command]) => command === 'spctl'));
  assert.ok(f.calls.every(([, args]) => !args.includes('--sign') && !args.includes('submit')), 'validation must not re-sign a notarized artifact or perform a new submission');
});

for (const [name, transform, message] of [
  ['ad-hoc helper', text => text.replace('Authority=Developer ID Application:', 'Authority=Ad Hoc:') + '\nSignature=adhoc', /Developer ID Application/u],
  ['different helper team', text => text.replace('TeamIdentifier=TEAM123456', 'TeamIdentifier=OTHER12345'), /different Developer ID teams/u],
  ['missing trusted timestamp', text => text.replace(/^Timestamp=.+$/mu, ''), /timestamp/u],
  ['missing hardened runtime', text => text.replace('flags=0x10000(runtime)', 'flags=0x0(none)'), /hardened runtime/u],
]) {
  test(`macOS release rejects a ${name} even when the container assessment would pass`, t => {
    const f = fixture(t);
    const run = (command, args) => {
      const result = f.run(command, args);
      return command === 'codesign' && args.includes('-dv') && args.at(-1) === f.helper
        ? { ...result, stderr: transform(result.stderr) } : result;
    };
    assert.throws(() => verifyMacNetHelperTrust(f.app, f.helper, { run }), message);
  });
}

for (const stage of ['helper codesign', 'app codesign', 'stapled ticket', 'Gatekeeper']) {
  test(`macOS ${stage} failure blocks release helper trust`, t => {
    const f = fixture(t);
    const run = (command, args) => {
      const fail = stage === 'helper codesign' ? command === 'codesign' && args.includes('--verify') && args.at(-1) === f.helper
        : stage === 'app codesign' ? command === 'codesign' && args.includes('--verify') && args.at(-1) === f.app
          : stage === 'stapled ticket' ? args.includes('validate') : command === 'spctl';
      return fail ? { status: 1, stderr: 'synthetic trust failure' } : f.run(command, args);
    };
    assert.throws(() => verifyMacNetHelperTrust(f.app, f.helper, { run }), /failed/u);
  });
}

test('a Gatekeeper allow-list result alone cannot substitute for notarization', t => {
  const f = fixture(t);
  const run = (command, args) => command === 'spctl' ? { status: 0, stderr: 'accepted\nsource=Developer ID\n' } : f.run(command, args);
  assert.throws(() => verifyMacNetHelperTrust(f.app, f.helper, { run }), /did not confirm Notarized Developer ID/u);
});

test('an external helper is not treated as nested notarized app code', t => {
  const f = fixture(t); const helper = join(f.root, 'outside-helper'); writeFileSync(helper, 'outside fixture');
  assert.throws(() => verifyMacNetHelperTrust(f.app, helper, { run: f.run }), /inside the notarized app/u);
});

for (const target of ['x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc']) {
  test(`${target}: signing inputs explicitly include the net-helper and detect post-signing helper modification`, t => {
    const f = fixture(t); const inputs = windowsSigningInputs(f.root, target);
    const helper = join(f.root, 'src-tauri/binaries', `noland-net-helper-${target}.exe`);
    assert.ok(inputs.includes(helper));
    for (const path of inputs) mkdirSync(dirname(path), { recursive: true });
    // Use native dirname semantics rather than executing foreign signing tools.
    for (const path of inputs) writeFileSync(path, 'unsigned orchestration fixture');
    const stage = join(f.root, 'signing'); manageSigningInputs('stage', f.root, target, stage);
    manageSigningInputs('restore', f.root, target, stage);
    manageSigningInputs('verify', f.root, target);
    writeFileSync(helper, 'changed helper bytes after signing');
    assert.throws(() => manageSigningInputs('verify', f.root, target), /modified a signed executable/u);
  });
}

test('production signing/notarization and native payload helper verification remain mandatory', () => {
  const read = name => readFileSync(new URL(name, import.meta.url), 'utf8');
  const fix = read('./fix-macos-bundle-deps.mjs');
  assert.match(fix, /join\(appMacosDir, 'noland-net-helper'\)/u);
  assert.match(fix, /explicitMacSidecarFiles/u);
  assert.match(fix, /resignBundle\(appPath,\s*\[[^\]]*\.\.\.explicitMacSidecarFiles[^\]]*\]\)/u);
  assert.match(fix, /args\.push\('--sign', appleSigningIdentity, '--timestamp'\)/u);
  assert.match(fix, /args\.push\('--options', 'runtime'\)/u);
  const wrapper = read('./tauri-with-mic-sidecar.mjs');
  assert.ok(wrapper.indexOf('fix-macos-bundle-deps.mjs') < wrapper.indexOf('notarize-macos-bundle.mjs'));
  assert.ok(wrapper.indexOf('notarize-macos-bundle.mjs') < wrapper.indexOf('Rebuilding updater archive from final notarized app'));
  assert.match(wrapper, /NOLAND_REQUIRE_SIGNED_RELEASE === '1' && !notarizationConfigured/u);
  const verify = read('./verify-bundled-sidecars.mjs');
  assert.match(verify, /verifyMacNetHelperTrust\(root, helper\)/u);
  assert.match(verify, /for \(const executable of \[appExecutable, helper, micSender\]\)/u);
  assert.match(verify, /verify-windows-signatures\.ps1.*'-FilePath', executable/u);
});
