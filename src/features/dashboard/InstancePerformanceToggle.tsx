import { useState } from "react";
import { setInstancePerformanceOverlay } from "../../lib/backend";
import type { RentedInstanceSummary } from "../../lib/types";
import { useAppStore } from "../../store/appStore";

export function InstancePerformanceToggle({ instance }: { instance: RentedInstanceSummary }) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function change(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      const saved = await setInstancePerformanceOverlay(instance.instanceId, enabled);
      useAppStore.setState((state) => ({
        rentedInstances: state.rentedInstances.map((current) => current.instanceId === instance.instanceId
          ? { ...current, performanceOverlayEnabled: saved } : current),
      }));
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="mt-3 rounded border border-[#3a4068] bg-[#10152f]/60 px-3 py-2">
      <label className="flex cursor-pointer items-center justify-between gap-3 text-sm text-[#d7e6f7]">
        <span>Performance overlay</span>
        <input
          type="checkbox"
          role="switch"
          aria-label={`Performance overlay for ${instance.label}`}
          checked={instance.performanceOverlayEnabled ?? false}
          disabled={saving}
          onChange={(event) => void change(event.target.checked)}
          className="h-4 w-4 accent-cyan-400"
        />
      </label>
      <p className="mt-1 text-xs text-[#8fa7c6]">Live FPS, latency, jitter and drops. Applies immediately during play.</p>
      {error ? <p role="alert" className="mt-1 text-xs text-red-300">{error}</p> : null}
    </div>
  );
}
