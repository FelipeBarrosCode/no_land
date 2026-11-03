import { useState } from "react";
import { Button } from "../../components/ui/Button";
import { applyInstanceDesktopPassword } from "../../lib/backend";
import { errorMessage } from "../../lib/errorMessage";
import { translateSource } from "../../lib/i18n";
import type { ProvisionedServerState } from "../../lib/types";

interface Props {
  servers: Pick<ProvisionedServerState, "instanceId">[];
  busy: boolean;
  hasUnsavedPassword: boolean;
}

export function RemoteDesktopPasswordSettings({ servers, busy, hasUnsavedPassword }: Props) {
  const [pendingId, setPendingId] = useState<number | null>(null);
  const [results, setResults] = useState<Record<number, { ok: boolean; text: string }>>({});

  async function apply(instanceId: number) {
    if (busy || hasUnsavedPassword || pendingId !== null) return;
    setPendingId(instanceId);
    setResults((current) => {
      const next = { ...current };
      delete next[instanceId];
      return next;
    });
    try {
      await applyInstanceDesktopPassword(instanceId);
      setResults((current) => ({ ...current, [instanceId]: {
        ok: true, text: translateSource("Remote desktop password updated successfully."),
      } }));
    } catch (error) {
      setResults((current) => ({ ...current, [instanceId]: { ok: false, text: errorMessage(error) } }));
    } finally {
      setPendingId(null);
    }
  }

  return (
    <section className="mt-4 space-y-3 border-t border-[#3b4067] pt-4">
      <h3 className="font-display text-[10px] uppercase text-neon-cyan">
        {translateSource("Remote desktop password")}
      </h3>
      <p className="text-[1.1rem] text-[#a8bed6]">
        {translateSource("New machines use your saved platform password for the Linux desktop account. The Linux username and key-based SSH access stay unchanged. Saving settings does not rotate existing machines; apply the saved password to each running machine below.")}
      </p>
      {hasUnsavedPassword && <p role="status">{translateSource("Save your platform password before applying it to a remote machine.")}</p>}
      {servers.map(({ instanceId }) => (
        <div key={instanceId} className="space-y-1">
          <Button
            variant="secondary"
            disabled={busy || hasUnsavedPassword || pendingId !== null}
            loading={pendingId === instanceId}
            onClick={() => void apply(instanceId)}
          >
            {translateSource("Apply saved password to machine")} #{instanceId}
          </Button>
          {results[instanceId] && <p role={results[instanceId].ok ? "status" : "alert"}>
            {results[instanceId].text}
          </p>}
        </div>
      ))}
    </section>
  );
}
