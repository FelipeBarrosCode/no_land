import clsx from "clsx";
import type { ButtonHTMLAttributes } from "react";
import { playArcadeClick } from "../../lib/arcadeAudio";

type Variant = "primary" | "secondary" | "ghost" | "danger";

interface Props extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  loading?: boolean;
  loadingText?: string;
}

const variantClasses: Record<Variant, string> = {
  primary:
    "border-[#61f7ff] bg-[#1b2f4d] text-[#7cf8ff] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#2f5f86,0_0_20px_rgba(68,214,255,0.25)] hover:bg-[#22466e] hover:text-white",
  secondary:
    "border-[#7bff48] bg-[#1d3620] text-[#b4ff88] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#366230] hover:bg-[#2b4f28]",
  ghost:
    "border-[#495188] bg-transparent text-[#b9caf0] shadow-[0_0_0_2px_#090a17,inset_0_0_0_1px_#11152f] hover:border-[#61f7ff] hover:text-[#84fbff]",
  danger:
    "border-[#ff8ca2] bg-[#4b1f2f] text-[#ffc1cf] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#6f2c45] hover:bg-[#673149]"
};

export function Button({
  variant = "primary",
  className,
  loading = false,
  loadingText,
  children,
  ...props
}: Props) {
  const { onClick, disabled, ...rest } = props;
  const resolvedLoadingText =
    loadingText ?? (typeof children === "string" ? children : "Working...");

  return (
    <button
      className={clsx(
        "relative inline-flex items-center justify-center border px-4 py-2 font-display text-[11px] uppercase tracking-[0.12em] transition duration-100 active:translate-y-[2px] disabled:cursor-not-allowed disabled:opacity-50",
        variantClasses[variant],
        className
      )}
      onClick={(event) => {
        if (!disabled && !loading) {
          playArcadeClick();
        }
        onClick?.(event);
      }}
      disabled={disabled || loading}
      {...rest}
    >
      <span className={clsx("inline-flex items-center justify-center gap-2", loading && "opacity-0")}>
        {children}
      </span>

      {loading && (
        <span className="absolute inset-0 flex items-center justify-center gap-2">
          <span
            aria-hidden="true"
            className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-current border-t-transparent"
          />
          <span>{resolvedLoadingText}</span>
        </span>
      )}
    </button>
  );
}
