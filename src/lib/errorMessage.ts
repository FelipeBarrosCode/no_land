import { translate, translateOptional, translateSource } from "./i18n";

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return translateSource(error);
  if (error instanceof Error) return translateSource(error.message);

  if (error && typeof error === "object") {
    const value = error as Record<string, unknown>;
    const code = typeof value.code === "string" ? value.code : null;
    const codeMessage = code ? translateOptional(`error.code.${code}`) : null;
    let rawMessage: string | null = null;
    // Tauri FrontendError uses a friendly top-level message and puts the
    // actionable operation failure in `details`. Prefer that detail so
    // transport failures are not reduced to "Operation timed out".
    for (const key of ["details", "message", "error"]) {
      const candidate = value[key];
      if (typeof candidate === "string" && candidate.trim()) {
        const localized = translateSource(candidate);
        if (localized !== candidate) return localized;
        rawMessage ??= candidate;
        continue;
      }
      if (candidate && typeof candidate === "object") {
        const nested = errorMessage(candidate);
        if (nested) return nested;
      }
    }

    if (codeMessage) return codeMessage;
    if (rawMessage) return rawMessage;

    try {
      return JSON.stringify(error);
    } catch {
      return translate("error.generic.logs");
    }
  }

  return String(error);
}
