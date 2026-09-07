import type { DiagnosticReportResponse, SystemHealthReport } from "./types";
import { GITHUB_NEW_ISSUE_URL } from "./constants";

interface DiagnosticIssueInput {
  report: DiagnosticReportResponse;
  reason: string;
  error?: string | null;
  health?: SystemHealthReport | null;
}

function truncate(value: string, maxLength: number): string {
  if (value.length <= maxLength) {
    return value;
  }
  return `${value.slice(0, maxLength - 3)}...`;
}

export function buildDiagnosticIssueUrl({
  report,
  reason,
  error,
  health,
}: DiagnosticIssueInput): string {
  const title = `[Crash report] ${reason}`;
  const failingChecks = health?.probes
    .filter((probe) => probe.status === "failed")
    .map((probe) => `- ${probe.label}: ${probe.summary}`)
    .join("\n");
  const warnings = health?.probes
    .filter((probe) => probe.status === "warning")
    .slice(0, 10)
    .map((probe) => `- ${probe.label}: ${probe.summary}`)
    .join("\n");

  const body = [
    "## What happened?",
    "Describe what you were doing when the error happened.",
    "",
    "## Generated diagnostic report",
    "The app generated and pasted the redacted diagnostic report below.",
    "",
    `Local copy: \`${report.path}\``,
    "",
    "```markdown",
    truncate(report.reportMarkdown, 4200),
    "```",
    "",
    "## App summary",
    `- Reason: \`${reason}\``,
    `- Health summary: ${health?.summary ?? report.summary}`,
    health ? `- OS: ${health.os} / ${health.arch}` : null,
    "",
    failingChecks ? "## Blocking health checks" : null,
    failingChecks || null,
    warnings ? "" : null,
    warnings ? "## Health warnings" : null,
    warnings || null,
    error ? "" : null,
    error ? "## Error shown in app" : null,
    error ? "```text" : null,
    error ? truncate(error, 900) : null,
    error ? "```" : null,
  ]
    .filter((line): line is string => line !== null)
    .join("\n");

  const params = new URLSearchParams({
    title,
    body: truncate(body, 7800),
    labels: "bug,crash-report",
  });

  return `${GITHUB_NEW_ISSUE_URL}?${params.toString()}`;
}
