import type {
  FeedbackControl,
  FeedbackMachineSummary,
  FeedbackProgramDocument,
  FeedbackProgramState,
  FeedbackProgramStatus,
  FeedbackReview,
  FeedbackReviewsPage,
} from "./feedbackTypes";

export const FEEDBACK_CACHE_TTL_MS = 60_000;
const MAX_PROVIDER_ID = "9223372036854775807";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const uuid = (value: unknown): boolean => typeof value === "string" && UUID.test(value);
const reference = (value: unknown): boolean => typeof value === "string" && value.startsWith("secure-store://feedback/") && uuid(value.slice("secure-store://feedback/".length));
const date = (value: unknown): string | null => typeof value === "string" && Number.isFinite(Date.parse(value)) ? value : null;
const object = (value: unknown): Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : {};
const text = (value: unknown): string | null =>
  typeof value === "string" && value.trim().length > 0 ? value : null;
const count = (value: unknown): number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0;
const finite = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value);
const ratio = (value: unknown): number | null => finite(value) && value >= 0 && value <= 1 ? value : null;

// Only the explicit native string field can identify a machine. Numbers may
// already have lost precision before JS sees them, even if they look integral.
export function feedbackMachineId(value: unknown): string | null {
  if (typeof value !== "string" || !/^[1-9][0-9]*$/.test(value)) return null;
  if (value.length > MAX_PROVIDER_ID.length ||
    (value.length === MAX_PROVIDER_ID.length && value > MAX_PROVIDER_ID)) return null;
  return value;
}

// Existing rental actions use numbers. Safe integers alone can cross the new
// string-only feedback seam; never round/stringify an unsafe provider number.
export function feedbackInstanceId(value: unknown): string | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0
    ? feedbackMachineId(String(value)) : feedbackMachineId(value);
}

export function defaultFeedbackProgram(): FeedbackProgramState {
  return {
    schemaVersion: 1, enabled: false, enrollmentStatus: "not_enrolled",
    installationId: null, userId: null, credentialRef: null, deletionCodeRef: null,
    consentEpoch: 0, privacyVersion: null, termsVersion: null, consentedAt: null,
    optionalContextEnabled: false, pendingControlAction: null,
    enrollmentRequestId: null, enrollmentProofRef: null, enrollmentStartedAt: null,
    receiptId: null, receiptStatus: null, receiptTokenRef: null, collectionPaused: false,
  };
}

export function normalizeFeedbackProgram(value: unknown): FeedbackProgramState {
  const state = object(value);
  if (state.schemaVersion !== 1 ||
    (state.collectionPaused !== undefined && typeof state.collectionPaused !== "boolean")) return defaultFeedbackProgram();
  const control = object(state.pendingControlAction);
  const pending: FeedbackControl | null = state.pendingControlAction === null ? null :
    (control.kind === "consent" || control.kind === "delete") && uuid(control.requestId) &&
    count(control.expectedEpoch) > 0 && typeof control.enabled === "boolean" && typeof control.optionalContextEnabled === "boolean"
      ? { kind: control.kind, requestId: control.requestId as string, expectedEpoch: count(control.expectedEpoch),
        enabled: control.enabled, optionalContextEnabled: control.optionalContextEnabled } : null;
  if (state.pendingControlAction !== null && pending === null) return defaultFeedbackProgram();
  if ([state.credentialRef, state.deletionCodeRef, state.enrollmentProofRef, state.receiptTokenRef]
    .some((value) => value !== null && value !== undefined && !reference(value))) return defaultFeedbackProgram();
  const normalized: FeedbackProgramState = {
    schemaVersion: 1, enabled: false,
    enrollmentStatus: text(state.enrollmentStatus) ?? "not_enrolled",
    installationId: text(state.installationId), userId: text(state.userId),
    credentialRef: text(state.credentialRef), deletionCodeRef: text(state.deletionCodeRef),
    consentEpoch: count(state.consentEpoch), privacyVersion: text(state.privacyVersion),
    termsVersion: text(state.termsVersion), consentedAt: text(state.consentedAt),
    optionalContextEnabled: state.optionalContextEnabled === true, pendingControlAction: pending,
    enrollmentRequestId: text(state.enrollmentRequestId), enrollmentProofRef: text(state.enrollmentProofRef),
    enrollmentStartedAt: text(state.enrollmentStartedAt), receiptId: text(state.receiptId), receiptStatus: text(state.receiptStatus),
    receiptTokenRef: text(state.receiptTokenRef), collectionPaused: state.collectionPaused === true,
  };
  normalized.enabled = state.enabled === true && normalized.enrollmentStatus === "enrolled" &&
    uuid(normalized.installationId) && uuid(normalized.userId) && reference(normalized.credentialRef) && reference(normalized.deletionCodeRef) &&
    normalized.consentEpoch > 0 && normalized.privacyVersion !== null && normalized.termsVersion !== null &&
    normalized.consentedAt !== null && Number.isFinite(Date.parse(normalized.consentedAt)) && pending === null &&
    typeof state.optionalContextEnabled === "boolean" && !normalized.collectionPaused;
  return normalized;
}

export function normalizeFeedbackStatus(value: unknown): FeedbackProgramStatus {
  const raw = object(value);
  const state = normalizeFeedbackProgram(value);
  const configured = raw.configured === true;
  const supported = raw.supported === true;
  return {
    ...state, enabled: state.enabled && configured && supported,
    configured, supported, pendingSessions: count(raw.pendingSessions),
    statusMessage: text(raw.statusMessage),
  };
}

export function feedbackViewerKey(status: FeedbackProgramStatus): string | null {
  return status.enabled ? JSON.stringify([
    status.installationId, status.userId, status.consentEpoch,
    status.privacyVersion, status.termsVersion, status.optionalContextEnabled,
  ]) : null;
}

export function feedbackPolicyUrl(value: unknown): string | null {
  if (typeof value !== "string") return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}

export function parseFeedbackDocument(value: unknown): FeedbackProgramDocument {
  const raw = object(value);
  if (raw.schemaVersion !== 1 || typeof raw.available !== "boolean" ||
    typeof raw.collectionPaused !== "boolean" || !text(raw.privacyVersion) || !text(raw.termsVersion)) {
    throw new Error("The feedback service returned an unsupported program document.");
  }
  return {
    schemaVersion: 1, available: raw.available, collectionPaused: raw.collectionPaused,
    privacyVersion: raw.privacyVersion as string, termsVersion: raw.termsVersion as string,
    privacyUrl: feedbackPolicyUrl(raw.privacyUrl) ?? "", termsUrl: feedbackPolicyUrl(raw.termsUrl) ?? "",
    deletionUrl: feedbackPolicyUrl(raw.deletionUrl) ?? "", contact: text(raw.contact) ?? "",
    disclosure: text(raw.disclosure) ?? "", metrics: Array.isArray(raw.metrics) ? raw.metrics : [],
  };
}

export function canJoinFeedback(status: FeedbackProgramStatus, document: FeedbackProgramDocument | null): boolean {
  return status.configured && status.supported && !status.collectionPaused && status.pendingControlAction === null &&
    document !== null && document.available && !document.collectionPaused &&
    !!document.privacyUrl && !!document.termsUrl && !!document.deletionUrl && !!document.contact && !!document.disclosure;
}

export function feedbackIdBatches(values: unknown[]): string[][] {
  const ids = [...new Set(values.map(feedbackMachineId).filter((id): id is string => id !== null))];
  const batches: string[][] = [];
  for (let offset = 0; offset < ids.length; offset += 100) batches.push(ids.slice(offset, offset + 100));
  return batches;
}

export function parseFeedbackSummary(value: unknown): FeedbackMachineSummary {
  const raw = object(value);
  const machineId = feedbackMachineId(raw.providerMachineId);
  if (!machineId) throw new Error("Feedback summary has no lossless machine identity.");
  const reliability = object(raw.reliability);
  const reviews = object(raw.reviews);
  const jitter = object(raw.jitter);
  if ([reliability.eligibleSessions, reliability.successfulSessions, reliability.failedSessions, reviews.commentCount, reviews.ratingCount]
    .some((value) => !finite(value) || !Number.isSafeInteger(value) || value < 0)) {
    throw new Error("Feedback summary has invalid outcome or review counts.");
  }
  const eligibleSessions = count(reliability.eligibleSessions);
  const successfulSessions = count(reliability.successfulSessions);
  const failedSessions = count(reliability.failedSessions);
  const validOutcome = reliability.source === "no_land" &&
    reliability.basis === "ready_and_paired_before_destroy" && reliability.scope === "retained_history" &&
    successfulSessions + failedSessions === eligibleSessions;
  const validJitter = [jitter.meanMs, jitter.minMs, jitter.maxMs].every((n) => finite(n) && n >= 0) &&
    count(jitter.sampleCount) > 0 && count(jitter.contributingSessions) > 0 &&
    jitter.metricVersion === "rtt_jitter_ewma_window60_report1s_v1" &&
    (jitter.completeness === undefined || jitter.completeness === "full_window") &&
    (jitter.minMs as number) <= (jitter.meanMs as number) && (jitter.meanMs as number) <= (jitter.maxMs as number) &&
    ratio(jitter.highSampleRatio) !== null && ratio(jitter.severeSampleRatio) !== null;
  return {
    providerMachineId: machineId,
    reliability: {
      source: "no_land", ratio: validOutcome && eligibleSessions >= 2 ? ratio(reliability.ratio) : null,
      eligibleSessions, successfulSessions, failedSessions,
      basis: text(reliability.basis) ?? "", scope: text(reliability.scope) ?? "",
      firstObservedAt: date(reliability.firstObservedAt), lastObservedAt: date(reliability.lastObservedAt),
    },
    jitter: validJitter ? {
      meanMs: jitter.meanMs as number, minMs: jitter.minMs as number, maxMs: jitter.maxMs as number,
      sampleCount: count(jitter.sampleCount), contributingSessions: count(jitter.contributingSessions),
      highSampleRatio: jitter.highSampleRatio as number, severeSampleRatio: jitter.severeSampleRatio as number,
      metricVersion: jitter.metricVersion as string, scope: text(jitter.scope) ?? "all_routes",
    } : null,
    reviews: {
      commentCount: count(reviews.commentCount), ratingCount: count(reviews.ratingCount),
      averageScore: count(reviews.ratingCount) > 0 && finite(reviews.averageScore) && reviews.averageScore >= 1 && reviews.averageScore <= 10
        ? reviews.averageScore : null,
    },
    asOf: date(raw.asOf),
  };
}

export function displayFeedbackReliability(providerRatio: number, summary?: FeedbackMachineSummary) {
  const community = summary?.reliability;
  const communityRatio = ratio(community?.ratio);
  if (community && Number.isSafeInteger(community.eligibleSessions) && community.eligibleSessions >= 2 && communityRatio !== null) {
    return { ratio: communityRatio, source: "No Land" as const, sessions: community.eligibleSessions };
  }
  return { ratio: providerRatio, source: "Vast" as const, sessions: community?.eligibleSessions ?? 0 };
}

export function parseFeedbackReviews(value: unknown): FeedbackReviewsPage {
  const raw = object(value);
  if (!Array.isArray(raw.reviews) || raw.reviews.length > 20 ||
    !(raw.nextCursor === null || typeof raw.nextCursor === "string")) throw new Error("Invalid feedback review page.");
  const reviews = raw.reviews.map((item): FeedbackReview => {
    const review = object(item);
    const id = text(review.reviewId);
    if (!uuid(id)) throw new Error("Invalid feedback review identity.");
    const hardware = object(review.hardwareSnapshot);
    return {
      reviewId: id as string,
      score: finite(review.score) && Number.isInteger(review.score) && review.score >= 1 && review.score <= 10 ? review.score : null,
      comment: typeof review.comment === "string" ? review.comment.slice(0, 2000) : null,
      // Never display the exact session time, even if a server accidentally adds one.
      publishedDate: typeof review.publishedDate === "string" && /^\d{4}-\d{2}-\d{2}/.test(review.publishedDate)
        ? review.publishedDate.slice(0, 10) : "Date unavailable",
      hardwareSnapshot: review.hardwareSnapshot ? {
        gpuName: text(hardware.gpuName), rentedGpuCount: count(hardware.rentedGpuCount) || null,
        country: text(hardware.country), region: text(hardware.region),
      } : null,
    };
  });
  return { reviews, nextCursor: text(raw.nextCursor) };
}

export function normalizeFeedbackReview(score: number | null, comment: string): { score: number | null; comment: string | null } | null {
  if (score !== null && (!Number.isInteger(score) || score < 1 || score > 10)) return null;
  if (comment.length > 2000) return null;
  const normalizedComment = comment.trim() || null;
  return score === null && normalizedComment === null ? null : { score, comment: normalizedComment };
}

export async function stageFeedbackThenDestroy(
  stage: () => Promise<unknown>,
  destroy: () => Promise<void>,
  onWarning: (message: string) => void,
  timeoutMs = 2500,
): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      stage(),
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("Feedback save timed out")), timeoutMs); }),
    ]);
  } catch {
    onWarning("Feedback could not be confirmed saved. Destruction will continue; a delayed native save may still complete.");
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
  await destroy();
}
