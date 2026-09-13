#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const version = process.argv[2]?.trim().replace(/^v/u, "");
if (!version || !/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/u.test(version)) {
  console.error("Usage: node scripts/set-release-version.mjs <semver>");
  process.exit(1);
}

for (const relativePath of ["package.json", "src-tauri/tauri.conf.json"]) {
  const path = resolve(relativePath);
  const value = JSON.parse(readFileSync(path, "utf8"));
  value.version = version;
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

console.log(`Configured release build version ${version}`);
