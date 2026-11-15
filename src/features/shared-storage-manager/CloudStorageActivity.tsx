import { BlockingLoaderOverlay } from "../../components/ui/BlockingLoaderOverlay";
import { Button } from "../../components/ui/Button";
import { translate } from "../../lib/i18n";
import { useAppStore } from "../../store/appStore";

/** Storage activity has its own lifetime, independent of stream/provisioning UI. */
export function CloudStorageActivity() {
  const action = useAppStore(state => state.backgroundStorageAction);
  const cancel = useAppStore(state => state.cancelSharedStorageOperation);
  const clear = useAppStore(state => state.clearBackgroundStorageAction);
  if (!action) return null;
  return (
    <aside aria-label={translate("settings.storage")}
      className="pointer-events-none fixed bottom-4 right-4 z-[120] max-h-[70dvh] w-[min(24rem,calc(100vw-2rem))] overflow-y-auto">
      {action.stage === "FAILED" ? (
        <div role="alert" className="pointer-events-auto glass-panel pixel-frame space-y-3 p-4">
          <h2 className="font-display text-red-300">{translate("storage.delete.failed")}</h2>
          <p className="break-words text-[#c6dbf4]">{action.detail}</p>
          {action.operationId && <Button variant="ghost" onClick={() => clear(action.operationId!)}>
            {translate("generated.7d9eb7acb13e2462")}
          </Button>}
        </div>
      ) : (
        <BlockingLoaderOverlay action={action} inline className="pointer-events-auto p-4"
          onCancel={action.instanceId != null && action.key !== "instance.storage.delete"
            ? () => void cancel(action.instanceId!) : undefined} />
      )}
    </aside>
  );
}
