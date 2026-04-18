interface Props {
  label: string;
  value: number;
  max?: number;
  valueLabel?: string;
}

export function HudBar({ label, value, max = 1, valueLabel }: Props) {
  const ratio = Math.max(0, Math.min(1, value / max));

  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between">
        <span className="font-display text-[10px] uppercase tracking-[0.1em] text-[#9ad9ff]">{label}</span>
        <span className="text-[1.1rem] leading-none text-[#d9efff]">{valueLabel ?? `${Math.round(ratio * 100)}%`}</span>
      </div>
      <div className="border border-[#3f476c] bg-[#0b0f23] p-[2px] shadow-[inset_0_0_0_1px_#121731]">
        <div className="h-3 bg-[#1a2145]">
          <div
            className="h-full bg-[repeating-linear-gradient(90deg,#44d6ff_0px,#44d6ff_6px,#7cff47_6px,#7cff47_12px)]"
            style={{ width: `${ratio * 100}%` }}
          />
        </div>
      </div>
    </div>
  );
}
