import { useEffect, type PropsWithChildren } from "react";
import clsx from "clsx";

let documentScrollLockCount = 0;
let previousHtmlOverflow = "";
let previousBodyOverflow = "";

function lockDocumentScroll() {
  if (documentScrollLockCount === 0) {
    previousHtmlOverflow = document.documentElement.style.overflow;
    previousBodyOverflow = document.body.style.overflow;
    document.documentElement.style.overflow = "hidden";
    document.body.style.overflow = "hidden";
  }
  documentScrollLockCount += 1;
}

function unlockDocumentScroll() {
  documentScrollLockCount = Math.max(0, documentScrollLockCount - 1);
  if (documentScrollLockCount === 0) {
    document.documentElement.style.overflow = previousHtmlOverflow;
    document.body.style.overflow = previousBodyOverflow;
  }
}

interface ModalFrameProps extends PropsWithChildren {
  panelClassName?: string;
  overlayClassName?: string;
  labelledBy?: string;
  zIndexClassName?: string;
}

export function ModalFrame({
  children,
  panelClassName,
  overlayClassName,
  labelledBy,
  zIndexClassName = "z-50",
}: ModalFrameProps) {
  useEffect(() => {
    lockDocumentScroll();
    return unlockDocumentScroll;
  }, []);

  return (
    <div
      className={clsx(
        "fixed inset-0 overflow-y-auto overscroll-contain bg-[#02040bdd] p-4",
        zIndexClassName,
        overlayClassName,
      )}
    >
      <div className="flex min-h-full items-start justify-center sm:items-center">
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby={labelledBy}
          className={clsx(
            "flex max-h-[calc(100dvh-2rem)] min-h-0 w-full flex-col overflow-hidden",
            panelClassName,
          )}
        >
          {children}
        </div>
      </div>
    </div>
  );
}

interface ModalBodyProps extends PropsWithChildren {
  className?: string;
}

export function ModalBody({ children, className }: ModalBodyProps) {
  return (
    <div
      className={clsx(
        "min-h-0 flex-1 overflow-y-auto overscroll-contain",
        className,
      )}
    >
      {children}
    </div>
  );
}
