import { invokeSafe } from "./tauri";
import { normalizeFeedbackStatus, parseFeedbackDocument } from "./feedbackModel";
import type { FeedbackEnrollmentRequest, FeedbackProgramStatus } from "./feedbackTypes";

// All feedback transport, authentication, and durable writes stay native.
export async function feedbackProgramStatus(): Promise<FeedbackProgramStatus> {
  return normalizeFeedbackStatus(await invokeSafe<unknown>("feedback_program_status"));
}
export async function feedbackProgramNotices() {
  return parseFeedbackDocument(await invokeSafe<unknown>("feedback_program_notices"));
}
export async function feedbackProgramEnroll(request: FeedbackEnrollmentRequest) {
  return normalizeFeedbackStatus(await invokeSafe<unknown>("feedback_program_enroll", { request }));
}
export async function feedbackProgramConsent(enabled: boolean, optionalContextEnabled: boolean) {
  return normalizeFeedbackStatus(await invokeSafe<unknown>("feedback_program_consent", { enabled, optionalContextEnabled }));
}
export async function feedbackProgramDelete() {
  return normalizeFeedbackStatus(await invokeSafe<unknown>("feedback_program_delete"));
}
export const feedbackMachineSummaries = (machineIds: string[]) =>
  invokeSafe<unknown>("feedback_machine_summaries", { machineIds });
export const feedbackMachineReviews = (machineId: string, cursor: string | null) =>
  invokeSafe<unknown>("feedback_machine_reviews", { machineId, cursor, limit: 20 });
export const feedbackReviewReport = (reviewId: string, reason: string) =>
  invokeSafe<unknown>("feedback_review_report", { reviewId, reason });
export const feedbackReviewBlockAuthor = (reviewId: string) =>
  invokeSafe<unknown>("feedback_review_block_author", { reviewId });
export async function feedbackReviewEligibility(providerInstanceId: string): Promise<boolean> {
  const result = await invokeSafe<{ eligible?: unknown }>("feedback_review_eligibility", { providerInstanceId });
  return result?.eligible === true;
}
export const feedbackStageReview = (providerInstanceId: string, score: number | null, comment: string | null) =>
  invokeSafe<unknown>("feedback_stage_review", { providerInstanceId, score, comment });
