import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { updateBudgetSettings } from "../../lib/backend";
import { errorMessage } from "../../lib/errorMessage";
import { SpendPanel } from "./SpendPanel";
import { useSpendSummary } from "./useSpendSummary";

export function BudgetSettings() {
  const { summary, setSummary } = useSpendSummary();
  const [budget, setBudget] = useState("");
  const [warnAt, setWarnAt] = useState("80");
  const [autoStop, setAutoStop] = useState(false);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    if (!summary || loaded) {
      return;
    }
    setBudget(summary.budget.monthlyBudgetUsd > 0 ? String(summary.budget.monthlyBudgetUsd) : "");
    setWarnAt(String(summary.budget.warnAtPercent));
    setAutoStop(summary.budget.autoStopAtBudget);
    setLoaded(true);
  }, [summary, loaded]);

  async function save() {
    const monthlyBudgetUsd = budget.trim() === "" ? 0 : Number(budget);
    const warnAtPercent = Math.round(Number(warnAt));
    if (!Number.isFinite(monthlyBudgetUsd) || monthlyBudgetUsd < 0) {
      setMessage("Enter a budget of 0 or more (leave empty for no budget).");
      return;
    }
    if (!Number.isFinite(warnAtPercent) || warnAtPercent < 1 || warnAtPercent > 100) {
      setMessage("Warning threshold must be between 1 and 100 percent.");
      return;
    }
    setSaving(true);
    setMessage(null);
    try {
      const next = await updateBudgetSettings({
        monthlyBudgetUsd,
        warnAtPercent,
        autoStopAtBudget: autoStop && monthlyBudgetUsd > 0,
      });
      setSummary(next);
      setMessage("Budget saved.");
    } catch (error) {
      setMessage(errorMessage(error));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="space-y-4">
      <Card className="pixel-frame">
        <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">Monthly Budget</h2>
        <p className="mt-2 text-[1.05rem] text-[#a8bed6]">
          No Land estimates what your instances cost while it runs and warns you before you overspend.
        </p>
        <div className="mt-4 grid gap-4 md:grid-cols-2">
          <InputField
            label="Monthly budget (USD)"
            type="number"
            min={0}
            step="1"
            placeholder="No budget"
            value={budget}
            onChange={(event) => setBudget(event.target.value)}
          />
          <InputField
            label="Warn at (% of budget)"
            type="number"
            min={1}
            max={100}
            value={warnAt}
            onChange={(event) => setWarnAt(event.target.value)}
          />
        </div>
        <label className="mt-4 flex cursor-pointer items-start gap-3 border border-[#3d426f] bg-[#10152f] p-4">
          <input
            type="checkbox"
            className="mt-1 h-5 w-5 accent-[#7bff48]"
            checked={autoStop}
            disabled={budget.trim() === "" || Number(budget) <= 0}
            onChange={(event) => setAutoStop(event.target.checked)}
          />
          <span>
            <span className="block font-display text-[11px] uppercase tracking-[0.08em] text-white">
              Stop instances when the budget is reached
            </span>
            <span className="mt-1 block text-[1rem] leading-snug text-[#a8bed6]">
              Runs the usual shared-storage backup first, then stops each running instance. Stopped instances keep
              billing for storage. Starting an instance again overrides the stop for the rest of the month.
            </span>
          </span>
        </label>
        <div className="mt-4 flex items-center gap-3">
          <Button onClick={() => void save()} loading={saving} loadingText="Saving..." disabled={saving}>
            Save Budget
          </Button>
          {message ? <p className="text-[1rem] text-[#9ec4df]" aria-live="polite">{message}</p> : null}
        </div>
      </Card>
      <SpendPanel summary={summary} />
    </div>
  );
}
