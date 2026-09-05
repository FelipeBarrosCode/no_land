import { getVersion } from "@tauri-apps/api/app";

const GITHUB_LATEST_RELEASE_URL =
  "https://api.github.com/repos/FelipeBarrosCode/no_land/releases/latest";

export interface AppUpdateInfo {
  currentVersion: string;
  latestVersion: string;
  releaseName: string;
  releaseUrl: string;
  releaseNotes: string;
  publishedAt: string | null;
}

interface GitHubReleaseResponse {
  tag_name?: string;
  name?: string | null;
  html_url?: string;
  body?: string | null;
  published_at?: string | null;
  draft?: boolean;
  prerelease?: boolean;
}

export async function checkForGitHubUpdate(): Promise<AppUpdateInfo | null> {
  if (!("__TAURI_INTERNALS__" in window)) {
    return null;
  }

  const currentVersion = normalizeVersion(await getVersion());
  const response = await fetch(GITHUB_LATEST_RELEASE_URL, {
    method: "GET",
    headers: {
      Accept: "application/vnd.github+json",
    },
  });

  if (response.status === 404) {
    return null;
  }

  if (!response.ok) {
    throw new Error(`GitHub update check failed (${response.status})`);
  }

  const release = (await response.json()) as GitHubReleaseResponse;
  if (release.draft || release.prerelease) {
    return null;
  }

  const latestVersion = normalizeVersion(release.tag_name ?? "");
  if (!latestVersion || !isVersionGreater(latestVersion, currentVersion)) {
    return null;
  }

  return {
    currentVersion,
    latestVersion,
    releaseName: release.name?.trim() || `Noland Connect ${latestVersion}`,
    releaseUrl: release.html_url ?? "https://github.com/FelipeBarrosCode/no_land/releases/latest",
    releaseNotes: release.body?.trim() || "No release notes provided.",
    publishedAt: release.published_at ?? null,
  };
}

function normalizeVersion(version: string): string {
  return version.trim().replace(/^v/i, "");
}

function isVersionGreater(candidate: string, current: string): boolean {
  const candidateParts = parseVersion(candidate);
  const currentParts = parseVersion(current);

  for (let index = 0; index < Math.max(candidateParts.length, currentParts.length); index += 1) {
    const candidatePart = candidateParts[index] ?? 0;
    const currentPart = currentParts[index] ?? 0;
    if (candidatePart > currentPart) {
      return true;
    }
    if (candidatePart < currentPart) {
      return false;
    }
  }

  return false;
}

function parseVersion(version: string): number[] {
  return version
    .split(/[.+-]/)
    .map((part) => Number.parseInt(part, 10))
    .filter((part) => Number.isFinite(part));
}
