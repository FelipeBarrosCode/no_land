import path from "node:path";

import {
  DEFAULT_BASE_URL,
  ROUTES,
  captureArtifacts,
  collectJsonResponses,
  ensureAuthenticated,
  envBool,
  launchBrowserContext,
  normalizeBaseUrl,
  resolveArtifactDir,
  resolveStorageStatePath,
  routeUrl,
  saveJson,
  summarizeVisibleText,
  waitForBrowserClose,
} from "./vast-ai-utils.mjs";

async function main() {
  const baseUrl = normalizeBaseUrl(process.env.VAST_AI_BASE_URL || DEFAULT_BASE_URL);
  const storageStatePath = resolveStorageStatePath();
  const artifactDir = resolveArtifactDir();
  const headless = envBool("VAST_AI_HEADLESS", false);
  const keepOpen = envBool("VAST_AI_KEEP_OPEN", true);
  const action = (process.env.VAST_AI_BILLING_ACTION || "snapshot").trim();
  const resultPath = path.join(artifactDir, "vast-ai-billing-result.json");

  const { browser, page } = await launchBrowserContext({ headless, storageStatePath });
  try {
    const responses = await collectJsonResponses(page, (_response, url) => url.includes("/api/"));
    await ensureAuthenticated(page, baseUrl);
    await page.goto(routeUrl(baseUrl, ROUTES.billing), { waitUntil: "domcontentloaded" });
    await page.waitForLoadState("networkidle").catch(() => undefined);

    let dialogDetected = false;
    if (action === "open-add-credit") {
      await page.getByRole("button", { name: /add credit/i }).click();
      const dialog = page.getByRole("dialog").filter({ hasText: /add credits/i }).first();
      await dialog.waitFor({ state: "visible", timeout: 20_000 });
      dialogDetected = true;
    } else if (action === "open-auto-topup") {
      const autoTopupCard = page.locator("div").filter({ hasText: /automatic top-up/i }).first();
      const editButton = autoTopupCard.locator("button").last();
      await editButton.click();
      const dialog = page.getByRole("dialog").filter({ hasText: /automatic billing/i }).first();
      await dialog.waitFor({ state: "visible", timeout: 20_000 });
      dialogDetected = true;
    }

    const bodyText = summarizeVisibleText(await page.locator("body").innerText());
    const artifacts = await captureArtifacts(page, "vast-ai-billing-session", artifactDir);
    await saveJson(resultPath, {
      baseUrl,
      savedAt: new Date().toISOString(),
      action,
      pageUrl: page.url(),
      dialogDetected,
      bodyPreview: bodyText.slice(0, 2500),
      artifacts,
      apiResponses: responses,
    });

    if (keepOpen && !headless) {
      console.log("Vast.ai billing browser session is ready. Close the browser window when you are done.");
      await waitForBrowserClose(browser);
      return;
    }
  } finally {
    if (browser.isConnected()) {
      await browser.close();
    }
  }
}

main().catch(error => {
  console.error("[vast-ai-open-billing-session] FAILED");
  console.error(error instanceof Error ? error.stack : String(error));
  process.exitCode = 1;
});
