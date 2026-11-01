import { FEEDBACK_CACHE_TTL_MS, feedbackIdBatches, feedbackMachineId, parseFeedbackReviews, parseFeedbackSummary } from "./feedbackModel";
import type { FeedbackMachineSummary, FeedbackRead, FeedbackReviewsPage } from "./feedbackTypes";

interface Transport {
  summaries: (machineIds: string[]) => Promise<unknown>;
  reviews: (machineId: string, cursor: string | null) => Promise<unknown>;
}

// Separate summary/page maps are both viewer-scoped: even summary review counts
// obey that participant's block list. Generation checks discard late responses.
export function createFeedbackDataClient(transport: Transport, now = Date.now) {
  let viewer: string | null = null;
  let generation = 0;
  const summaries = new Map<string, FeedbackRead<FeedbackMachineSummary>>();
  const pages = new Map<string, FeedbackRead<FeedbackReviewsPage>>();
  function invalidate() {
    generation += 1;
    summaries.clear();
    pages.clear();
  }
  function assertCurrent(expected: number) {
    if (!viewer || expected !== generation) throw new Error("Feedback permission or visibility changed.");
  }
  function fresh<T>(cache: Map<string, FeedbackRead<T>>, key: string): FeedbackRead<T> | undefined {
    const entry = cache.get(key);
    if (entry && entry.expiresAt > now()) return entry;
    cache.delete(key);
    return undefined;
  }
  function put<T>(cache: Map<string, FeedbackRead<T>>, key: string, entry: FeedbackRead<T>) {
    if (cache.size >= 400) cache.delete(cache.keys().next().value as string);
    cache.set(key, entry);
  }
  return {
    invalidate,
    setViewer(next: string | null) {
      if (next !== viewer) { viewer = next; invalidate(); }
    },
    async summaries(values: unknown[]): Promise<FeedbackRead<Record<string, FeedbackMachineSummary>>> {
      const expected = generation;
      assertCurrent(expected);
      const ids = feedbackIdBatches(values).flat();
      const missing = ids.filter((id) => !fresh(summaries, id));
      try {
        for (const machineIds of feedbackIdBatches(missing)) {
          assertCurrent(expected);
          const response = await transport.summaries(machineIds);
          assertCurrent(expected);
          const raw = response as { summaries?: unknown } | null;
          if (!raw || !Array.isArray(raw.summaries)) throw new Error("Invalid feedback summary response.");
          const requested = new Set(machineIds);
          const parsed = raw.summaries.map(parseFeedbackSummary);
          if (parsed.length !== requested.size || new Set(parsed.map((s) => s.providerMachineId)).size !== requested.size ||
            parsed.some((s) => !requested.has(s.providerMachineId))) throw new Error("Incomplete feedback summaries.");
          const expiresAt = now() + FEEDBACK_CACHE_TTL_MS;
          for (const summary of parsed) put(summaries, summary.providerMachineId, { value: summary, expiresAt });
        }
        assertCurrent(expected);
        let expiresAt = now() + FEEDBACK_CACHE_TTL_MS;
        const value: Record<string, FeedbackMachineSummary> = {};
        for (const id of ids) {
          const entry = fresh(summaries, id);
          if (entry) { value[id] = entry.value; expiresAt = Math.min(expiresAt, entry.expiresAt); }
        }
        return { value, expiresAt };
      } catch (error) {
        if (expected === generation) invalidate();
        throw error;
      }
    },
    async reviews(machineId: string, cursor: string | null): Promise<FeedbackRead<FeedbackReviewsPage>> {
      const expected = generation;
      assertCurrent(expected);
      if (!feedbackMachineId(machineId)) throw new Error("Machine identity is unavailable.");
      const key = JSON.stringify([machineId, cursor]);
      const cached = fresh(pages, key);
      if (cached) return cached;
      try {
        const value = parseFeedbackReviews(await transport.reviews(machineId, cursor));
        assertCurrent(expected);
        const entry = { value, expiresAt: now() + FEEDBACK_CACHE_TTL_MS };
        put(pages, key, entry);
        return entry;
      } catch (error) {
        if (expected === generation) invalidate();
        throw error;
      }
    },
  };
}
