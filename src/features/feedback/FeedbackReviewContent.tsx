import type { FeedbackReview } from "../../lib/feedbackTypes";
import { translate } from "../../lib/i18n";

export function FeedbackReviewContent({ review }: { review: FeedbackReview }) {
  return <>
    <div className="flex flex-wrap justify-between gap-2"><p>{review.score !== null ? translate("feedback.rating", { score: review.score }) : translate("feedback.noRating")}</p><p>{review.publishedDate}</p></div>
    {review.hardwareSnapshot && <p className="text-sm text-[#9ec4df]">{translate("feedback.hardware")} {[
      review.hardwareSnapshot.gpuName, review.hardwareSnapshot.rentedGpuCount ? `${review.hardwareSnapshot.rentedGpuCount} GPU(s)` : null,
      review.hardwareSnapshot.region, review.hardwareSnapshot.country,
    ].filter(Boolean).join(" · ") || translate("feedback.notSupplied")}</p>}
    {review.comment ? <p className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{review.comment}</p> : <p className="text-[#9ec4df]">{translate("feedback.ratingOnly")}</p>}
  </>;
}
