import { useEffect, useId, useRef, type PropsWithChildren } from "react";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";

interface Props extends PropsWithChildren {
  title: string;
  onClose: () => void;
  busy?: boolean;
}

export function FeedbackDialog({ title, onClose, busy = false, children }: Props) {
  const titleId = useId();
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const first = root.current?.querySelector<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex='0']");
    (first ?? root.current)?.focus();
    return () => { if (previous?.isConnected) previous.focus(); };
  }, []);
  return (
    <div ref={root} tabIndex={-1} className="[&_button]:max-w-full [&_button>span]:min-w-0 [&_button>span]:flex-wrap" onKeyDown={(event) => {
      if (event.key === "Escape") {
        // A nested reviews dialog must not also close the server picker.
        event.stopPropagation();
        event.preventDefault();
        if (!busy) onClose();
      }
      if (event.key === "Tab") {
        const items = Array.from(root.current?.querySelectorAll<HTMLElement>(
          "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex='0']",
        ) ?? []).filter((item) => item.getClientRects().length > 0);
        const first = items[0];
        const last = items[items.length - 1];
        if (!first || !last) {
          event.preventDefault(); root.current?.focus();
        } else if (event.shiftKey && (document.activeElement === first || document.activeElement === root.current || !root.current?.contains(document.activeElement))) {
          event.preventDefault(); last.focus();
        } else if (!event.shiftKey && (document.activeElement === last || document.activeElement === root.current || !root.current?.contains(document.activeElement))) {
          event.preventDefault(); first.focus();
        }
      }
    }}>
      <ModalFrame labelledBy={titleId} panelClassName="glass-panel pixel-frame max-w-2xl" zIndexClassName="z-[80]">
        <div className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-b-2 border-[#3e4270] p-4">
          <h2 id={titleId} className="min-w-0 break-words font-display text-sm text-white">{title}</h2>
          <Button type="button" variant="ghost" disabled={busy} onClick={onClose}>Close</Button>
        </div>
        <ModalBody className="min-w-0 space-y-4 break-words p-4 text-[1.12rem] leading-snug text-[#c6dbf4] [overflow-wrap:anywhere]">{children}</ModalBody>
      </ModalFrame>
    </div>
  );
}
