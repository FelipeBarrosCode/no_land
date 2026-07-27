import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import type {
  VastBrowserAutomationStatus,
  VastBrowserBillingAction,
  VastBrowserGeneratedApiKeyResult,
} from "../../lib/types";

interface Props {
  open: boolean;
  busy: boolean;
  status: VastBrowserAutomationStatus | null;
  apiKeyName?: string;
  onClose: () => void;
  onRefresh: () => Promise<VastBrowserAutomationStatus | null>;
  onConnect: () => Promise<unknown>;
  onGenerateApiKey: (
    apiKeyName?: string,
  ) => Promise<VastBrowserGeneratedApiKeyResult | null>;
  onOpenBilling: (action?: VastBrowserBillingAction) => Promise<unknown>;
  onApiKeyGenerated?: (apiKey: string) => void;
}

export function VastBrowserAutomationModal({
  open,
  busy,
  status,
  apiKeyName,
  onClose,
  onRefresh,
  onConnect,
  onGenerateApiKey,
  onOpenBilling,
  onApiKeyGenerated,
}: Props) {
  if (!open) {
    return null;
  }

  const available = status?.available ?? false;
  const sessionConnected = status?.sessionConnected ?? false;

  async function handleGenerateApiKey() {
    const result = await onGenerateApiKey(apiKeyName);
    if (result?.apiKey) {
      onApiKeyGenerated?.(result.apiKey);
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-[#02040bdd] p-4">
      <Card className="pixel-frame w-full max-w-2xl animate-fade-in p-6 md:p-8">
        <div className="flex items-start justify-between gap-4">
          <div>
            <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">
              Vast.ai Browser Tools
            </p>
            <h2 className="pixel-heading mt-2 font-display text-lg text-white md:text-xl">
              Connect Vast.ai Through a Managed Browser Session
            </h2>
            <p className="mt-3 text-[1.2rem] leading-snug text-[#c5d8ec]">
              This opens a managed Chrome window for Vast.ai. Log in there once,
              then come back here to generate the API key or open billing with the
              same saved browser session.
            </p>
          </div>

          <Button variant="ghost" onClick={onClose}>
            Close
          </Button>
        </div>

        <div className="mt-5 grid gap-4 md:grid-cols-2">
          <div className="rounded-md border border-[#35506e] bg-[#0d1630]/80 p-4 text-[1.05rem] text-[#b4d7f4]">
            <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">
              Current Status
            </h3>
            <div className="mt-3 space-y-1">
              <p>Automation: {available ? "available" : "not available in this build"}</p>
              <p>Saved session: {sessionConnected ? "connected" : "not connected"}</p>
              {status?.savedAt ? <p>Last connected: {status.savedAt}</p> : null}
              {status?.storageStatePath ? <p className="break-all">State file: {status.storageStatePath}</p> : null}
              {status?.lastError ? <p className="text-[#ff9eb0]">Last error: {status.lastError}</p> : null}
            </div>
          </div>

          <div className="rounded-md border border-[#35506e] bg-[#0d1630]/80 p-4 text-[1.05rem] text-[#b4d7f4]">
            <h3 className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">
              Suggested Flow
            </h3>
            <ol className="mt-3 list-decimal space-y-1 pl-5 leading-snug">
              <li>Connect Vast.ai account</li>
              <li>Log in in the Chrome window that opens</li>
              <li>Close that browser window</li>
              <li>Generate API key from the saved session</li>
              <li>Use billing tools to add credit or configure auto top-up</li>
            </ol>
          </div>
        </div>

        <div className="mt-6 grid gap-3 md:grid-cols-2 xl:grid-cols-3">
          <Button
            variant="secondary"
            disabled={busy || !available}
            onClick={() => void onConnect()}
          >
            {sessionConnected ? "Reconnect Vast.ai Browser" : "Open Vast.ai Login Browser"}
          </Button>
          <Button
            variant="ghost"
            disabled={busy || !sessionConnected}
            onClick={() => void handleGenerateApiKey()}
          >
            Generate API Key
          </Button>
          <Button
            variant="ghost"
            disabled={busy}
            onClick={() => void onRefresh()}
          >
            Refresh Session Status
          </Button>
          <Button
            variant="ghost"
            disabled={busy || !sessionConnected}
            onClick={() => void onOpenBilling("snapshot")}
          >
            Open Billing Browser
          </Button>
          <Button
            variant="ghost"
            disabled={busy || !sessionConnected}
            onClick={() => void onOpenBilling("open-add-credit")}
          >
            Open Add Credit Browser
          </Button>
          <Button
            variant="ghost"
            disabled={busy || !sessionConnected}
            onClick={() => void onOpenBilling("open-auto-topup")}
          >
            Open Auto Top-up Browser
          </Button>
        </div>
      </Card>
    </div>
  );
}
