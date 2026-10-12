import { useCallback, useEffect, useState } from "react";
import { getSpendSummary, subscribeSpendUpdates } from "../../lib/backend";
import type { SpendSummary } from "../../lib/types";

/** Latest spend summary, refreshed whenever the backend tracker ticks. */
export function useSpendSummary() {
  const [summary, setSummary] = useState<SpendSummary | null>(null);

  const refresh = useCallback(async () => {
    try {
      setSummary(await getSpendSummary());
    } catch (error) {
      console.warn("[spend] failed to load spend summary", error);
    }
  }, []);

  useEffect(() => {
    void refresh();
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void subscribeSpendUpdates((next) => setSummary(next)).then((stop) => {
      if (disposed) {
        stop();
      } else {
        unlisten = stop;
      }
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refresh]);

  return { summary, setSummary, refresh };
}
