import path from "node:path";

import {
  DEFAULT_BASE_URL,
  ROUTES,
  captureArtifacts,
  collectJsonResponses,
  deepFindSecretCandidate,
  ensureAuthenticated,
  envBool,
  launchBrowserContext,
  maskSecret,
  normalizeBaseUrl,
  resolveArtifactDir,
  resolveStorageStatePath,
  routeUrl,
  saveJson,
  summarizeVisibleText,
} from "./vast-ai-utils.mjs";

async function maybeToggleBillingEarning(dialog) {
  const row = dialog.locator("tr").filter({ hasText: /billing\/earning/i }).first();
  if (!(await row.isVisible().catch(() => false))) return;
  const checkbox = row.locator('input[type="checkbox"]').nth(1);
  if (await checkbox.isVisible().catch(() => false)) {
    const checked = await checkbox.isChecked().catch(() => false);
    if (!checked) await checkbox.click();
  }
}

async function main() {
  const baseUrl = normalizeBaseUrl(process.env.VAST_AI_BASE_URL || DEFAULT_BASE_URL);
  const storageStatePath = resolveStorageStatePath();
  const artifactDir = resolveArtifactDir();
  const headless = envBool("VAST_AI_HEADLESS", true);
  const apiKeyName = (process.env.VAST_AI_API_KEY_NAME || `noland-${Date.now()}`).trim();
  const require2fa = envBool("VAST_AI_API_KEY_REQUIRE_2FA", false);
  const resultPath = path.join(artifactDir, "vast-ai-api-key-result.json");

  const { browser, page } = await launchBrowserContext({ headless, storageStatePath });
  try {
    const responses = await collectJsonResponses(page, (_response, url) => url.includes("/api/"));
    await ensureAuthenticated(page, baseUrl);
    await page.goto(routeUrl(baseUrl, ROUTES.apiKeys), { waitUntil: "domcontentloaded" });
    await page.waitForLoadState("networkidle").catch(() => undefined);

    await page.getByRole("button", { name: /\+ new/i }).click();
    const dialog = page.getByRole("dialog").filter({ hasText: /create api key/i }).first();
    await dialog.waitFor({ state: "visible", timeout: 20_000 });
    await dialog.getByLabel("Name").fill(apiKeyName);

    if (require2fa) {
      await dialog.getByText(/require 2fa for this key/i).click().catch(() => undefined);
    }

    await maybeToggleBillingEarning(dialog).catch(() => undefined);
    await dialog.getByRole("button", { name: /^save$/i }).click();
    await page.waitForLoadState("networkidle").catch(() => undefined);
    await page.waitForTimeout(2_000);

    const bodyText = summarizeVisibleText(await page.locator("body").innerText());
    const secretFromUi = deepFindSecretCandidate(bodyText);
    const secretFromResponse = responses.map(entry => deepFindSecretCandidate(entry.body)).find(Boolean) || null;
    const apiKey = secretFromResponse || secretFromUi || null;
    const artifacts = await captureArtifacts(page, "vast-ai-api-key-after-save", artifactDir);

    await saveJson(resultPath, {
      baseUrl,
      savedAt: new Date().toISOString(),
      pageUrl: page.url(),
      apiKeyName,
      require2fa,
      apiKey,
      discoveredSecretMasked: apiKey ? maskSecret(apiKey) : null,
      bodyPreview: bodyText.slice(0, 3000),
      artifacts,
      apiResponses: responses,
    });

    console.log(`Saved Vast.ai API key result to ${resultPath}`);
  } finally {
    await browser.close();
  }
}

main().catch(error => {
  console.error("[vast-ai-create-api-key] FAILED");
  console.error(error instanceof Error ? error.stack : String(error));
  process.exitCode = 1;
});
