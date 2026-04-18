import clsx from "clsx";
import type { InputHTMLAttributes } from "react";

interface Props extends InputHTMLAttributes<HTMLInputElement> {
  label: string;
  error?: string;
}

export function InputField({ label, className, error, ...props }: Props) {
  return (
    <label className="flex flex-col gap-2 text-base">
      <span className="font-display text-[10px] uppercase tracking-[0.14em] text-[#9ad9ff]">{label}</span>
      <input
        className={clsx(
          "border bg-[#0b0f23] px-3 py-2 text-[1.45rem] leading-none text-[#dff8ff] outline-none transition placeholder:text-[#5e7396]",
          error
            ? "border-[#ff687d] shadow-[inset_0_0_0_2px_#3f1623]"
            : "border-[#3f476c] shadow-[inset_0_0_0_2px_#121731] focus:border-neon-cyan focus:shadow-[inset_0_0_0_2px_#121731,0_0_0_2px_rgba(68,214,255,0.28)]",
          className
        )}
        {...props}
      />
      {error && <span className="font-display text-[10px] uppercase text-[#ff9eb0]">{error}</span>}
    </label>
  );
}
