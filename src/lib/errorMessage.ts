export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;

  if (error && typeof error === "object") {
    const value = error as Record<string, unknown>;
    // Tauri FrontendError uses a friendly top-level message and puts the
    // actionable operation failure in `details`. Prefer that detail so
    // transport failures are not reduced to "Operation timed out".
    for (const key of ["details", "message", "error"]) {
      const candidate = value[key];
      if (typeof candidate === "string" && candidate.trim()) return candidate;
      if (candidate && typeof candidate === "object") {
        const nested = errorMessage(candidate);
        if (nested) return nested;
      }
    }

    try {
      return JSON.stringify(error);
    } catch {
      return "Something went wrong. Check the logs and try again.";
    }
  }

  return String(error);
}
