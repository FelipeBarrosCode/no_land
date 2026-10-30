import { Button } from "../../components/ui/Button";
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
  if (!enabled) return <p>Reliability: {(offer.reliability * 100).toFixed(1)}%</p>;
  const machineId = feedbackMachineId(offer.machineId);
  const explanation = !machineId ? "Native machine identity is missing or not a lossless decimal string. No history is guessed." :
    unavailable ? "Community data is unavailable. Vast fallback; open reviews to retry." :
    "Reported rentals that reached ready and paired before destruction. Includes setup failures and rentals abandoned during setup. Small samples may be unrepresentative. This is not a guarantee of future performance.";
  return (
    <div className="col-span-full min-w-0 space-y-2 break-words border-t border-[#3e4270] pt-2 text-[1.05rem] leading-snug [overflow-wrap:anywhere]">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0" title={`${explanation} Original Vast reliability: ${(offer.reliability * 100).toFixed(1)}%.`}>
          <p>Reliability <strong className="text-neon-cyan">{(reliability.ratio * 100).toFixed(1)}%</strong> · {reliability.source}</p>
          {summary && <p className="text-sm text-[#9ec4df]">{reliability.sessions} No Land {reliability.sessions === 1 ? "session" : "sessions"}
            {reliability.sessions >= 2 && reliability.sessions < 10 ? " · Small sample" : ""}
            {reliability.sessions < 2 ? " · At least 2 required for replacement" : ""}</p>}
          {reliability.source === "No Land" && <p className="text-sm text-[#9ec4df]">Original Vast: {(offer.reliability * 100).toFixed(1)}%</p>}
        </div>
        <p title={summary?.jitter ? `${summary.jitter.sampleCount} samples across ${summary.jitter.contributingSessions} rentals. Min ${summary.jitter.minMs.toFixed(1)} ms; max ${summary.jitter.maxMs.toFixed(1)} ms. Source: ${summary.jitter.metricVersion}; ${summary.jitter.scope}. Jitter depends on route, connection, and client as well as machine.` : explanation}>
          Jitter <strong>{summary?.jitter ? `${summary.jitter.meanMs.toFixed(1)} ms` : "N/A"}</strong>
          {summary?.jitter && <span className="block text-sm text-[#9ec4df]">{summary.jitter.sampleCount} samples · {summary.jitter.contributingSessions} rentals</span>}
        </p>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Button variant="ghost" className="min-h-9 px-2 text-xs" disabled={!machineId}
          aria-label={`View machine reviews for ${offer.hostLabel}`}
          title={machineId ? "View published comments, ratings, and moderation controls" : explanation}
          onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") event.stopPropagation(); }}
          onClick={(event) => { event.stopPropagation(); onReviews(); }}>
          <svg aria-hidden="true" viewBox="0 0 24 24" className="h-5 w-5" fill="none" stroke="currentColor" strokeWidth="2"><path d="M4 3h16v13H9l-5 5V3Z" /><path d="M8 7h8M8 11h6" /></svg>
          Comments {summary ? summary.reviews.commentCount > 0 ? summary.reviews.commentCount : "N/A · 0 comments" : "N/A"}
        </Button>
        <span className="text-sm text-[#9ec4df]">{summary?.reviews.ratingCount ? `${summary.reviews.averageScore?.toFixed(1) ?? "N/A"}/10 · ${summary.reviews.ratingCount} ratings` : "Ratings N/A"}</span>
      </div>
      {!machineId || unavailable ? <p className="text-sm text-[#ffd78a]">{explanation}</p> : null}
      {summary && <p className="text-sm text-[#9ec4df]">Setup observation range: {summary.reliability.firstObservedAt && summary.reliability.lastObservedAt
        ? `${new Date(summary.reliability.firstObservedAt).toLocaleDateString()} – ${new Date(summary.reliability.lastObservedAt).toLocaleDateString()}`
        : "Not supplied"}</p>}
      {summary?.asOf && <p className="text-sm text-[#9ec4df]">Summary as of: {new Date(summary.asOf).toLocaleString()}</p>}
    </div>
  );
}
