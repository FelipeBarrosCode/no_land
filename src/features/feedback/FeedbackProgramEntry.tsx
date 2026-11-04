import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { translate } from "../../lib/i18n";
import { FeedbackProgramModal } from "./FeedbackProgramModal";
import { useFeedbackStore } from "./feedbackStore";

export function FeedbackProgramEntry() {
  const [open, setOpen] = useState(false);
  const status = useFeedbackStore((s) => s.status);
  const refreshStatus = useFeedbackStore((s) => s.refreshStatus);
  const invalidate = useFeedbackStore((s) => s.invalidate);
  const warning = useFeedbackStore((s) => s.warning);
  const setWarning = useFeedbackStore((s) => s.setWarning);
  useEffect(() => {
    // Local-only, including while off. Native alone owns network retry/control.
    void refreshStatus();
    const refresh = () => { if (document.visibilityState === "visible") void refreshStatus(); };
    const timer = window.setInterval(refresh, 30_000);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [refreshStatus]);
  useEffect(() => {
    // Optional native invalidation signal; TTL remains a hard upper bound even
    // on older native builds that do not emit it.
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import("@tauri-apps/api/event").then(({ listen }) => listen("feedback:invalidated", () => {
      invalidate();
      void refreshStatus();
    }))
      .then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch(() => {});
    return () => { disposed = true; unlisten?.(); };
  }, [invalidate, refreshStatus]);
  return (
    <div className="bg-[#080b19] px-4 pb-2 pt-3 md:px-8">
      <div className="mx-auto max-w-7xl">
        <Button variant="ghost" className="max-w-full text-xs" aria-haspopup="dialog" aria-expanded={open} onClick={() => setOpen(true)}>{translate("feedback.program")}
          <span className="ml-2 text-xs normal-case">{status.pendingControlAction ? translate("feedback.pending") : status.enabled ? translate("feedback.on") : translate("feedback.off")}</span>
        </Button>
        {warning && <div role="status" className="mt-2 flex flex-wrap items-start gap-3 border border-[#ffd78a] p-2 text-[#ffd78a]">
          <p className="min-w-0 break-words">{warning}</p><Button variant="ghost" onClick={() => setWarning(null)}>{translate("generated.48845bff334a50a5")}</Button>
        </div>}
      </div>
      {open && <FeedbackProgramModal onClose={() => setOpen(false)} />}
    </div>
  );
}
