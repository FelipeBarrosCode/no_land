import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { resolveOpenSshRoot, stageOpenSshRuntime, verifyOpenSshRuntime } from './windows-openssh.mjs';

// Pin both the upstream release and archive digest; never bundle the runner's
// System32 OpenSSH, whose private DLLs are not necessarily redistributable there.
const version = '10.4.0.0p1-Preview';
const distributions = {
  'x86_64-pc-windows-msvc': {
    directory: 'OpenSSH-Win64',
    sha256: 'b67ff8becd203502190ec42ac7787c24c2cd3325ed7700485fe7a1c83d2a7957',
  },
  'aarch64-pc-windows-msvc': {
    directory: 'OpenSSH-ARM64',
    sha256: 'd2b1928225479fec9e0c938f527fdb5ed676446cca1bbcc5af84aca1f272842e',
  },
};

export function openSshDistribution(target) {
  const distribution = distributions[target];
  if (!distribution) throw new Error(`Unsupported Windows OpenSSH target: ${target}`);
  return {
    ...distribution,
    version,
    url: `https://github.com/PowerShell/Win32-OpenSSH/releases/download/${version}/${distribution.directory}.zip`,
  };
}

export function verifyOpenSshArchive(path, expectedSha256) {
  const actual = createHash('sha256').update(readFileSync(path)).digest('hex');
  if (actual !== expectedSha256) throw new Error(`OpenSSH archive checksum mismatch: ${path}`);
}

export function ensureWindowsOpenSshRoot(target, srcTauriDir, { env = process.env, install = installOpenSsh } = {}) {
  const distribution = openSshDistribution(target);
  if (['NOLAND_OPENSSH_ROOT', 'NOLAND_SSH_BIN', 'NOLAND_SCP_BIN', 'NOLAND_SSH_KEYGEN_BIN'].some((key) => env[key]?.trim())) {
    // Explicit distributions must still be complete and target-native. Do not
    // hide a broken operator override by downloading a different distribution.
    return resolveOpenSshRoot(target, srcTauriDir, env);
  }
  const cache = join(srcTauriDir, '.native-deps', 'cache', `openssh-${distribution.version}-${target}`);
  const root = join(cache, 'root');
  if (!existsSync(root)) {
    mkdirSync(cache, { recursive: true });
    install(distribution, cache, root, target);
  }
  verifyOpenSshRuntime(root, target);
  return root;
}

function installOpenSsh(distribution, cache, root, target) {
  const archive = join(cache, `${distribution.directory}.zip`);
  const extracted = join(cache, 'extracted');
  const quote = (value) => `'${String(value).replaceAll("'", "''")}'`;
  const powershell = (script) => {
    const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
      `$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'; ${script}`],
    { stdio: 'inherit', timeout: 180_000, windowsHide: true });
    if (result.error || result.status !== 0) {
      throw new Error(`OpenSSH provisioning failed: ${result.error?.message || `PowerShell exit ${result.status}`}`);
    }
  };
  powershell(`Invoke-WebRequest -Uri ${quote(distribution.url)} -OutFile ${quote(archive)} -TimeoutSec 120`);
  verifyOpenSshArchive(archive, distribution.sha256);
  rmSync(extracted, { recursive: true, force: true });
  powershell(`Expand-Archive -LiteralPath ${quote(archive)} -DestinationPath ${quote(extracted)} -Force`);
  try {
    stageOpenSshRuntime(join(extracted, distribution.directory), root, target);
  } catch (error) {
    rmSync(root, { recursive: true, force: true });
    throw error;
  } finally {
    rmSync(extracted, { recursive: true, force: true });
  }
}
