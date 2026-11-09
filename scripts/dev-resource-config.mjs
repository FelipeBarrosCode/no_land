import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';

const optional = new Map([
  ['../state-agent/prebuilt/*/agents.tar.gz', 'agents.tar.gz'],
  ['../state-agent/prebuilt/*/SHA256SUMS', 'SHA256SUMS'],
]);

// Only development may omit unavailable release archives. Keep native media,
// tools, agent sources and all caller/platform resource overrides intact.
export function devResourceConfig(root, target, args = [], mode = 'dev') {
  if (mode !== 'dev') return null;
  const configDir = join(root, 'src-tauri');
  const configs = [JSON.parse(readFileSync(join(configDir, 'tauri.conf.json'), 'utf8'))];
  const platform = target?.includes('windows') ? 'windows'
    : target?.includes('apple-darwin') ? 'macos' : target?.includes('linux') ? 'linux' : null;
  const platformPath = platform && join(configDir, `tauri.${platform}.conf.json`);
  if (platformPath && existsSync(platformPath)) configs.push(JSON.parse(readFileSync(platformPath, 'utf8')));
  for (let i = 0; i < args.length; i += 1) {
    let value;
    if (args[i] === '--config') value = args[++i];
    else if (args[i].startsWith('--config=')) value = args[i].slice('--config='.length);
    if (!value) continue;
    if (value.trim().startsWith('{')) configs.push(JSON.parse(value));
    else for (const path of value.split(',')) configs.push(JSON.parse(readFileSync(resolve(root, path.trim()), 'utf8')));
  }
  let resources;
  for (const config of configs) {
    if (config.bundle === null) resources = undefined;
    else if (config.bundle && Object.hasOwn(config.bundle, 'resources')) resources = config.bundle.resources;
  }
  if (!resources) return null;
  const prebuilt = join(root, 'state-agent', 'prebuilt');
  const arches = existsSync(prebuilt) ? readdirSync(prebuilt, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => entry.name) : [];
  const keep = (path) => !optional.has(path) || arches.some((arch) => existsSync(join(prebuilt, arch, optional.get(path))));
  const keys = Array.isArray(resources) ? resources : Object.keys(resources);
  if (keys.every(keep)) return null;
  return { bundle: { resources: Array.isArray(resources) ? resources.filter(keep)
    : Object.fromEntries(Object.entries(resources).filter(([path]) => keep(path))) } };
}
