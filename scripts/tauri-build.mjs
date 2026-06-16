import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { spawnSync } from "child_process";
import crypto from "crypto";

function parseEnvFile(content) {
  const env = {};
  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const eqIndex = line.indexOf("=");
    if (eqIndex === -1) continue;
    const key = line.slice(0, eqIndex).trim();
    let value = line.slice(eqIndex + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    env[key] = value;
  }
  return env;
}

function loadEnvFile(filePath) {
  if (!existsSync(filePath)) return {};
  return parseEnvFile(readFileSync(filePath, "utf8"));
}

function isProbablyBase64(value) {
  return /^[A-Za-z0-9+/=\n\r]+$/.test(value) && value.length > 32;
}

function normalizeCertificateSource(source) {
  if (existsSync(source)) {
    const buffer = readFileSync(source);
    return {
      path: source,
      base64: buffer.toString("base64"),
    };
  }

  const tempDir = mkdtempSync(join(tmpdir(), "noland-cert-"));
  const certPath = join(tempDir, "certificate.p12");
  const normalized = source.replace(/\s+/g, "");
  if (!isProbablyBase64(normalized)) {
    throw new Error(
      "APPLE_CERT_P12 must be either a path to a .p12 file or a base64-encoded .p12 payload",
    );
  }

  const buffer = Buffer.from(normalized, "base64");
  if (buffer.length === 0) {
    throw new Error("APPLE_CERT_P12 could not be decoded as base64");
  }

  writeFileSync(certPath, buffer);
  return {
    path: certPath,
    base64: buffer.toString("base64"),
  };
}

function writeTempFile(prefix, fileName, content) {
  const tempDir = mkdtempSync(join(tmpdir(), prefix));
  const filePath = join(tempDir, fileName);
  writeFileSync(filePath, content, { mode: 0o600 });
  return filePath;
}

function setupAppleNotarization(env) {
  const apiKeyPath = env.APPLE_API_KEY_PATH;
  const apiKeyContent = env.APPLE_API_KEY_CONTENT;

  if (apiKeyPath && existsSync(apiKeyPath)) {
    return env;
  }

  if (!apiKeyContent) {
    return env;
  }

  if (!env.APPLE_API_KEY) {
    throw new Error(
      "APPLE_API_KEY must be set when APPLE_API_KEY_CONTENT is provided",
    );
  }

  const normalizedContent = apiKeyContent.replace(/\\n/g, "\n");
  env.APPLE_API_KEY_PATH = writeTempFile(
    "noland-api-key-",
    `AuthKey_${env.APPLE_API_KEY}.p8`,
    normalizedContent.endsWith("\n")
      ? normalizedContent
      : `${normalizedContent}\n`,
  );

  return env;
}

function setupMacSigning(env) {
  const certSource = env.APPLE_CERT_P12;
  const certPassword = env.APPLE_CERT_PASSWORD;
  if (!certSource || !certPassword) {
    return env;
  }

  const certificate = normalizeCertificateSource(certSource);
  env.APPLE_CERTIFICATE = certificate.base64;
  env.APPLE_CERTIFICATE_PASSWORD = certPassword;

  return env;
}

const rootEnv = {
  ...loadEnvFile(join(process.cwd(), ".env")),
  ...loadEnvFile(join(process.cwd(), ".env.local")),
  ...process.env,
};

const macEnv =
  process.platform === "darwin"
    ? setupAppleNotarization(setupMacSigning({ ...rootEnv }))
    : { ...rootEnv };

const tauriArgs = ["exec", "--", "tauri", "build", ...process.argv.slice(2)];
const result = spawnSync("npm", tauriArgs, {
  stdio: "inherit",
  env: macEnv,
});

if (result.status !== 0) {
  process.exit(result.status ?? 1);
}
