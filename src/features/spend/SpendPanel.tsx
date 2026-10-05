import { Card } from "../../components/ui/Card";
import { HudBar } from "../../components/ui/HudBar";
import type { SpendSummary } from "../../lib/types";
import {
  budgetStatusClass,
  budgetStatusLabel,
  formatHourlyUsd,
  formatMonth,
  formatSessionDuration,
  formatUsd,
} from "./spendFormat";

interface Props {
  summary: SpendSummary | null;
}

export function SpendPanel({ summary }: Props) {
  if (!summary) {
    return null;
  }

  const budgetEnabled = summary.budgetStatus !== "disabled";
  const previousMonths = summary.months.filter((month) => month.month !== summary.month).slice(0, 3);

  return (
    <Card className="pixel-frame" data-testid="spend-panel">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <h3 className="font-display text-sm uppercase tracking-[0.12em] text-white">Spending</h3>
        <span
          className={`border px-2 py-1 font-display text-[10px] uppercase tracking-[0.08em] ${budgetStatusClass(summary.budgetStatus)}`}
        >
          {budgetStatusLabel(summary.budgetStatus)}
        </span>
      </div>

      <div className="grid gap-3 text-[#bfd3ee] sm:grid-cols-3">
        <div>
          <p className="font-display text-[10px] uppercase tracking-widest text-[#9ad9ff]">
            {formatMonth(summary.month)}
          </p>
          <p className="text-[1.6rem] leading-tight text-white">{formatUsd(summary.monthToDateUsd)}</p>
        </div>
        <div>
          <p className="font-display text-[10px] uppercase tracking-widest text-[#9ad9ff]">Burning now</p>
          <p className="text-[1.6rem] leading-tight text-white">
            {formatHourlyUsd(summary.currentBurnUsdPerHour)}
          </p>
        </div>
        <div>
          <p className="font-display text-[10px] uppercase tracking-widest text-[#9ad9ff]">Month projection</p>
          <p className="text-[1.6rem] leading-tight text-white">{formatUsd(summary.projectedMonthUsd)}</p>
        </div>
      </div>

      {budgetEnabled ? (
        <div className="mt-3">
          <HudBar
            label="Monthly budget"
            value={summary.monthToDateUsd}
            max={summary.budget.monthlyBudgetUsd}
            valueLabel={`${formatUsd(summary.monthToDateUsd)} / ${formatUsd(summary.budget.monthlyBudgetUsd)}`}
          />
          {summary.budget.autoStopAtBudget ? (
            <p className="mt-1 text-[0.95rem] text-[#8db7d8]">
              Running instances are stopped (after backup) when the budget is reached.
            </p>
          ) : null}
        </div>
      ) : (
        <p className="mt-3 text-[1rem] text-[#8db7d8]">
          Set a monthly budget in Settings → Budget to get warnings and optional auto-stop.
        </p>
      )}

      {summary.instances.length > 0 ? (
        <ul className="mt-3 space-y-2">
          {summary.instances.map((instance) => (
            <li
              key={`${instance.provider}-${instance.instanceId}`}
              className="flex flex-wrap items-center justify-between gap-2 border border-[#3a4068] bg-[#10152f]/60 px-3 py-2 text-[1rem] text-[#bfd3ee]"
            >
              <span className="text-white">
                {instance.label || `Instance ${instance.instanceId}`}
                {instance.gpuName ? <span className="text-[#8db7d8]"> · {instance.gpuName}</span> : null}
              </span>
              <span>
                {instance.running
                  ? `Session ${formatUsd(instance.sessionUsd)} · ${formatSessionDuration(instance.sessionStartedAt)}`
                  : "Stopped (storage only)"}
                {" · "}
                {formatHourlyUsd(instance.currentHourlyUsd)}
              </span>
            </li>
          ))}
        </ul>
      ) : null}

      {previousMonths.length > 0 ? (
        <p className="mt-3 text-[0.95rem] text-[#8db7d8]">
          {previousMonths
            .map((month) => `${formatMonth(month.month)}: ${formatUsd(month.totalUsd)}`)
            .join(" · ")}
        </p>
      ) : null}
      <p className="mt-2 text-[0.85rem] text-[#6f8db0]">
        Estimated locally from observed instance time and prices. Your provider's invoice is authoritative.
      </p>
    </Card>
  );
}
