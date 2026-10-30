import { create } from "zustand";
import {
  feedbackMachineReviews, feedbackMachineSummaries, feedbackProgramConsent, feedbackProgramDelete,
  feedbackProgramEnroll, feedbackProgramStatus, feedbackReviewBlockAuthor, feedbackReviewReport,
} from "../../lib/feedbackBackend";
import { createFeedbackDataClient } from "../../lib/feedbackData";
import { feedbackViewerKey, normalizeFeedbackStatus } from "../../lib/feedbackModel";
import type {
  FeedbackEnrollmentRequest, FeedbackMachineSummary, FeedbackProgramStatus, FeedbackRead, FeedbackReviewsPage,
} from "../../lib/feedbackTypes";

const reader = createFeedbackDataClient({ summaries: feedbackMachineSummaries, reviews: feedbackMachineReviews });
const READ_ERROR = "Community feedback is unavailable. Vast reliability and normal rental actions are unchanged. Retry when ready.";

interface FeedbackStore {
  status: FeedbackProgramStatus;
  statusError: string | null;
  controlError: string | null;
  controlBusy: boolean;
  dataError: string | null;
  dataRevision: number;
  warning: string | null;
  refreshStatus: () => Promise<void>;
  enroll: (request: FeedbackEnrollmentRequest) => Promise<boolean>;
  consent: (enabled: boolean, optionalContextEnabled: boolean) => Promise<boolean>;
  deleteContributions: () => Promise<boolean>;
  invalidate: () => void;
  retryReads: () => void;
  loadSummaries: (ids: string[]) => Promise<FeedbackRead<Record<string, FeedbackMachineSummary>>>;
  loadReviews: (id: string, cursor: string | null) => Promise<FeedbackRead<FeedbackReviewsPage>>;
  report: (id: string, reason: string) => Promise<void>;
  block: (id: string) => Promise<void>;
  setWarning: (warning: string | null) => void;
}

// This store is an in-memory projection, never a state.json/credential writer.
export const useFeedbackStore = create<FeedbackStore>((set, get) => {
  let controlGeneration = 0;
  let statusSequence = 0;
  let requestedOff = false;
  function applyStatus(received: FeedbackProgramStatus) {
    const status = requestedOff ? { ...received, enabled: false } : received;
    const old = get().status;
    const identityChanged = feedbackViewerKey(status) !== feedbackViewerKey(old) ||
      JSON.stringify(status.pendingControlAction) !== JSON.stringify(old.pendingControlAction) ||
      status.pendingSessions !== old.pendingSessions;
    reader.setViewer(feedbackViewerKey(status));
    if (identityChanged) reader.invalidate();
    set((state) => ({
      status, statusError: null,
      ...(identityChanged ? { dataError: null, dataRevision: state.dataRevision + 1 } : {}),
    }));
  }
  function failRead() {
    reader.invalidate();
    set((state) => ({ dataError: READ_ERROR, dataRevision: state.dataRevision + 1 }));
  }
  async function control(action: () => Promise<FeedbackProgramStatus>, enabled: boolean) {
    if (get().controlBusy) return false;
    controlGeneration += 1;
    statusSequence += 1;
    set({ controlBusy: true, controlError: null });
    requestedOff = !enabled;
    applyStatus({ ...get().status, enabled: false });
    get().invalidate();
    try {
      applyStatus(await action());
      return true;
    } catch {
      // A failed response may follow a durable native write. Only local status
      // decides whether a withdrawal/deletion is pending; never claim success.
      try { applyStatus(await feedbackProgramStatus()); }
      catch { applyStatus(normalizeFeedbackStatus(null)); }
      set({ controlError: "The program change could not be confirmed. Check local status and any pending control below before retrying. Rental and streaming are unaffected." });
      return false;
    } finally { set({ controlBusy: false }); }
  }
  return {
    status: normalizeFeedbackStatus(null), statusError: null, controlError: null, controlBusy: false,
    dataError: null, dataRevision: 0, warning: null,
    refreshStatus: async () => {
      if (get().controlBusy) return;
      const expected = controlGeneration;
      const sequence = ++statusSequence;
      try {
        const status = await feedbackProgramStatus();
        if (expected === controlGeneration && sequence === statusSequence) applyStatus(status);
      } catch {
        if (expected === controlGeneration && sequence === statusSequence) {
          applyStatus(normalizeFeedbackStatus(null));
          set({ statusError: "Local feedback status is unavailable. The program UI is off until native consent can be verified." });
        }
      }
    },
    enroll: (request) => control(() => feedbackProgramEnroll(request), true),
    consent: (enabled, optional) => control(() => feedbackProgramConsent(enabled, optional), enabled),
    deleteContributions: () => control(feedbackProgramDelete, false),
    invalidate: () => {
      reader.invalidate();
      set((state) => ({ dataError: null, dataRevision: state.dataRevision + 1 }));
    },
    retryReads: () => get().invalidate(),
    loadSummaries: async (ids) => {
      if (!get().status.enabled || get().dataError) throw new Error(READ_ERROR);
      const expected = get().dataRevision;
      try { return await reader.summaries(ids); }
      catch (error) { if (get().status.enabled && get().dataRevision === expected) failRead(); throw error; }
    },
    loadReviews: async (id, cursor) => {
      if (!get().status.enabled || get().dataError) throw new Error(READ_ERROR);
      const expected = get().dataRevision;
      try { return await reader.reviews(id, cursor); }
      catch (error) { if (get().status.enabled && get().dataRevision === expected) failRead(); throw error; }
    },
    report: async (id, reason) => {
      const viewer = feedbackViewerKey(get().status);
      if (!viewer) throw new Error("Feedback is disabled.");
      try {
        await feedbackReviewReport(id, reason);
        if (feedbackViewerKey(get().status) === viewer) get().invalidate();
      } catch (error) {
        if (feedbackViewerKey(get().status) === viewer) failRead();
        throw error;
      }
    },
    block: async (id) => {
      const viewer = feedbackViewerKey(get().status);
      if (!viewer) throw new Error("Feedback is disabled.");
      try {
        await feedbackReviewBlockAuthor(id);
        if (feedbackViewerKey(get().status) === viewer) get().invalidate();
      } catch (error) {
        if (feedbackViewerKey(get().status) === viewer) failRead();
        throw error;
      }
    },
    setWarning: (warning) => set({ warning }),
  };
});
