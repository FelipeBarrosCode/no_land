import { invoke } from "@tauri-apps/api/core";

export async function invokeSafe<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args);
}

export function isRunningInTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function reportFrontendReady(route: string): Promise<void> {
  return invokeSafe<void>("report_frontend_ready", { route });
}
