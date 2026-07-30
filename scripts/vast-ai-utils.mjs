import fs from "node:fs/promises";
import path from "node:path";

import { chromium } from "playwright";

const DEFAULT_BASE_URL = "https://cloud.vast.ai";
const DEFAULT_STORAGE_STATE_PATH = "playwright/.auth/vast-ai.json";
const DEFAULT_ARTIFACT_DIR = "test-results/vast-ai";

export const ROUTES = {
  console: "/",
  billing: "/billing/",
  apiKeys: "/manage-keys/?tab=api-keys",
};

export function optionalEnv(name, defaultValue = undefined) {
  const value = process.env[name] ?? defaultValue;
  if (value === undefined || value === null) return undefined;
  const trimmed = String(value).trim();
  return trimmed === "" ? undefined : trimmed;
}

export function envBool(name, defaultValue = false) {
  const raw = optionalEnv(name);
  if (raw === undefined) return defaultValue;
  return ["1", "true", "yes", "y", "on"].includes(raw.toLowerCase());
}

export function normalizeBaseUrl(baseUrl = DEFAULT_BASE_URL) {
  return String(baseUrl).replace(/\/+$/, "");
}

export function resolveStorageStatePath() {
  return path.resolve(process.cwd(), optionalEnv("VAST_AI_STORAGE_STATE_PATH", DEFAULT_STORAGE_STATE_PATH));
}

export function resolveArtifactDir() {
  return path.resolve(process.cwd(), optionalEnv("VAST_AI_ARTIFACT_DIR", DEFAULT_ARTIFACT_DIR));
}

export function routeUrl(baseUrl, routePath) {
  return new URL(routePath, normalizeBaseUrl(baseUrl)).toString();
}

export async function ensureDir(dirPath) {
  await fs.mkdir(dirPath, { recursive: true });
}

export async function launchBrowserContext({ storageStatePath, headless = false } = {}) {
  if (storageStatePath) {
    await fs.access(storageStatePath).catch(() => {
      throw new Error(
        `Saved Vast.ai browser session not found at ${storageStatePath}. Open the managed Vast.ai login browser first to create the session, then retry this action.`,
      );
    });
  }

  const browser = await chromium.launch({ channel: "chrome", headless });
  const context = await browser.newContext({
    storageState: storageStatePath,
    viewport: { width: 1440, height: 1024 },
    ignoreHTTPSErrors: true,
  });
  const page = await context.newPage();
  return { browser, context, page };
}

function isAuthUrl(url) {
  return ["/create/", "/login", "/signup", "/auth"].some(segment =>
    String(url || "").toLowerCase().includes(segment),
  );
}

export async function waitForAuthenticatedShell(page, timeoutMs = 300_000) {
  const candidates = [
    page.getByText("Log out").first(),
    page.getByText("Credit:").first(),
    page.getByText("Add Credit").first(),
    page.getByText("Automatic Top-up").first(),
    page.getByText("API Keys").first(),
  ];
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const currentUrl = page.url();
    if (!isAuthUrl(currentUrl)) {
      for (const locator of candidates) {
        if (await locator.isVisible().catch(() => false)) {
          return true;
        }
      }
    }
    await page.waitForTimeout(500);
  }
  return false;
}

export async function ensureAuthenticated(page, baseUrl) {
  await page.goto(routeUrl(baseUrl, ROUTES.billing), { waitUntil: "domcontentloaded" });
  await page.waitForLoadState("networkidle").catch(() => undefined);
  if (isAuthUrl(page.url())) {
    throw new Error(`Saved Vast.ai browser session is not authenticated. Landed on ${page.url()}`);
  }
  const ready = await waitForAuthenticatedShell(page, 20_000);
  if (!ready) {
    throw new Error(`Authenticated Vast.ai markers were not visible on ${page.url()}`);
  }
}

export async function captureArtifacts(page, prefix, artifactDir) {
  await ensureDir(artifactDir);
  const screenshotPath = path.join(artifactDir, `${prefix}.png`);
  const htmlPath = path.join(artifactDir, `${prefix}.html`);
  await page.screenshot({ path: screenshotPath, fullPage: true }).catch(() => undefined);
  await fs.writeFile(htmlPath, await page.content(), "utf8").catch(() => undefined);
  return { screenshotPath, htmlPath };
}

export async function saveJson(filePath, payload) {
  await ensureDir(path.dirname(filePath));
  await fs.writeFile(filePath, JSON.stringify(payload, null, 2), "utf8");
}

export async function removeFileIfExists(filePath) {
  await fs.rm(filePath, { force: true }).catch(() => undefined);
}

export async function collectJsonResponses(page, filter = () => true) {
  const responses = [];
  page.on("response", async response => {
    const url = response.url();
    if (!filter(response, url)) return;
    const contentType = response.headers()["content-type"] || "";
    if (!contentType.includes("application/json")) return;
    const text = await response.text().catch(() => "");
    let body = text;
    try {
      body = text ? JSON.parse(text) : null;
    } catch {}
    responses.push({ url, status: response.status(), body });
  });
  return responses;
}

export function summarizeVisibleText(text) {
  return String(text || "")
    .replace(/\s+/g, " ")
    .replace(/\u00a0/g, " ")
    .trim();
}

export function deepFindSecretCandidate(value) {
  if (typeof value === "string") {
    const normalized = value.trim();
    if (normalized.length >= 20 && !normalized.includes(" ")) {
      return normalized;
    }
    return null;
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = deepFindSecretCandidate(item);
      if (found) return found;
    }
    return null;
  }
  if (value && typeof value === "object") {
    for (const [key, nested] of Object.entries(value)) {
      const lower = key.toLowerCase();
      if (["api_key", "apikey", "token", "secret", "key"].includes(lower) && typeof nested === "string" && nested.trim()) {
        return nested.trim();
      }
      const found = deepFindSecretCandidate(nested);
      if (found) return found;
    }
  }
  return null;
}

export function maskSecret(secret) {
  if (!secret || secret.length < 8) return "<hidden>";
  return `${secret.slice(0, 4)}…${secret.slice(-4)}`;
}

export async function waitForBrowserClose(browser) {
  await new Promise(resolve => browser.on("disconnected", resolve));
}

export { DEFAULT_ARTIFACT_DIR, DEFAULT_BASE_URL, DEFAULT_STORAGE_STATE_PATH };
