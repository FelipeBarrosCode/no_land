import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import { useLocalization, translate } from "../../lib/i18n";
import type {
  LaunchLibraryResponse,
  LaunchSoftwareJob,
  SoftwareArtworkResult,
} from "../../lib/types";
import {
  isTerminalLaunchJob,
  LaunchPcCard,
  SoftwareLaunchCard,
} from "./LaunchLibraryCard";

interface Props {
  instanceId: number;
  instanceLabel: string;
  library: LaunchLibraryResponse | null;
  loading: boolean;
  job: LaunchSoftwareJob | null;
  launchingAppId: string | null;
  artwork: Record<string, SoftwareArtworkResult>;
  artworkLoading: Record<string, boolean>;
  onLoadLibrary: (instanceId: number) => Promise<LaunchLibraryResponse | null>;
  onLaunchPc: (instanceId: number) => Promise<void>;
  onLaunchSoftware: (
    instanceId: number,
    appId: string,
  ) => Promise<LaunchSoftwareJob | null>;
  onPollJob: (jobId: string) => Promise<LaunchSoftwareJob | null>;
  onLoadArtwork: (name: string) => Promise<SoftwareArtworkResult | null>;
  onClose: () => void;
}

export function LaunchLibraryModal({
  instanceId,
  instanceLabel,
  library,
  loading,
  job,
  launchingAppId,
  artwork,
  artworkLoading,
  onLoadLibrary,
  onLaunchPc,
  onLaunchSoftware,
  onPollJob,
  onLoadArtwork,
  onClose,
}: Props) {
  const { t } = useLocalization();
  const [loadRequested, setLoadRequested] = useState(false);
  const [search, setSearch] = useState("");
  const navigate = useNavigate();

  useEffect(() => {
    setLoadRequested(true);
    void onLoadLibrary(instanceId);
  }, [instanceId, onLoadLibrary]);

  useEffect(() => {
    if (!job || isTerminalLaunchJob(job)) {
      return;
    }

    let polling = false;
    const intervalId = window.setInterval(() => {
      if (polling) {
        return;
      }
      polling = true;
      void onPollJob(job.jobId).finally(() => {
        polling = false;
      });
    }, 1500);

    return () => window.clearInterval(intervalId);
  }, [job?.jobId, job?.status, job?.finishedAt, onPollJob]);

  const launchInProgress =
    launchingAppId !== null || (job !== null && !isTerminalLaunchJob(job));
  const filteredItems = useMemo(() => {
    const term = search.trim().toLowerCase();
    if (!term) {
      return library?.items ?? [];
    }
    return (library?.items ?? []).filter((item) =>
      [item.displayName, item.appId, ...item.aliases].some((value) =>
        value.toLowerCase().includes(term),
      ),
    );
  }, [library?.items, search]);

  async function launchPc() {
    onClose();
    await onLaunchPc(instanceId);
  }

  function openIgdbSettings() {
    onClose();
    navigate("/settings");
  }

  return (
    <ModalFrame
      labelledBy="launch-library-title"
      panelClassName="pixel-frame max-w-5xl bg-[#090d20] text-white"
      overlayClassName="bg-[#02040be8] backdrop-blur-sm"
      zIndexClassName="z-[60]"
    >
      <div className="flex items-start justify-between gap-4 border-b border-[#283252] p-5">
        <div>
          <p className="font-display text-[10px] uppercase tracking-[0.16em] text-neon-cyan">
            {translate("generated.5d43e1bb65ed54c8")} {instanceId}
          </p>
          <h2 id="launch-library-title" className="mt-1 font-display text-xl text-white">
            {translate("generated.558aab1ea62591ac")}
          </h2>
          <p className="mt-2 text-sm text-[#91a9c4]">
            {instanceLabel} {translate("generated.44dbfc9d6214e1f8")}
          </p>
        </div>
        <Button variant="ghost" onClick={onClose}>
          {translate("generated.7d9eb7acb13e2462")}
        </Button>
      </div>

      <ModalBody className="p-5">
        <div className="mb-4 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <LaunchPcCard
            available={library?.launchPcAvailable ?? true}
            disabled={launchInProgress}
            onLaunch={() => void launchPc()}
          />
        </div>

        <div className="mb-4 flex flex-wrap items-center justify-between gap-3 border-y border-[#283252] py-3">
          <input
            type="search"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder={t("library.search")}
            aria-label={t("library.search")}
            className="min-w-0 flex-1 border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-sm text-[#dff8ff] outline-none placeholder:text-[#5e7396] focus:border-neon-cyan"
          />
          <div className="flex items-center gap-2">
            <p className="text-xs text-[#7890ae]">{t("library.artwork")}</p>
            <Button variant="ghost" className="px-3 py-1 text-[11px]" onClick={openIgdbSettings}>
              {translate("generated.306a2cd87b6e0cdf")}
            </Button>
          </div>
        </div>

        {loading || !loadRequested ? (
          <div className="flex min-h-40 flex-col items-center justify-center gap-4 text-[#a8bed6]">
            <span className="h-8 w-8 animate-spin rounded-full border-2 border-neon-cyan border-t-transparent" />
            <p className="font-display text-xs uppercase tracking-[0.12em]">
              {translate("generated.265bcb09adbb4128")}
            </p>
          </div>
        ) : library ? (
          <>
            <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
              {filteredItems.map((item) => {
                const artworkName = item.artworkKey.trim() || item.displayName;
                const itemJob = job?.appId === item.appId ? job : null;
                return (
                  <SoftwareLaunchCard
                    key={item.appId}
                    item={item}
                    artwork={artwork[artworkName]}
                    artworkLoading={Boolean(artworkLoading[artworkName])}
                    job={itemJob}
                    launching={launchingAppId === item.appId}
                    disabled={launchInProgress && !itemJob && launchingAppId !== item.appId}
                    onLoadArtwork={onLoadArtwork}
                    onLaunch={() => void onLaunchSoftware(instanceId, item.appId)}
                  />
                );
              })}
            </div>

            {library.items.length === 0 ? (
              <div className="border border-[#283252] bg-[#0d132b] p-4 text-sm text-[#a8bed6]">
                {translate("generated.9592be79a0699345")}
              </div>
            ) : filteredItems.length === 0 ? (
              <div className="border border-[#283252] bg-[#0d132b] p-4 text-sm text-[#a8bed6]">
                {translate("generated.318dc6997872935f")}{search.trim()}”.
              </div>
            ) : null}
          </>
        ) : (
          <div className="flex min-h-40 flex-col items-center justify-center gap-4 text-center">
            <p className="text-[#a8bed6]">{t("library.load.failed")}</p>
            <Button variant="secondary" onClick={() => void onLoadLibrary(instanceId)}>
              {translate("generated.df0fe9e00d354b01")}
            </Button>
          </div>
        )}
      </ModalBody>
    </ModalFrame>
  );
}
