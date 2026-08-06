#!/usr/bin/env node
import { appendFileSync, existsSync, mkdirSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');
const target = readTarget(process.argv.slice(2))
  ?? process.env.TAURI_ENV_TARGET_TRIPLE?.trim()
  ?? defaultHostTarget();

if (!target) {
  fail('Unable to determine target triple. Pass --target <triple>.');
}

if (target.includes('linux')) {
  stageLinuxTools(target);
  process.exit(0);
}

if (target.includes('windows')) {
  stageWindowsTools(target);
  process.exit(0);
}

console.log(`No CI managed-tool staging required for ${target}`);
process.exit(0);

function stageLinuxTools(targetTriple) {
  if (process.platform !== 'linux') {
    fail(`Linux CI managed-tool staging for ${targetTriple} must run on a Linux host.`);
  }

  const gotatunPath = buildGotatun(targetTriple);
  const envAssignments = {
    NOLAND_GOTATUN_BIN: gotatunPath,
    NOLAND_WG_BIN: requireExistingPath('/usr/bin/wg', 'wireguard-tools package did not provide /usr/bin/wg'),
    NOLAND_WG_QUICK_BIN: requireExistingPath('/usr/bin/wg-quick', 'wireguard-tools package did not provide /usr/bin/wg-quick'),
    NOLAND_SSH_BIN: locateFirstExisting([
      '/usr/bin/ssh',
      which('ssh'),
    ], 'openssh-client did not provide ssh'),
    NOLAND_SCP_BIN: locateFirstExisting([
      '/usr/bin/scp',
      which('scp'),
    ], 'openssh-client did not provide scp'),
    NOLAND_SSH_KEYGEN_BIN: locateFirstExisting([
      '/usr/bin/ssh-keygen',
      which('ssh-keygen'),
    ], 'openssh-client did not provide ssh-keygen'),
  };

  writeGitHubEnv(envAssignments);
  logAssignments(envAssignments);
}

function stageWindowsTools(targetTriple) {
  if (process.platform !== 'win32') {
    fail(`Windows CI managed-tool staging for ${targetTriple} must run on a Windows host.`);
  }

  const gotatunPath = buildGotatun(targetTriple);
  const programFiles = process.env.ProgramFiles ?? 'C:\\Program Files';
  const wireguardDir = join(programFiles, 'WireGuard');
  const envAssignments = {
    NOLAND_GOTATUN_BIN: gotatunPath,
    NOLAND_WG_BIN: locateFirstExisting([
      process.env.NOLAND_WG_BIN,
      join(wireguardDir, 'wg.exe'),
      ...where('wg.exe'),
    ], 'Unable to locate wg.exe after installing WireGuard'),
    NOLAND_WIREGUARD_EXE_BIN: locateFirstExisting([
      process.env.NOLAND_WIREGUARD_EXE_BIN,
      join(wireguardDir, 'wireguard.exe'),
      ...where('wireguard.exe'),
    ], 'Unable to locate wireguard.exe after installing WireGuard'),
    NOLAND_SSH_BIN: locateFirstExisting([
      process.env.NOLAND_SSH_BIN,
      ...where('ssh.exe'),
      join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'OpenSSH', 'ssh.exe'),
    ], 'Unable to locate ssh.exe on the Windows runner'),
    NOLAND_SCP_BIN: locateFirstExisting([
      process.env.NOLAND_SCP_BIN,
      ...where('scp.exe'),
      join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'OpenSSH', 'scp.exe'),
    ], 'Unable to locate scp.exe on the Windows runner'),
    NOLAND_SSH_KEYGEN_BIN: locateFirstExisting([
      process.env.NOLAND_SSH_KEYGEN_BIN,
      ...where('ssh-keygen.exe'),
      join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'OpenSSH', 'ssh-keygen.exe'),
    ], 'Unable to locate ssh-keygen.exe on the Windows runner'),
  };

  writeGitHubEnv(envAssignments);
  logAssignments(envAssignments);
}

function buildGotatun(targetTriple) {
  const explicit = process.env.NOLAND_GOTATUN_BIN?.trim();
  if (explicit && existsSync(explicit)) {
    console.log(`Using preconfigured gotatun from ${explicit}`);
    return explicit;
  }

  const cacheRoot = join(repoRoot, 'src-tauri', '.native-deps', 'cache', 'gotatun');
  const sourceDir = join(cacheRoot, 'src');
  const cargoTargetDir = join(cacheRoot, 'target');
  const ext = targetTriple.includes('windows') ? '.exe' : '';
  const builtBinary = join(cargoTargetDir, targetTriple, 'release', `gotatun${ext}`);

  mkdirSync(cacheRoot, { recursive: true });

  if (!existsSync(sourceDir)) {
    run('git', ['clone', '--depth', '1', 'https://github.com/mullvad/gotatun.git', sourceDir]);
  } else {
    rmSync(sourceDir, { recursive: true, force: true });
    run('git', ['clone', '--depth', '1', 'https://github.com/mullvad/gotatun.git', sourceDir]);
  }

  run('cargo', [
    'build',
    '--locked',
    '--release',
    '--target', targetTriple,
    '--bin', 'gotatun',
    '--manifest-path', join(sourceDir, 'Cargo.toml'),
  ], {
    cwd: repoRoot,
    env: {
      ...process.env,
      CARGO_TARGET_DIR: cargoTargetDir,
    },
  });

  return requireExistingPath(builtBinary, `gotatun build completed but ${builtBinary} was not produced`);
}

function writeGitHubEnv(assignments) {
  const githubEnv = process.env.GITHUB_ENV?.trim();
  if (!githubEnv) {
    console.warn('GITHUB_ENV is not set; managed tool paths will only be printed, not exported to later steps.');
    return;
  }

  for (const [name, value] of Object.entries(assignments)) {
    appendFileSync(githubEnv, `${name}<<__NOLAND_EOF__\n${value}\n__NOLAND_EOF__\n`);
  }
}

function logAssignments(assignments) {
  console.log(`Managed tool staging ready for ${target}`);
  for (const [name, value] of Object.entries(assignments)) {
    console.log(`  ${name}=${value}`);
  }
}

function locateFirstExisting(candidates, errorMessage) {
  for (const candidate of candidates) {
    if (!candidate) continue;
    const resolved = String(candidate).trim();
    if (resolved && existsSync(resolved)) {
      return resolved;
    }
  }
  fail(errorMessage);
}

function requireExistingPath(path, errorMessage) {
  if (!existsSync(path)) {
    fail(errorMessage);
  }
  return path;
}

function which(command) {
  const result = spawnSync('sh', ['-lc', `command -v ${shellEscape(command)}`], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (result.status !== 0) {
    return null;
  }
  const resolved = result.stdout.trim().split(/\r?\n/).find(Boolean);
  return resolved || null;
}

function where(command) {
  const result = spawnSync('where.exe', [command], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (result.status !== 0) {
    return [];
  }
  return result.stdout
    .split(/\r?\n/)
    .map((entry) => entry.trim())
    .filter(Boolean)
    .filter((entry) => existsSync(entry));
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    stdio: 'inherit',
    ...options,
  });
  if (result.status !== 0) {
    fail(`${command} ${args.join(' ')} failed with exit code ${result.status ?? -1}`);
  }
}

function readTarget(args) {
  for (let i = 0; i < args.length; i += 1) {
    if (args[i] === '--target' && args[i + 1]) {
      return args[i + 1];
    }
    if (args[i].startsWith('--target=')) {
      return args[i].slice('--target='.length);
    }
  }
  return undefined;
}

function defaultHostTarget() {
  if (process.platform === 'darwin') {
    if (process.arch === 'arm64') return 'aarch64-apple-darwin';
    if (process.arch === 'x64') return 'x86_64-apple-darwin';
  }
  if (process.platform === 'linux') {
    if (process.arch === 'arm64') return 'aarch64-unknown-linux-gnu';
    if (process.arch === 'x64') return 'x86_64-unknown-linux-gnu';
  }
  if (process.platform === 'win32') {
    if (process.arch === 'arm64') return 'aarch64-pc-windows-msvc';
    if (process.arch === 'x64') return 'x86_64-pc-windows-msvc';
  }
  return null;
}

function shellEscape(value) {
  return `'${String(value).replaceAll(`'`, `'\\''`)}'`;
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
