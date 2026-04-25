import { useState } from "react";
import { Button } from "../../components/ui/Button";
import { SpriteIcon } from "../../components/ui/SpriteIcon";
import type { RentedInstanceSummary } from "../../lib/types";

interface Props {
  instance: RentedInstanceSummary;
  busy: boolean;
  instanceActionRunning: boolean;
  onPlay: (instanceId: number) => void;
  onSettings: (instanceId: number) => void;
  onRestore: (instanceId: number) => void;
  onReconnect: (instanceId: number) => void;
  onPause: (instanceId: number) => void;
  onDestroy: (instanceId: number) => void;
}

export function InstanceCardActions({
  instance,
  busy,
  instanceActionRunning,
  onPlay,
  onSettings,
  onRestore,
  onReconnect,
  onPause,
  onDestroy
}: Props) {
  const [showDestroyConfirm, setShowDestroyConfirm] = useState(false);
  const isRunning = instance.status.toLowerCase().includes("run");
  const actionDisabled = busy || instanceActionRunning;

  const handleDestroy = () => {
    if (showDestroyConfirm) {
      onDestroy(instance.instanceId);
      setShowDestroyConfirm(false);
    } else {
      setShowDestroyConfirm(true);
    }
  };

  return (
    <div className="space-y-2">
      <div className="grid grid-cols-2 gap-2">
        <Button
          className="w-full"
          disabled={actionDisabled}
          onClick={() => onPlay(instance.instanceId)}
        >
          <SpriteIcon icon="play" />
          <span className="ml-1">Play</span>
        </Button>

        <Button
          variant="secondary"
          className="w-full"
          disabled={actionDisabled}
          onClick={() => onSettings(instance.instanceId)}
        >
          <span className="text-xs">⚙</span>
          <span className="ml-1">Settings</span>
        </Button>
      </div>

      <div className="grid grid-cols-4 gap-2">
        <Button
          variant="ghost"
          className="w-full text-xs"
          disabled={actionDisabled}
          onClick={() => onRestore(instance.instanceId)}
        >
          Restore
        </Button>

        <Button
          variant="ghost"
          className="w-full text-xs"
          disabled={actionDisabled}
          onClick={() => onReconnect(instance.instanceId)}
        >
          Reconnect
        </Button>

        <Button
          variant="ghost"
          className="w-full text-xs"
          disabled={actionDisabled || !isRunning}
          onClick={() => onPause(instance.instanceId)}
        >
          Pause
        </Button>

        <Button
          variant="ghost"
          className={`w-full text-xs ${showDestroyConfirm ? "text-red-400 border-red-500/50" : ""}`}
          disabled={actionDisabled}
          onClick={handleDestroy}
        >
          {showDestroyConfirm ? "Confirm Destroy" : "Destroy"}
        </Button>
      </div>

      {showDestroyConfirm && (
        <div className="text-xs text-red-300 bg-red-900/20 p-2 rounded border border-red-500/30">
          This will permanently destroy instance {instance.instanceId}. A backup will run first if configured.
          <div className="mt-1 flex gap-2">
            <button
              className="text-red-400 underline"
              onClick={handleDestroy}
            >
              Yes, destroy
            </button>
            <button
              className="text-gray-400 underline"
              onClick={() => setShowDestroyConfirm(false)}
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
