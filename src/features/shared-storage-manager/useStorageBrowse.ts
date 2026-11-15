import { useCallback, useEffect, useRef, useState } from "react";
import { getInstanceStorageBrowseSnapshot, prepareInstanceStorageHelper } from "../../lib/backend";
import { createStorageHelperPreparation, startStorageBrowse, type StorageBrowseView, type StorageHelperPreparationView } from "../../lib/storageBrowse";
import type { SharedStorageBrowseTarget } from "../../lib/types";
import { useAppStore } from "../../store/appStore";

const EMPTY: StorageBrowseView = { snapshot: null, refreshing: false, error: null };
const HELPER_EMPTY: StorageHelperPreparationView = { preparing: false, error: null };

export function useStorageBrowse(open: boolean, instanceId: number | null, target: SharedStorageBrowseTarget) {
  const profiles = useAppStore(state => state.sharedStorageProfiles);
  const profileVersion = JSON.stringify(profiles);
  const query = `${instanceId}:${target}:${profileVersion}`;
  const identity = useRef({ open, query });
  if (identity.current.open !== open || identity.current.query !== query) {
    identity.current = { open, query };
  }
  const session = identity.current;
  const [view, setView] = useState<{ query: string; value: StorageBrowseView }>({ query, value: EMPTY });
  const [helperView, setHelperView] = useState({ session, value: HELPER_EMPTY });
  const helper = useRef<ReturnType<typeof createStorageHelperPreparation> | null>(null);
  const [revision, setRevision] = useState(0);
  const forceNextRefresh = useRef(false);
  const [slow, setSlow] = useState(false);
  const reload = useCallback(() => {
    forceNextRefresh.current = true;
    setRevision(value => value + 1);
  }, []);
  const prepareHelper = useCallback(() => { void helper.current?.run(); }, []);

  useEffect(() => {
    setSlow(false);
    setHelperView({ session, value: HELPER_EMPTY });
    if (!open || instanceId === null) {
      forceNextRefresh.current = false;
      setView({ query, value: EMPTY });
      return;
    }
    const timeout = window.setTimeout(() => setSlow(true), 5000);
    const preparation = createStorageHelperPreparation(
      () => prepareInstanceStorageHelper(instanceId),
      value => setHelperView({ session, value }),
      reload,
      () => identity.current === session && session.open,
    );
    helper.current = preparation;
    const forceRefresh = forceNextRefresh.current;
    forceNextRefresh.current = false;
    const request = startStorageBrowse(
      refresh => getInstanceStorageBrowseSnapshot(instanceId, target, refresh),
      value => {
        if (!value.refreshing) window.clearTimeout(timeout);
        setView({ query, value });
      },
      forceRefresh,
    );
    return () => {
      request.cancel();
      preparation.cancel();
      if (helper.current === preparation) helper.current = null;
      window.clearTimeout(timeout);
    };
  }, [open, instanceId, target, query, revision, reload, session]);

  const current = view.query === query && open ? view.value : EMPTY;
  const currentHelper = helperView.session === session && open ? helperView.value : HELPER_EMPTY;
  return {
    ...current,
    error: currentHelper.error ?? current.error,
    preparingHelper: currentHelper.preparing,
    entries: current.snapshot?.entries ?? [],
    slow: current.refreshing && slow,
    reload,
    prepareHelper,
  };
}
