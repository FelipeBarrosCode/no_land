import type { BudgetStatus } from "../../lib/types";

export function formatUsd(value: number, fractionDigits = 2): string {
  if (!Number.isFinite(value)) {
    return "$0.00";
  }
  return `$${value.toFixed(fractionDigits)}`;
}

export function formatHourlyUsd(value: number): string {
  return `${formatUsd(value, 3)}/hr`;
}

export function formatMonth(month: string): string {
  const [year, monthIndex] = month.split("-").map(Number);
  if (!year || !monthIndex) {
    return month;
  }
  return new Date(Date.UTC(year, monthIndex - 1, 1)).toLocaleDateString(undefined, {
    month: "long",
    year: "numeric",
    timeZone: "UTC",
  });
}

export function formatSessionDuration(startedAt: string | null, now: Date = new Date()): string {
  if (!startedAt) {
    return "--";
  }
  const started = new Date(startedAt).getTime();
  if (Number.isNaN(started)) {
    return "--";
  }
  const totalMinutes = Math.max(0, Math.floor((now.getTime() - started) / 60000));
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  return hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
}

export function budgetStatusLabel(status: BudgetStatus): string {
  switch (status) {
    case "exceeded":
      return "Budget reached";
    case "warning":
      return "Approaching budget";
    case "ok":
      return "Within budget";
    default:
      return "No budget set";
  }
}

export function budgetStatusClass(status: BudgetStatus): string {
  switch (status) {
    case "exceeded":
      return "border-[#ff8ca2] bg-[#361220] text-[#ffc1cf]";
    case "warning":
      return "border-[#ffd166] bg-[#3c2c13] text-[#ffe0a3]";
    case "ok":
      return "border-[#7bff48] bg-[#142815] text-[#b4ff88]";
    default:
      return "border-[#3a4068] bg-[#10152f] text-[#9ec4df]";
  }
}
