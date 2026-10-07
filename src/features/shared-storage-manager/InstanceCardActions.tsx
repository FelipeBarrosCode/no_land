import { useState } from "react";
import type { BlockingActionState } from "../../components/ui/BlockingLoaderOverlay";
import { useLocalization, translate } from "../../lib/i18n";
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
  const { t } = useLocalization();
  const [showDestroyConfirm, setShowDestroyConfirm] = useState(false);
  const isRunning = instance.status.toLowerCase().includes("run");
  const actionDisabled = busy || instanceActionRunning;
  const loadingKey = blockingAction?.key ?? null;
  const transferRunning =
    loadingKey === "instance.storage.export" ||
    loadingKey === "instance.storage.sync" ||
    loadingKey === "instance.files.upload";

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
          loadingText={t("instance.launching")}
          onClick={() => onProvisioning(instance.instanceId)}
        >
          <SpriteIcon icon="play" />
          <span className="ml-1">{t("instance.provisioning")}</span>
        </Button>

        <Button
          variant="secondary"
          className="w-full"
          disabled={actionDisabled}
          onClick={() => onOpenLaunchLibrary(instance.instanceId)}
        >
          <SpriteIcon icon="play" />
          <span className="ml-1">{t("common.play")}</span>
        </Button>
      </div>

      <div className="grid grid-cols-2 gap-2">
        <Button
          variant="ghost"
          className="w-full text-[14px]"
          disabled={actionDisabled || transferRunning || !isRunning}
          loading={loadingKey === "instance.storage.export"}
          loadingText={translate("generated.0a6e1f59cb714422")}
          onClick={() => onSaveStorage(instance.instanceId)}
        >
          {translate("generated.1509f561f2416598")}
        </Button>

        <Button
          variant="ghost"
          className="w-full text-[14px]"
          disabled={actionDisabled || transferRunning || !isRunning}
          loading={loadingKey === "instance.storage.sync"}
          loadingText={translate("generated.61263112babd588c")}
          onClick={() => onSyncStorage(instance.instanceId)}
        >
          {translate("generated.8156b7d5123fb069")}
        </Button>
      </div>

      <div className="grid grid-cols-3 gap-2">
        <Button
          variant="ghost"
          className="w-full text-[14px]"
          disabled={actionDisabled || !isRunning}
          onClick={() => onDisplay(instance.instanceId)}
        >
          {translate("generated.34e108c0896d0158")}
        </Button>

        <Button
          variant="ghost"
          className="w-full text-[14px]"
          disabled={actionDisabled}
          loading={loadingKey === "instance.services.reboot"}
          loadingText={translate("generated.6a5b9e2c84b8ffe6")}
          onClick={() => onReboot(instance.instanceId)}
        >
          {translate("generated.fba023ca78ebb022")}
        </Button>

        <Button
          variant="ghost"
          className={`w-full text-[14px] ${showDestroyConfirm ? "text-red-400 border-red-500/50" : ""}`}
          disabled={actionDisabled}
          loading={loadingKey === "instance.destroy"}
          loadingText={translate("generated.81718696be2eed63")}
          onClick={handleDestroy}
        >
          {showDestroyConfirm ? (
            "Confirm Destroy"
          ) : (
            <>
              <SpriteIcon icon="destroy" />
              <span className="ml-1">{t("instance.destroy")}</span>
            </>
          )}
        </Button>
      </div>

      {showDestroyConfirm && (
        <div className="text-xs text-red-300 bg-red-900/20 p-2 rounded border border-red-500/30">
          {translate("generated.933882d245653198")} {instance.instanceId}{translate("generated.4567726876726721")}
          <div className="mt-1 flex gap-2">
            <button className="text-red-400 underline" onClick={handleDestroy}>
              {translate("generated.3c98f82698fe06a8")}
            </button>
            <button
              className="text-gray-400 underline"
              onClick={() => setShowDestroyConfirm(false)}
            >
              {translate("generated.19766ed6ccb2f4a3")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
