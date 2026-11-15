import { Button } from "../../components/ui/Button";
import { translate } from "../../lib/i18n";
import { storageBrowseErrorKey, type StorageBrowseView } from "../../lib/storageBrowse";

interface Props extends StorageBrowseView {
  slow: boolean;
  onRefresh: () => void;
  preparingHelper: boolean;
  onPrepareHelper: () => void;
}

export function StorageBrowseStatus({ snapshot, refreshing, error, slow, onRefresh, preparingHelper, onPrepareHelper }: Props) {
  const time = snapshot?.observedAt && new Date(snapshot.observedAt);
  return (
    <div role="status" aria-live="polite" className="mb-3 space-y-1 text-[1.05rem] text-[#b4c8de]">
      <div className="flex items-center justify-between gap-3">
        <span>
          {refreshing ? translate("storage.browse.refreshing")
            : snapshot?.freshness === "fresh" && !error
              ? translate("storage.browse.updated") : translate("storage.browse.unavailable")}
          {time && Number.isFinite(time.getTime()) && ` · ${time.toLocaleTimeString()}`}
        </span>
        <Button variant="ghost" disabled={refreshing || preparingHelper} onClick={onRefresh}>
          {translate("storage.browse.refresh")}
        </Button>
      </div>
      {snapshot?.freshness === "cached" && <p>{translate("storage.browse.cached")}</p>}
      {slow && <p>{translate("storage.browse.slow")}</p>}
      {error && <p className="text-red-300">{translate(storageBrowseErrorKey(error))}</p>}
      {(error === "helper_missing" || error === "helper_install_failed") && (
        <Button variant="ghost" disabled={preparingHelper || refreshing}
          loading={preparingHelper} loadingText={translate("storage.browse.preparingHelper")}
          onClick={onPrepareHelper}>
          {translate("storage.browse.prepareHelper")}
        </Button>
      )}
    </div>
  );
}
