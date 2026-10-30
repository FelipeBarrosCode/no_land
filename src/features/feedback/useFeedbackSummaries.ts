import { useEffect, useState } from "react";
import { feedbackIdBatches, feedbackViewerKey } from "../../lib/feedbackModel";
import type { FeedbackMachineSummary, FeedbackRead } from "../../lib/feedbackTypes";
import { useFeedbackStore } from "./feedbackStore";

interface FeedbackSummariesResult {
  enabled: boolean;
  summaries: Record<string, FeedbackMachineSummary>;
  loading: boolean;
  error: string | null;
}

export function useFeedbackSummaries(machineIds: unknown[], active: boolean): FeedbackSummariesResult {
  const status = useFeedbackStore((s) => s.status);
  const revision = useFeedbackStore((s) => s.dataRevision);
  const error = useFeedbackStore((s) => s.dataError);
  const load = useFeedbackStore((s) => s.loadSummaries);
  const [tick, setTick] = useState(0);
  const [read, setRead] = useState<(FeedbackRead<Record<string, FeedbackMachineSummary>> & { key: string }) | null>(null);
  const [loading, setLoading] = useState(false);
  const ids = JSON.stringify(feedbackIdBatches(machineIds).flat().sort());
  const viewer = feedbackViewerKey(status);
  const key = JSON.stringify([viewer, revision, ids, tick]);
  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setRead(null);
    setLoading(false);
    if (!active || !viewer || error || ids === "[]") return;
    setLoading(true);
    void load(JSON.parse(ids) as string[]).then((result) => {
      if (disposed) return;
      setRead({ ...result, key });
      timer = setTimeout(() => { setRead(null); setTick((n) => n + 1); }, Math.max(1, result.expiresAt - Date.now()));
    }).catch(() => { if (!disposed) setRead(null); })
      .finally(() => { if (!disposed) setLoading(false); });
    return () => { disposed = true; if (timer !== undefined) clearTimeout(timer); };
  }, [active, viewer, error, ids, revision, key, load]);
  useEffect(() => {
    const refresh = () => { if (document.visibilityState === "visible") setTick((n) => n + 1); };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, []);
  return {
    enabled: active && status.enabled,
    summaries: active && viewer && !error && read?.key === key && read.expiresAt > Date.now() ? read.value : {},
    loading, error,
  };
}
