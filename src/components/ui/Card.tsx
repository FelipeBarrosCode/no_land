import clsx from "clsx";
import type { PropsWithChildren } from "react";

interface Props extends PropsWithChildren {
  className?: string;
  interactive?: boolean;
  onClick?: () => void;
}

export function Card({ className, children, interactive = false, onClick }: Props) {
  return (
    <div
      className={clsx(
        "glass-panel p-4",
        interactive &&
          "cursor-pointer transition duration-100 hover:border-neon-cyan hover:shadow-[inset_0_0_0_2px_#090a17,inset_0_0_0_4px_#2d315b,0_0_0_2px_#090a17,0_0_22px_rgba(68,214,255,0.35)]",
        className
      )}
      onClick={onClick}
      role={interactive ? "button" : undefined}
      tabIndex={interactive ? 0 : undefined}
    >
      {children}
    </div>
  );
}
