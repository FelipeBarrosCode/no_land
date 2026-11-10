import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { useLocalization } from "../../lib/i18n";
import {
  buildMcpSetupPrompt, mcpAccessToken, mcpConfigure, mcpDisable, mcpStatus, type McpStatus,
} from "../../lib/mcp";

export function McpSettings() {
  const { t } = useLocalization();
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  useEffect(() => {
    let disposed = false;
    let fetching = false;
    const refresh = async () => {
      if (fetching) return;
      fetching = true;
      try {
        const next = await mcpStatus();
        if (!disposed) setStatus(next);
      } catch (e) {
        if (!disposed) setError(String(e));
      } finally {
        fetching = false;
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 2000);
    return () => { disposed = true; window.clearInterval(timer); };
  }, []);

  async function action(run: () => Promise<void>) {
    setBusy(true); setError(""); setNotice("");
    try { await run(); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }

  if (!status) return <Card><p role={error ? "alert" : "status"}>{error || t("common.loading")}</p></Card>;

  return <Card className="pixel-frame space-y-5" data-mcp-human-only="true">
    <div className="space-y-2">
      <h2 className="font-display text-neon-cyan">{t("mcp.title")}</h2>
      <p className="text-sm text-[#9eb1c8]">{t("mcp.simpleDescription")}</p>
    </div>
    <label className="flex items-center gap-3 rounded border border-[#34425d] p-4">
      <input type="checkbox" role="switch" checked={status.config.enabled} disabled={busy}
        onChange={(event) => {
          const enabled = event.target.checked;
          void action(async () => {
            setStatus(enabled ? await mcpConfigure({ ...status.config, enabled }) : await mcpDisable());
          });
        }} />
      <span className="font-bold">{t("mcp.enabled")}</span>
    </label>
    <div className="space-y-3 rounded border border-[#34425d] bg-black/20 p-4">
      <p className="text-sm text-[#9eb1c8]">{t("mcp.endpoint")}</p>
      <div className="flex flex-wrap items-center gap-3">
        <code className="min-w-0 flex-1 break-all text-neon-cyan">{status.endpoint}</code>
        <Button variant="ghost" disabled={busy} onClick={() => void action(async () => {
          await navigator.clipboard.writeText(status.endpoint);
          setNotice(t("mcp.addressCopied"));
        })}>{t("mcp.copyEndpoint")}</Button>
        <Button variant="ghost" disabled={busy} onClick={() => void action(async () => {
          await navigator.clipboard.writeText(String(status.config.port));
          setNotice(t("mcp.addressCopied"));
        })}>{t("mcp.copyPort")}</Button>
      </div>
      <p role="status" className="text-sm text-[#9eb1c8]">{t("mcp.status", {
        status: status.listening ? t("common.connected") : t("common.disconnected"), jobs: status.activeJobs,
      })}</p>
    </div>
    <div className="space-y-2">
      <Button disabled={busy || !status.listening} onClick={() => void action(async () => {
        const token = await mcpAccessToken();
        await navigator.clipboard.writeText(buildMcpSetupPrompt(status.endpoint, token));
        setNotice(t("mcp.setupCopied"));
      })}>{t("mcp.copySetup")}</Button>
      <p className="text-sm text-[#9eb1c8]">{t("mcp.simpleHelp")}</p>
    </div>
    {(error || status.error) && <p role="alert" className="text-red-300">{error || status.error}</p>}
    {notice && <p role="status" className="text-green-300">{notice}</p>}
  </Card>;
}
