import clsx from "clsx";
import type { OrchestrationState } from "../../lib/types";

interface Props {
  state: OrchestrationState;
}

export function StatusPill({ state }: Props) {
  const intent =
    state === "Error"
      ? "border-[#ff687d] bg-[#481b2a] text-[#ffb2bf]"
      : state === "Ready"
        ? "border-[#8af75d] bg-[#243d21] text-[#c8ffad]"
        : "border-[#44d6ff] bg-[#182a43] text-[#8deeff]";

  return (
    <span
      className={clsx(
        "inline-flex min-h-7 items-center border px-2.5 py-1 font-display text-[11px] uppercase tracking-[0.08em] shadow-[inset_0_0_0_2px_#121731]",
        intent
      )}
    >
      {state}
    </span>
  );
}
