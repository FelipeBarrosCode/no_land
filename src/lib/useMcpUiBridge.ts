import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invokeSafe, isRunningInTauri } from "./tauri";

type Request = { id: string; kind: string; expiresAt: number; arguments: { action_id?: string; revision?: string; route?: string } };

export function useMcpUiBridge(enabled: boolean) {
  useEffect(() => {
    if (!enabled || !isRunningInTauri()) return;
    let disposed = false;
    let timer: number | undefined;
    let previous = "";
    let revision = "";
    let sequence = 0;
    let publishing: Promise<unknown> | null = null;
    let actions = new Map<string, HTMLButtonElement>();
    const ids = new WeakMap<Element, string>();
    const idFor = (node: Element) => {
      let id = ids.get(node);
      if (!id) { id = `ui-${crypto.randomUUID()}`; ids.set(node, id); }
      return id;
    };
    const publish = async () => {
      while (publishing) await publishing.catch(() => undefined);
      if (disposed) return;
      const next = new Map<string, HTMLButtonElement>();
      const allActions: { id: string; label: string; allowed: boolean; capability: string; instanceId: number | null }[] = [];
      const dialogs = [...document.querySelectorAll<HTMLElement>('[role="dialog"]')].slice(0, 30).map((dialog) => {
        const human = Boolean(dialog.closest('[data-mcp-human-only="true"]'));
        const buttons = [...dialog.querySelectorAll<HTMLButtonElement>("button")].slice(0, 30).map((button) => {
          const id = idFor(button);
          const allowed = !human && !button.closest('[data-mcp-human-only="true"]') && button.dataset.mcpAllow === "true" && !button.disabled && button.getClientRects().length > 0;
          const instance = button.closest<HTMLElement>("[data-mcp-instance-id]")?.dataset.mcpInstanceId;
          const action = { id, label: (button.textContent || button.getAttribute("aria-label") || "Button").trim().slice(0, 160), allowed,
            capability: button.dataset.mcpCapability || "dialogs", instanceId: instance ? Number(instance) : null };
          if (allowed) next.set(id, button);
          allActions.push(action);
          return action;
        });
        return { id: idFor(dialog), instanceId: dialog.dataset.mcpInstanceId ? Number(dialog.dataset.mcpInstanceId) : null,
          text: human ? "[Human-only security or permission dialog]" : (dialog.innerText || "").slice(0, 6000), actions: buttons,
          humanOnly: human, reason: human ? "Human security/permission approval" : "Only explicitly delegated buttons can be used" };
      });
      // Never publish input values, DOM HTML, credential fields or MCP settings.
      const route = location.hash.slice(1) || "/";
      const content = JSON.stringify({ route, dialogs, actions: allActions });
      actions = next;
      if (content === previous) return;
      revision = `${Date.now()}-${++sequence}`;
      publishing = invokeSafe("mcp_publish_ui", { snapshot: { route, revision, dialogs, actions: allActions } });
      try { await publishing; previous = content; } finally { publishing = null; }
    };
    const schedule = () => { window.clearTimeout(timer); timer = window.setTimeout(() => void publish().catch(() => undefined), 150); };
    const observer = new MutationObserver(schedule);
    observer.observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ["disabled", "hidden", "data-mcp-allow"] });
    window.addEventListener("hashchange", schedule);
    const subscription = listen<Request>("mcp:ui-action", async ({ payload }) => {
      let error: string | null = null;
      try {
        if (disposed) throw new Error("Main UI bridge is unavailable");
        // Recompute before acting to catch changes since the agent's snapshot.
        await publish();
        if (!Number.isFinite(payload.expiresAt) || Date.now() > payload.expiresAt) throw new Error("UI action expired; refresh context before retrying");
        if (payload.kind === "navigate") {
          if (!["/", "/provisioning", "/settings"].includes(payload.arguments.route || "")) throw new Error("Route is not delegated");
          location.hash = payload.arguments.route!;
        } else {
          if (payload.arguments.revision !== revision) throw new Error("UI changed; request a new snapshot");
          const button = actions.get(payload.arguments.action_id || "");
          if (!button?.isConnected || button.disabled || button.dataset.mcpAllow !== "true" || button.closest('[data-mcp-human-only="true"]')) throw new Error("This action requires a human or is no longer available");
          button.click();
        }
      } catch (e) { error = e instanceof Error ? e.message : String(e); }
      await invokeSafe("mcp_complete_ui", { id: payload.id, error }).catch(() => undefined);
    });
    void publish().catch(() => undefined);
    return () => { disposed = true; observer.disconnect(); window.clearTimeout(timer); window.removeEventListener("hashchange", schedule); void subscription.then((stop) => stop()); };
  }, [enabled]);
}
