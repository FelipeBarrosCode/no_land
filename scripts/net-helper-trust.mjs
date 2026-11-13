import { spawnSync } from 'node:child_process';
import { realpathSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const stapler = fileURLToPath(new URL('./apple-stapler-retry.mjs', import.meta.url));
const execute = (command, args) => spawnSync(command, args, { encoding: 'utf8', timeout: 120_000 });

function signingIdentity(details, label) {
  const team = details.match(/^TeamIdentifier=(.+)$/mu)?.[1].trim();
  if (!team || team === 'not set' || /Signature=adhoc/u.test(details)
      || !/^Authority=Developer ID Application:/mu.test(details)) {
    throw new Error(`${label} requires a Developer ID Application signature and TeamIdentifier, not ad-hoc signing`);
  }
  if (!/^Timestamp=(?!none\b)\S.+$/mu.test(details)) throw new Error(`${label} is missing a trusted signing timestamp`);
  if (!/flags=0x[\da-f]+\([^\r\n)]*\bruntime\b/iu.test(details)) throw new Error(`${label} is missing hardened runtime signing`);
  return team;
}

/** Read-only release validation. Notarization covers the helper as nested app code. */
export function verifyMacNetHelperTrust(app, helper, { run = execute } = {}) {
  app = realpathSync(app); helper = realpathSync(helper);
  if (!statSync(app).isDirectory() || dirname(helper) !== realpathSync(join(app, 'Contents', 'MacOS')) || !statSync(helper).isFile()) {
    throw new Error('Signed noland-net-helper must be inside the notarized app Contents/MacOS directory');
  }
  const required = (command, args, label) => {
    const result = run(command, args);
    if (result.error || result.status !== 0) throw new Error(`${label} failed: ${result.error?.message || result.stderr || result.stdout || 'no successful exit status'}`);
    return `${result.stdout || ''}\n${result.stderr || ''}`;
  };
  required('codesign', ['--verify', '--strict', '--verbose=2', helper], 'net-helper code signature verification');
  required('codesign', ['--verify', '--deep', '--strict', '--verbose=2', app], 'containing app code signature verification');
  const appTeam = signingIdentity(required('codesign', ['-dv', '--verbose=4', app], 'app signing identity'), 'Containing app');
  const helperTeam = signingIdentity(required('codesign', ['-dv', '--verbose=4', helper], 'net-helper signing identity'), 'noland-net-helper');
  if (appTeam !== helperTeam) throw new Error('noland-net-helper and the containing app have different Developer ID teams');
  required(process.execPath, [stapler, 'validate', app], 'containing app stapled notarization ticket');
  const assessment = required('spctl', ['--assess', '--type', 'execute', '--verbose=2', app], 'containing app Gatekeeper assessment');
  if (!/source=Notarized Developer ID/u.test(assessment)) {
    throw new Error('Gatekeeper did not confirm Notarized Developer ID coverage for the app containing noland-net-helper');
  }
  return { app, helper, team: appTeam, notarizedContainer: true };
}
