import { useCallback, useEffect, useRef, useState } from "react";
import { getSpendSummary, subscribeSpendUpdates } from "../../lib/backend";
import type { SpendSummary } from "../../lib/types";

/** Latest spend summary, refreshed whenever the backend tracker ticks. */
export function useSpendSummary() {
  const [summary, setSummary] = useState<SpendSummary | null>(null);
  const refreshing = useRef(false);

  const refresh = useCallback(async () => {
    if (refreshing.current) return;
    refreshing.current = true;
    try {
      setSummary(await getSpendSummary());
    } catch (error) {
      console.warn("[spend] failed to load spend summary", error);
    } finally {
      refreshing.current = false;
    }
  }, []);

  useEffect(() => {
    void refresh();
    let disposed = false;
    let unlisten: (() => void) | null = null;
    // Cross-window events are hints, not authenticated financial data.
    void subscribeSpendUpdates(() => { void refresh(); }).then((stop) => {
      if (disposed) {
        stop();
      } else {
        unlisten = stop;
      }
    }).catch((error) => {
      console.warn("[spend] failed to subscribe to spend updates", error);
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refresh]);

  return { summary, setSummary, refresh };
}
