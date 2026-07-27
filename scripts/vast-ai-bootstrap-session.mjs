import path from "node:path";

import {
  DEFAULT_BASE_URL,
  ROUTES,
  captureArtifacts,
  ensureDir,
  envBool,
  launchBrowserContext,
  normalizeBaseUrl,
  resolveArtifactDir,
  resolveStorageStatePath,
  routeUrl,
  saveJson,
  waitForAuthenticatedShell,
  ensureAuthenticated,
  removeFileIfExists,
} from "./vast-ai-utils.mjs";

async function main() {
  const baseUrl = normalizeBaseUrl(process.env.VAST_AI_BASE_URL || DEFAULT_BASE_URL);
  const storageStatePath = resolveStorageStatePath();
  const artifactDir = resolveArtifactDir();
  const headless = envBool("VAST_AI_HEADLESS", false);

  await ensureDir(path.dirname(storageStatePath));
  await ensureDir(artifactDir);
  const resultPath = path.join(artifactDir, "vast-ai-authenticated-session.json");

  await removeFileIfExists(storageStatePath);
  await removeFileIfExists(resultPath);

  const { browser, context, page } = await launchBrowserContext({ headless });
  try {
    await page.goto(routeUrl(baseUrl, ROUTES.console), { waitUntil: "domcontentloaded" });
    console.log("Vast.ai browser session opened. Complete login/signup, then close the browser when done.");

    const ready = await waitForAuthenticatedShell(page, 300_000);
    if (!ready) {
      throw new Error("Timed out waiting for authenticated Vast.ai UI markers.");
    }

    await ensureAuthenticated(page, baseUrl);
    await context.storageState({ path: storageStatePath });

    const artifacts = await captureArtifacts(page, "vast-ai-authenticated-session", artifactDir);
    await saveJson(resultPath, {
      baseUrl,
      savedAt: new Date().toISOString(),
      storageStatePath,
      artifactDir,
      pageUrl: page.url(),
      artifacts,
    });
    console.log(`Saved Vast.ai browser session metadata to ${resultPath}`);
  } catch (error) {
    await removeFileIfExists(storageStatePath);
    await removeFileIfExists(resultPath);
    throw error;
  } finally {
    await browser.close();
  }
}

main().catch(error => {
  console.error("[vast-ai-bootstrap-session] FAILED");
  console.error(error instanceof Error ? error.stack : String(error));
  process.exitCode = 1;
});
