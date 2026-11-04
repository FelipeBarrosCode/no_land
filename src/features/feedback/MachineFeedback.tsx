import { Button } from "../../components/ui/Button";
import { translate } from "../../lib/i18n";
import { displayFeedbackReliability, feedbackMachineId } from "../../lib/feedbackModel";
import type { FeedbackMachineSummary } from "../../lib/feedbackTypes";
import type { OfferCandidate } from "../../lib/types";

interface Props {
  offer: OfferCandidate;
  summary?: FeedbackMachineSummary;
  enabled: boolean;
  unavailable: boolean;
  onReviews: () => void;
}

export function MachineFeedback({ offer, summary, enabled, unavailable, onReviews }: Props) {
  const reliability = displayFeedbackReliability(offer.reliability, enabled ? summary : undefined);
  if (!enabled) return <p>{translate("generated.71d0acd465564246")}: {(offer.reliability * 100).toFixed(1)}%</p>;
  const machineId = feedbackMachineId(offer.machineId);
  const explanation = !machineId ? translate("feedback.machine.missing") :
    unavailable ? translate("feedback.machine.unavailable") : translate("feedback.machine.explanation");
  return (
    <div className="col-span-full min-w-0 space-y-2 break-words border-t border-[#3e4270] pt-2 text-[1.05rem] leading-snug [overflow-wrap:anywhere]">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0" title={`${explanation} ${translate("feedback.machine.original", { value: (offer.reliability * 100).toFixed(1) })}`}>
          <p>{translate("generated.71d0acd465564246")} <strong className="text-neon-cyan">{(reliability.ratio * 100).toFixed(1)}%</strong> · {reliability.source}</p>
          {summary && <p className="text-sm text-[#9ec4df]">{translate("feedback.machine.sessions", { count: reliability.sessions })}
            {reliability.sessions >= 2 && reliability.sessions < 10 ? translate("feedback.machine.smallSample") : ""}
            {reliability.sessions < 2 ? translate("feedback.machine.minimum") : ""}</p>}
          {reliability.source === "No Land" && <p className="text-sm text-[#9ec4df]">{translate("feedback.machine.original", { value: (offer.reliability * 100).toFixed(1) })}</p>}
        </div>
        <p title={summary?.jitter ? translate("feedback.machine.jitterDetails", { samples: summary.jitter.sampleCount, rentals: summary.jitter.contributingSessions, min: summary.jitter.minMs.toFixed(1), max: summary.jitter.maxMs.toFixed(1), version: summary.jitter.metricVersion, scope: summary.jitter.scope }) : explanation}>
          {translate("feedback.machine.jitter")} <strong>{summary?.jitter ? `${summary.jitter.meanMs.toFixed(1)} ms` : translate("feedback.na")}</strong>
          {summary?.jitter && <span className="block text-sm text-[#9ec4df]">{translate("feedback.machine.samples", { samples: summary.jitter.sampleCount, rentals: summary.jitter.contributingSessions })}</span>}
        </p>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Button variant="ghost" className="min-h-9 px-2 text-xs" disabled={!machineId}
          aria-label={translate("feedback.machine.reviewsFor", { host: offer.hostLabel })}
          title={machineId ? translate("feedback.machine.reviewsHelp") : explanation}
          onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") event.stopPropagation(); }}
          onClick={(event) => { event.stopPropagation(); onReviews(); }}>
          <svg aria-hidden="true" viewBox="0 0 24 24" className="h-5 w-5" fill="none" stroke="currentColor" strokeWidth="2"><path d="M4 3h16v13H9l-5 5V3Z" /><path d="M8 7h8M8 11h6" /></svg>
          {translate("feedback.comments")} {summary ? summary.reviews.commentCount > 0 ? summary.reviews.commentCount : translate("feedback.noCommentsCount") : translate("feedback.na")}
        </Button>
        <span className="text-sm text-[#9ec4df]">{summary?.reviews.ratingCount ? translate("feedback.machine.ratings", { score: summary.reviews.averageScore?.toFixed(1) ?? translate("feedback.na"), count: summary.reviews.ratingCount }) : translate("feedback.ratingsNA")}</span>
      </div>
      {!machineId || unavailable ? <p className="text-sm text-[#ffd78a]">{explanation}</p> : null}
      {summary && <p className="text-sm text-[#9ec4df]">{translate("feedback.machine.observationRange")} {summary.reliability.firstObservedAt && summary.reliability.lastObservedAt
        ? `${new Date(summary.reliability.firstObservedAt).toLocaleDateString()} – ${new Date(summary.reliability.lastObservedAt).toLocaleDateString()}`
        : translate("feedback.notSupplied")}</p>}
      {summary?.asOf && <p className="text-sm text-[#9ec4df]">{translate("feedback.machine.asOf")} {new Date(summary.asOf).toLocaleString()}</p>}
    </div>
  );
}
