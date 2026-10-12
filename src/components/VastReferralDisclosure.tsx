import { translate } from "../lib/i18n";

interface Props {
  className?: string;
}

export function VastReferralDisclosure({ className = "" }: Props) {
  return (
    <p
      className={`rounded border border-amber-400/30 bg-amber-950/20 px-3 py-2 text-[0.95rem] leading-snug text-amber-100/80 ${className}`}
      role="note"
    >
      {translate("vast.referral.disclosure")}
    </p>
  );
}
