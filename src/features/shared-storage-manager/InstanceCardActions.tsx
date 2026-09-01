import { useState } from "react";
import type { BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { SpriteIcon } from "../../components/ui/SpriteIcon";
import type { RentedInstanceSummary } from "../../lib/types";

interface Props {
  instance: RentedInstanceSummary;
  busy: boolean;
  instanceActionRunning: boolean;
  blockingAction: BlockingActionState | null;
  onProvisioning: (instanceId: number) => void;
  onOpenLaunchLibrary: (instanceId: number) => void;
  onDisplay: (instanceId: number) => void;
  onReboot: (instanceId: number) => void;
  onDestroy: (instanceId: number) => Promise<void>;
  onSaveStorage: (instanceId: number) => void;
  onSyncStorage: (instanceId: number) => void;
}

export function InstanceCardActions({
  instance,
  busy,
  instanceActionRunning,
  blockingAction,
  onProvisioning,
  onOpenLaunchLibrary,
  onDisplay,
  onReboot,
  onDestroy,
  onSaveStorage,
  onSyncStorage,
}: Props) {
  const [showDestroyConfirm, setShowDestroyConfirm] = useState(false);
  const isRunning = instance.status.toLowerCase().includes("run");
  const actionDisabled = busy || instanceActionRunning;
  const loadingKey = blockingAction?.key ?? null;

  const handleDestroy = async () => {
    if (!showDestroyConfirm) {
      setShowDestroyConfirm(true);
      return;
    }

    await onDestroy(instance.instanceId);
    setShowDestroyConfirm(false);
  };

  return (
    <div className="space-y-2">
      <div className="grid grid-cols-2 gap-2">
        <Button
          className="w-full"
          disabled={actionDisabled}
          loading={loadingKey === "provisioning.flow"}
          loadingText="Launching..."
          onClick={() => onProvisioning(instance.instanceId)}
        >
          <SpriteIcon icon="play" />
          <span className="ml-1">Provisioning</span>
        </Button>

        <Button
          variant="secondary"
          className="w-full"
          disabled={actionDisabled}
          onClick={() => onOpenLaunchLibrary(instance.instanceId)}
        >
          <SpriteIcon icon="play" />
          <span className="ml-1">Play</span>
        </Button>
      </div>

      <div className="grid grid-cols-2 gap-2">
        <Button
          variant="ghost"
          className="w-full text-[14px]"
          disabled={actionDisabled || !isRunning}
          loading={loadingKey === "instance.storage.export"}
          loadingText="Saving files..."
          onClick={() => onSaveStorage(instance.instanceId)}
        >
          Save
        </Button>

        <Button
          variant="ghost"
          className="w-full text-[14px]"
          disabled={actionDisabled || !isRunning}
          loading={loadingKey === "instance.storage.sync"}
          loadingText="Syncing files..."
          onClick={() => onSyncStorage(instance.instanceId)}
        >
          Sync Files
        </Button>
      </div>

      <div className="grid grid-cols-3 gap-2">
        <Button
          variant="ghost"
          className="w-full text-[14px]"
          disabled={actionDisabled || !isRunning}
          onClick={() => onDisplay(instance.instanceId)}
        >
          Display
        </Button>

        <Button
          variant="ghost"
          className="w-full text-[14px]"
          disabled={actionDisabled}
          loading={loadingKey === "instance.services.reboot"}
          loadingText="Rebooting..."
          onClick={() => onReboot(instance.instanceId)}
        >
          Reboot
        </Button>

        <Button
          variant="ghost"
          className={`w-full text-[14px] ${showDestroyConfirm ? "text-red-400 border-red-500/50" : ""}`}
          disabled={actionDisabled}
          loading={loadingKey === "instance.destroy"}
          loadingText="Destroying..."
          onClick={handleDestroy}
        >
          {showDestroyConfirm ? (
            "Confirm Destroy"
          ) : (
            <>
              <SpriteIcon icon="destroy" />
              <span className="ml-1">Destroy</span>
            </>
          )}
        </Button>
      </div>

      {showDestroyConfirm && (
        <div className="text-xs text-red-300 bg-red-900/20 p-2 rounded border border-red-500/30">
          This will permanently destroy instance {instance.instanceId}. A backup
          will run first if configured.
          <div className="mt-1 flex gap-2">
            <button className="text-red-400 underline" onClick={handleDestroy}>
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
