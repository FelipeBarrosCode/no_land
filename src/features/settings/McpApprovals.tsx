import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import { mcpDecide, mcpStatus, type McpApproval } from "../../lib/mcp";
import { isRunningInTauri } from "../../lib/tauri";
import { useLocalization } from "../../lib/i18n";

export function McpApprovals() {
  const { t } = useLocalization();
  const [pending, setPending] = useState<McpApproval | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    if (!isRunningInTauri()) return;
    let disposed = false; let fetching = false;
    const refresh = async () => {
      if (fetching) return; fetching = true;
      try { const status = await mcpStatus(); if (!disposed) setPending(status.approvals.find((a) => a.status === "pending") || null); }
      catch { /* Server startup/secure-store errors are shown in Settings. */ }
      finally { fetching = false; }
    };
    void refresh(); const timer = window.setInterval(() => void refresh(), 1500);
    return () => { disposed = true; window.clearInterval(timer); };
  }, []);
  if (!pending) return null;
  async function decide(approve: boolean) {
    if (!pending) return;
    setBusy(true); setError("");
    try { await mcpDecide(pending.id, approve); setPending(null); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }
  return <div data-mcp-human-only="true"><ModalFrame panelClassName="pixel-frame max-w-2xl bg-[#090d20] text-white" labelledBy="mcp-approval-title" zIndexClassName="z-[150]">
    <div className="border-b border-[#34425d] p-5"><h2 id="mcp-approval-title" className="font-display text-amber-300">{t("mcp.approvalTitle")}</h2><p>{t("mcp.approvalHelp")}</p></div>
    <ModalBody className="space-y-4 p-5">
      <p><strong>{pending.tool}</strong>{pending.instanceId ? ` · ${t("mcp.instance", { id: pending.instanceId })}` : ""}</p>
      <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-all rounded bg-black/40 p-3 text-xs">{JSON.stringify(pending.arguments, null, 2)}</pre>
      {error && <p role="alert" className="text-red-300">{error}</p>}
      <div className="flex gap-3"><Button variant="ghost" disabled={busy} onClick={() => void decide(false)}>{t("mcp.deny")}</Button><Button variant="danger" disabled={busy} onClick={() => void decide(true)}>{t("mcp.approve")}</Button></div>
    </ModalBody>
  </ModalFrame></div>;
}
