import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { updateBudgetSettings } from "../../lib/backend";
import { errorMessage } from "../../lib/errorMessage";
import { useLocalization } from "../../lib/i18n";
import { SpendPanel } from "./SpendPanel";
import { useSpendSummary } from "./useSpendSummary";

export function BudgetSettings() {
  const { t } = useLocalization();
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
      setMessage(t("budget.error.amount"));
      return;
    }
    if (!Number.isFinite(warnAtPercent) || warnAtPercent < 1 || warnAtPercent > 100) {
      setMessage(t("budget.error.threshold"));
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
      setMessage(t("budget.saved"));
    } catch (error) {
      setMessage(errorMessage(error));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="space-y-4">
      <Card className="pixel-frame">
        <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">{t("budget.title")}</h2>
        <p className="mt-2 text-[1.05rem] text-[#a8bed6]">
          {t("budget.description")}
        </p>
        <div className="mt-4 grid gap-4 md:grid-cols-2">
          <InputField
            label={t("budget.amount.label")}
            type="number"
            min={0}
            step="1"
            placeholder={t("budget.amount.placeholder")}
            value={budget}
            onChange={(event) => setBudget(event.target.value)}
          />
          <InputField
            label={t("budget.warn_at.label")}
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
              {t("budget.auto_stop.title")}
            </span>
            <span className="mt-1 block text-[1rem] leading-snug text-[#a8bed6]">
              {t("budget.auto_stop.description")}
            </span>
          </span>
        </label>
        <div className="mt-4 flex items-center gap-3">
          <Button onClick={() => void save()} loading={saving} loadingText={t("budget.saving")} disabled={saving}>
            {t("budget.save")}
          </Button>
          {message ? <p className="text-[1rem] text-[#9ec4df]" aria-live="polite">{message}</p> : null}
        </div>
      </Card>
      <SpendPanel summary={summary} />
    </div>
  );
}
