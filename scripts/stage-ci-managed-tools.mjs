#!/usr/bin/env node
import { appendFileSync, chmodSync, copyFileSync, existsSync, mkdirSync, rmSync, statSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
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

if (target.includes('apple-darwin')) {
  stageMacosTools(target);
  process.exit(0);
}

console.log(`No CI managed-tool staging required for ${target}`);
process.exit(0);

function stageLinuxTools(targetTriple) {
  if (process.platform !== 'linux') {
    fail(`Linux CI managed-tool staging for ${targetTriple} must run on a Linux host.`);
  }

  const envAssignments = {
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

  stageLinuxToolRuntime(Object.values(envAssignments), targetTriple);
  writeGitHubEnv(envAssignments);
  logAssignments(envAssignments);
}

function stageLinuxToolRuntime(seedExecutables, targetTriple) {
  const destination = join(repoRoot, 'src-tauri', 'binaries', 'ssh-runtime', targetTriple);
  rmSync(destination, { recursive: true, force: true });
  mkdirSync(destination, { recursive: true });

  const queue = [...seedExecutables];
  const scanned = new Set();
  const copied = new Set();
  while (queue.length > 0) {
    const current = queue.pop();
    if (!current || scanned.has(current)) continue;
    scanned.add(current);

    const result = spawnSync('ldd', [current], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    if (result.status !== 0) {
      fail(`Unable to inspect Linux runtime dependencies for ${current}: ${result.stderr?.trim() || `ldd exited ${result.status}`}`);
    }
    if (/not found/u.test(result.stdout)) {
      fail(`Linux managed tool has unresolved dependencies: ${current}\n${result.stdout}`);
    }

    for (const dependency of parseLddDependencies(result.stdout)) {
      if (isLinuxBaseRuntime(dependency) || copied.has(dependency)) continue;
      copied.add(dependency);
      const output = join(destination, basename(dependency));
      copyFileSync(dependency, output);
      chmodSync(output, statSync(dependency).mode);
      queue.push(dependency);
    }
  }

  console.log(`Staged ${copied.size} non-base OpenSSH runtime libraries in ${destination}`);
}

function parseLddDependencies(output) {
  return output
    .split(/\r?\n/u)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const mapped = line.match(/=>\s+(\/[^\s]+)\s+\(/u);
      if (mapped) return mapped[1];
      const direct = line.match(/^(\/[^\s]+)\s+\(/u);
      return direct ? direct[1] : null;
    })
    .filter((path) => path && existsSync(path));
}

function isLinuxBaseRuntime(path) {
  return [
    /^ld-linux/u,
    /^ld-musl/u,
    /^libc\.so/u,
    /^libm\.so/u,
    /^libpthread\.so/u,
    /^libdl\.so/u,
    /^librt\.so/u,
    /^libresolv\.so/u,
    /^libutil\.so/u,
  ].some((pattern) => pattern.test(basename(path)));
}

function stageWindowsTools(targetTriple) {
  if (process.platform !== 'win32') {
    fail(`Windows CI managed-tool staging for ${targetTriple} must run on a Windows host.`);
  }

  const envAssignments = {
    NOLAND_WINTUN_DLL: stageWintun(targetTriple),
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

function stageMacosTools(targetTriple) {
  if (process.platform !== 'darwin') {
    fail(`macOS CI managed-tool staging for ${targetTriple} must run on a macOS host.`);
  }

  const envAssignments = {
    NOLAND_SSH_BIN: locateFirstExisting([
      process.env.NOLAND_SSH_BIN,
      '/usr/bin/ssh',
      which('ssh'),
    ], 'Unable to locate ssh on the macOS runner'),
    NOLAND_SCP_BIN: locateFirstExisting([
      process.env.NOLAND_SCP_BIN,
      '/usr/bin/scp',
      which('scp'),
    ], 'Unable to locate scp on the macOS runner'),
    NOLAND_SSH_KEYGEN_BIN: locateFirstExisting([
      process.env.NOLAND_SSH_KEYGEN_BIN,
      '/usr/bin/ssh-keygen',
      which('ssh-keygen'),
    ], 'Unable to locate ssh-keygen on the macOS runner'),
  };

  writeGitHubEnv(envAssignments);
  logAssignments(envAssignments);
}

function stageWintun(targetTriple) {
  const explicit = process.env.NOLAND_WINTUN_DLL?.trim();
  if (explicit && existsSync(explicit)) {
    return explicit;
  }

  const version = '0.14.1';
  const archDir = targetTriple.includes('aarch64') ? 'arm64' : 'amd64';
  const cacheRoot = join(repoRoot, 'src-tauri', '.native-deps', 'cache', `wintun-${version}`);
  const zipPath = join(cacheRoot, `wintun-${version}.zip`);
  const extractedRoot = join(cacheRoot, 'extracted');
  const dllPath = join(extractedRoot, 'wintun', 'bin', archDir, 'wintun.dll');
  if (existsSync(dllPath)) {
    return dllPath;
  }

  mkdirSync(cacheRoot, { recursive: true });
  const escapePs = (value) => String(value).replaceAll("'", "''");
  const script = [
    "$ProgressPreference = 'SilentlyContinue'",
    `Invoke-WebRequest -Uri 'https://www.wintun.net/builds/wintun-${version}.zip' -OutFile '${escapePs(zipPath)}'`,
    `if (Test-Path '${escapePs(extractedRoot)}') { Remove-Item -Recurse -Force '${escapePs(extractedRoot)}' }`,
    `Expand-Archive -Path '${escapePs(zipPath)}' -DestinationPath '${escapePs(extractedRoot)}' -Force`,
  ].join('; ');
  run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script]);
  return requireExistingPath(dllPath, `Wintun archive did not contain ${dllPath}`);
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
