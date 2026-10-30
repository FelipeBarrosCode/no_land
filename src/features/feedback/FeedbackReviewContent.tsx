import type { FeedbackReview } from "../../lib/feedbackTypes";

export function FeedbackReviewContent({ review }: { review: FeedbackReview }) {
  return <>
    <div className="flex flex-wrap justify-between gap-2"><p>{review.score !== null ? `Rating ${review.score}/10` : "No rating"}</p><p>{review.publishedDate}</p></div>
    {review.hardwareSnapshot && <p className="text-sm text-[#9ec4df]">Rental hardware: {[
      review.hardwareSnapshot.gpuName, review.hardwareSnapshot.rentedGpuCount ? `${review.hardwareSnapshot.rentedGpuCount} GPU(s)` : null,
      review.hardwareSnapshot.region, review.hardwareSnapshot.country,
    ].filter(Boolean).join(" · ") || "Not supplied"}</p>}
    {review.comment ? <p className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{review.comment}</p> : <p className="text-[#9ec4df]">N/A — Rating only, no comment.</p>}
  </>;
}
