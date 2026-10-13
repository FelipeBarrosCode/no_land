import type { SpendAlert } from "./types";

interface Handlers {
  loadPending: () => Promise<SpendAlert[]>;
  notify: (alert: SpendAlert) => Promise<boolean>;
  acknowledge: (id: string) => Promise<void>;
  onStopped: () => void;
  disposed: () => boolean;
  reportError: (error: unknown) => void;
}

/** Events can be emitted by webviews: trust only the native pending-alert API. */
export function createSpendAlertQueue(handlers: Handlers) {
  const seen = new Set<string>();
  let deliveries = Promise.resolve();
  let replaying = false;

  const enqueue = (alert: SpendAlert) => {
    if (handlers.disposed() || seen.has(alert.id)) return;
    seen.add(alert.id);
    deliveries = deliveries.then(async () => {
      if (handlers.disposed()) return;
      if (alert.kind === "auto_stopped") handlers.onStopped();
      let handled = false;
      try {
        handled = await handlers.notify(alert);
      } catch (error) {
        handlers.reportError(error);
      }
      if (!handled) {
        seen.delete(alert.id);
        return;
      }
      try {
        await handlers.acknowledge(alert.id);
      } catch (error) {
        // Keep the in-process deduplication; a restart can retry acknowledgment.
        handlers.reportError(error);
      }
    }).catch(handlers.reportError);
  };

  return {
    async wake() {
      if (handlers.disposed() || replaying) return;
      replaying = true;
      try {
        for (const alert of await handlers.loadPending()) enqueue(alert);
      } catch (error) {
        handlers.reportError(error);
      } finally {
        replaying = false;
      }
    },
    drain: () => deliveries,
  };
}
