import clsx from "clsx";
import type { InputHTMLAttributes, ReactNode } from "react";
import { translateSource } from "../../lib/i18n";

interface Props extends InputHTMLAttributes<HTMLInputElement> {
  label: ReactNode;
  error?: string;
}

export function InputField({ label, className, error, ...props }: Props) {
  return (
    <label className="flex flex-col gap-1.5 text-base">
      <span className="font-display text-[11px] uppercase tracking-[0.1em] text-[#9ad9ff]">{label}</span>
      <input
        className={clsx(
          "min-h-11 border bg-[#0b0f23] px-3 py-2 text-[1.1rem] leading-[1.2] text-[#dff8ff] outline-none transition placeholder:text-[#5e7396]",
          error
            ? "border-[#ff687d] shadow-[inset_0_0_0_2px_#3f1623]"
            : "border-[#3f476c] shadow-[inset_0_0_0_2px_#121731] focus:border-neon-cyan focus:shadow-[inset_0_0_0_2px_#121731,0_0_0_2px_rgba(68,214,255,0.28)]",
          className
        )}
        {...props}
      />
      {error && <span className="font-display text-[11px] uppercase text-[#ff9eb0]">{translateSource(error)}</span>}
    </label>
  );
}
