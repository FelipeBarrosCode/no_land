// Offline child-process fixture. Never delegates to any system/Apple command.
import { cpSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

let [command, ...args] = process.argv.slice(2);
// Model the read-only nested-helper trust check's shared stapler CLI offline.
if (command === process.execPath && args[0]?.endsWith('apple-stapler-retry.mjs')) {
  command = 'xcrun';
  args = ['stapler', ...args.slice(1)];
}
const plan = JSON.parse(readFileSync(process.env.APPLE_RETRY_FIXTURE_PLAN, 'utf8'));
const statePath = process.env.APPLE_RETRY_FIXTURE_STATE;
const history = JSON.parse(readFileSync(statePath, 'utf8'));
const artifact = args.find((arg) => /\.(?:app|dmg|zip)$/u.test(arg));
const kind = artifact?.endsWith('.dmg') ? 'dmg' : 'app';
const key = `${command} ${args.slice(0, 2).join(' ')} ${kind}`;
const attempt = history.filter((entry) => entry.key === key).length;
history.push({ key, command, args });
writeFileSync(statePath, JSON.stringify(history));
const outcomes = plan[key] ?? [];
const outcome = outcomes[Math.min(attempt, outcomes.length - 1)] ?? { status: 0 };
if (outcome.stdout) console.log(outcome.stdout);
if (outcome.stderr) console.error(outcome.stderr);
if (outcome.status) process.exit(outcome.status);

switch (command) {
  case 'xcrun':
    if (args[0] === 'stapler' && ['validate', 'staple'].includes(args[1])) break;
    if (args[0] !== 'notarytool') throw new Error('Unexpected xcrun fixture operation');
    if (args[1] === 'submit') {
      console.log(JSON.stringify({ id: `fixture-${kind}`, status: outcome.notaryStatus ?? 'Accepted' }));
    } else if (args[1] === 'log') {
      console.log(JSON.stringify({ status: 'Invalid', issues: [{ message: 'The signature is invalid.' }] }));
    } else throw new Error('Unexpected notarytool fixture operation');
    break;
  case 'ditto': {
    const [source, destination] = args.slice(-2);
    if (args.includes('-c')) writeFileSync(destination, 'fake zip');
    else cpSync(source, destination, { recursive: true });
    break;
  }
  case 'hdiutil':
    if (args[0] === 'create') {
      const destination = args.at(-1);
      mkdirSync(dirname(destination), { recursive: true });
      writeFileSync(destination, 'fake dmg');
    } else if (args[0] === 'attach') {
      const mount = args[args.indexOf('-mountpoint') + 1];
      cpSync(process.env.APPLE_RETRY_FIXTURE_APP, join(mount, 'Fixture.app'), { recursive: true });
    } else if (args[0] !== 'detach') throw new Error('Unexpected hdiutil fixture operation');
    break;
  case 'codesign':
    if (args.includes('-dv')) console.error([
      'CodeDirectory v=20500 size=512 flags=0x10000(runtime)',
      `Authority=Developer ID Application: Fixture (${process.env.APPLE_TEAM_ID})`,
      `TeamIdentifier=${process.env.APPLE_TEAM_ID}`,
      'Timestamp=Nov 15, 2026 at 12:00:00 PM',
    ].join('\n'));
    break;
  case 'spctl':
    console.error('accepted\nsource=Notarized Developer ID');
    break;
  default:
    throw new Error(`Unexpected fixture command: ${command}`);
}
