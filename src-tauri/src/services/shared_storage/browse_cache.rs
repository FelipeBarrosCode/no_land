//! Private, bounded, display-only browse snapshots and cancellation-safe
//! singleflight. No sessions, credentials, repository keys or loader futures
//! are retained by the cache.

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use hmac::{Hmac, Mac};
use rand_core::RngCore;
use sha2::Sha256;
use tokio::{sync::watch, task::AbortHandle};

use crate::models::app_state::{
    SharedStorageBrowseFreshness, SharedStorageBrowseSnapshot, SharedStorageObjectEntry,
};

pub(super) const SELECTION_CHANGED: &str =
    "Storage account, profile or repository changed. Reload the options and try again.";
const CANCELLED: &str = "Storage option refresh was cancelled. Retry to refresh the options.";
const FRESH_FOR: Duration = Duration::from_secs(30);
const MAX_ENTRIES: usize = 16;
const MAX_ROWS: usize = 10_000;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;
// Count-bounded map/flight bookkeeping has no row payload. Leave headroom for
// its allocation metadata instead of spending the entire budget on row buffers.
const CACHE_BOOKKEEPING_RESERVE: usize = 16 * 1024;
const OVERSIZED_METADATA: &str =
    "Storage option metadata exceeds the safe browse limit (10,000 rows, 8 MiB per snapshot, 16 MiB cache). Last-known options were not replaced. Reduce the option metadata or wait for other refreshes to finish and retry.";

type Outcome = Result<SharedStorageBrowseSnapshot, String>;
type SharedOutcome = Result<Arc<CachedValue>, String>;

#[cfg_attr(test, derive(Clone))]
struct CachedValue {
    entries: Vec<SharedStorageObjectEntry>,
    observed_at: String,
    observed: Instant,
    charged_bytes: usize,
}

struct Flight {
    id: String,
    outcome: watch::Sender<Option<SharedOutcome>>,
    abort: AbortHandle,
    waiters: usize,
}

struct Entry {
    value: Option<Arc<CachedValue>>,
    touched: Instant,
    flight: Option<Flight>,
    refresh_failed: bool,
}

#[derive(Default)]
struct State {
    revision: u64,
    selection: Option<String>,
    entries: HashMap<String, Entry>,
    cached_bytes: usize,
}

pub(super) struct BrowseCache {
    salt: [u8; 32],
    state: Mutex<State>,
}

impl BrowseCache {
    pub fn new() -> Arc<Self> {
        let mut salt = [0; 32];
        rand_core::OsRng.fill_bytes(&mut salt);
        Arc::new(Self {
            salt,
            state: Mutex::new(State::default()),
        })
    }

    /// A per-context keyed digest is safe to return as an opaque UI binding.
    /// In particular, this is not a reusable hash of a password or account.
    pub fn binding(&self, value: &impl serde::Serialize) -> String {
        struct Writer(Hmac<Sha256>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Writer(Hmac::<Sha256>::new_from_slice(&self.salt).expect("HMAC key"));
        // Serialize directly into the digest, not a credential-bearing buffer.
        serde_json::to_writer(&mut writer, value).expect("serializable browse binding");
        hex::encode(writer.0.finalize().into_bytes())
    }

    pub fn observe_selection(&self, selection: String) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.selection.as_ref() != Some(&selection) {
            invalidate(&mut state);
            state.selection = Some(selection);
        }
        state.revision
    }

    pub fn invalidate(&self) {
        invalidate(&mut self.state.lock().unwrap_or_else(|error| error.into_inner()));
    }

    pub fn is_current(&self, revision: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .revision
            == revision
    }

    pub fn peek(&self, scope: &str) -> SharedStorageBrowseSnapshot {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let entry = state.entries.get_mut(scope);
        if let Some(entry) = entry {
            entry.touched = Instant::now();
            snapshot(scope, entry)
        } else {
            SharedStorageBrowseSnapshot {
                entries: Vec::new(),
                scope: scope.to_owned(),
                observed_at: None,
                freshness: SharedStorageBrowseFreshness::Unavailable,
            }
        }
    }

    pub async fn refresh<F, Fut>(
        self: &Arc<Self>,
        scope: String,
        revision: u64,
        loader: F,
    ) -> Outcome
    where
        F: FnOnce(String) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<SharedStorageObjectEntry>, String>> + Send + 'static,
    {
        let (id, mut receiver) = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.revision != revision {
                return Err(SELECTION_CHANGED.into());
            }
            if !state.entries.contains_key(&scope) {
                prune(&mut state);
                if state.entries.len() >= MAX_ENTRIES {
                    return Err("Too many storage option refreshes are running. Close unused selectors and retry.".into());
                }
            }
            let entry = state.entries.entry(scope.clone()).or_insert_with(|| Entry {
                value: None,
                touched: Instant::now(),
                flight: None,
                refresh_failed: false,
            });
            entry.touched = Instant::now();
            if let Some(flight) = entry.flight.as_mut() {
                flight.waiters += 1;
                tracing::info!(
                    event = "shared_storage_browse_request",
                    request_id = %flight.id,
                    stage = "coalesced",
                    waiters = flight.waiters,
                    "Joining an existing storage option refresh"
                );
                // `loader` (and any frozen credentials it owns) is dropped
                // before awaiting another caller's request.
                drop(loader);
                (flight.id.clone(), flight.outcome.subscribe())
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                let (sender, receiver) = watch::channel(None);
                let completion = Completion {
                    cache: Arc::downgrade(self),
                    scope: scope.clone(),
                    id: id.clone(),
                    sender: sender.clone(),
                    finished: false,
                };
                let request_id = id.clone();
                let task = tokio::spawn(async move {
                    let mut completion = completion;
                    let result = loader(request_id).await;
                    completion.finish(result);
                });
                entry.flight = Some(Flight {
                    id: id.clone(),
                    outcome: sender,
                    abort: task.abort_handle(),
                    waiters: 1,
                });
                (id, receiver)
            }
        };
        let _waiter = Waiter {
            cache: Arc::downgrade(self),
            scope,
            id,
        };
        loop {
            // Drop watch's read guard before returning/dropping our waiter,
            // which may need the cache lock while invalidation publishes.
            let result = { receiver.borrow().clone() };
            if let Some(result) = result {
                // The watch channel and cache share one charged allocation.
                // Only the returned DTO is cloned, with capacities bounded by
                // the already-admitted source snapshot.
                return result.map(|value| {
                    snapshot_value(&_waiter.scope, &value, SharedStorageBrowseFreshness::Fresh)
                });
            }
            receiver
                .changed()
                .await
                .map_err(|_| CANCELLED.to_string())?;
        }
    }
}

fn snapshot(scope: &str, entry: &Entry) -> SharedStorageBrowseSnapshot {
    let freshness = match entry.value.as_ref() {
        Some(value)
            if !entry.refresh_failed
                && entry.flight.is_none()
                && value.observed.elapsed() <= FRESH_FOR =>
        {
            SharedStorageBrowseFreshness::Fresh
        }
        Some(_) => SharedStorageBrowseFreshness::Cached,
        None => SharedStorageBrowseFreshness::Unavailable,
    };
    if let Some(value) = entry.value.as_ref() {
        snapshot_value(scope, value, freshness)
    } else {
        SharedStorageBrowseSnapshot {
            entries: Vec::new(),
            scope: scope.to_owned(),
            observed_at: None,
            freshness,
        }
    }
}

fn snapshot_value(
    scope: &str,
    value: &CachedValue,
    freshness: SharedStorageBrowseFreshness,
) -> SharedStorageBrowseSnapshot {
    SharedStorageBrowseSnapshot {
        entries: value.entries.clone(),
        scope: scope.to_owned(),
        observed_at: Some(value.observed_at.clone()),
        freshness,
    }
}

/// One pass over row headers, never their string contents. Charge backing Vec
/// capacity (including unused slots), all three String capacities, timestamp,
/// scope key and value/Arc headers. Arithmetic also works on 32-bit targets.
fn snapshot_charge(
    entries: &Vec<SharedStorageObjectEntry>,
    scope_bytes: usize,
    timestamp_bytes: usize,
) -> Option<usize> {
    if entries.len() > MAX_ROWS {
        return None;
    }
    checked_charge(
        entries.capacity(),
        scope_bytes,
        timestamp_bytes,
        entries.iter().flat_map(|entry| {
            [
                entry.path.capacity(),
                entry.name.capacity(),
                entry.parent_path.capacity(),
            ]
        }),
    )
    .filter(|bytes| *bytes <= MAX_SNAPSHOT_BYTES)
}

fn checked_charge(
    vector_capacity: usize,
    scope_bytes: usize,
    timestamp_bytes: usize,
    string_capacities: impl IntoIterator<Item = usize>,
) -> Option<usize> {
    let mut bytes = std::mem::size_of::<CachedValue>()
        .checked_add(2 * std::mem::size_of::<usize>())? // Arc reference counts
        .checked_add(std::mem::size_of::<String>())? // cached scope key
        .checked_add(vector_capacity.checked_mul(std::mem::size_of::<SharedStorageObjectEntry>())?)?
        .checked_add(scope_bytes)?
        .checked_add(timestamp_bytes)?;
    for capacity in string_capacities {
        bytes = bytes.checked_add(capacity)?;
    }
    Some(bytes)
}

fn within_cache_budget(bytes: usize) -> bool {
    bytes
        .checked_add(CACHE_BOOKKEEPING_RESERVE)
        .is_some_and(|total| total <= MAX_CACHE_BYTES)
}

fn remove_entry(state: &mut State, scope: &str) {
    if let Some(entry) = state.entries.remove(scope) {
        let bytes = entry.value.as_ref().map_or(0, |value| value.charged_bytes);
        state.cached_bytes = state
            .cached_bytes
            .checked_sub(bytes)
            .expect("cache byte accounting tracks every retained value");
    }
}

/// Plan eviction before changing anything: if protected/in-flight snapshots
/// leave insufficient room, reject without discarding any last-known rows.
/// The only scan is over at most MAX_ENTRIES metadata records, not their rows.
fn admit(
    state: &mut State,
    scope: &str,
    value: Arc<CachedValue>,
) -> Result<Arc<CachedValue>, String> {
    let old_bytes = state
        .entries
        .get(scope)
        .and_then(|entry| entry.value.as_ref())
        .map_or(0, |value| value.charged_bytes);
    let mut projected = state
        .cached_bytes
        .checked_sub(old_bytes)
        .and_then(|bytes| bytes.checked_add(value.charged_bytes))
        .ok_or_else(|| OVERSIZED_METADATA.to_owned())?;
    let mut victims = Vec::new();
    if !within_cache_budget(projected) {
        let mut idle = state
            .entries
            .iter()
            .filter(|(key, entry)| {
                key.as_str() != scope && entry.flight.is_none() && entry.value.is_some()
            })
            .map(|(key, entry)| {
                (
                    key.clone(),
                    entry.touched,
                    entry.value.as_ref().expect("filtered value").charged_bytes,
                )
            })
            .collect::<Vec<_>>();
        idle.sort_by_key(|(_, touched, _)| *touched);
        for (key, _, charged_bytes) in idle {
            projected = projected
                .checked_sub(charged_bytes)
                .ok_or_else(|| OVERSIZED_METADATA.to_owned())?;
            victims.push(key);
            if within_cache_budget(projected) {
                break;
            }
        }
    }
    if !within_cache_budget(projected) {
        return Err(OVERSIZED_METADATA.into());
    }
    for victim in victims {
        remove_entry(state, &victim);
    }
    let entry = state
        .entries
        .get_mut(scope)
        .expect("current refresh is protected");
    entry.value = Some(value.clone());
    state.cached_bytes = projected;
    Ok(value)
}

fn invalidate(state: &mut State) {
    state.revision = state.revision.wrapping_add(1);
    for entry in state.entries.values_mut() {
        if let Some(flight) = entry.flight.take() {
            flight
                .outcome
                .send_replace(Some(Err(SELECTION_CHANGED.into())));
            flight.abort.abort();
        }
    }
    state.entries.clear();
    state.cached_bytes = 0;
}

fn prune(state: &mut State) {
    while state.entries.len() >= MAX_ENTRIES {
        let oldest = state
            .entries
            .iter()
            .filter(|(_, entry)| entry.flight.is_none())
            .min_by_key(|(_, entry)| entry.touched)
            .map(|(scope, _)| scope.clone());
        let Some(oldest) = oldest else { break };
        remove_entry(state, &oldest);
    }
}

struct Completion {
    cache: Weak<BrowseCache>,
    scope: String,
    id: String,
    sender: watch::Sender<Option<SharedOutcome>>,
    finished: bool,
}

impl Completion {
    fn finish(&mut self, result: Result<Vec<SharedStorageObjectEntry>, String>) {
        // Reject before cloning, retaining or taking the cache lock. The charge
        // is stored with the immutable value so admission/eviction is O(scopes),
        // not O(all retained rows), and spare buffer capacity cannot hide bytes.
        let result = result.and_then(|entries| {
            let observed_at = chrono::Utc::now().to_rfc3339();
            let charged_bytes = snapshot_charge(&entries, self.scope.len(), observed_at.capacity())
                .ok_or_else(|| OVERSIZED_METADATA.to_owned())?;
            Ok(Arc::new(CachedValue {
                entries,
                observed_at,
                observed: Instant::now(),
                charged_bytes,
            }))
        });
        let result = if let Some(cache) = self.cache.upgrade() {
            let mut state = cache
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.entries.get(&self.scope).is_some_and(|entry| {
                entry
                    .flight
                    .as_ref()
                    .is_some_and(|flight| flight.id == self.id)
            }) {
                let result = result.and_then(|value| admit(&mut state, &self.scope, value));
                let entry = state
                    .entries
                    .get_mut(&self.scope)
                    .expect("matched protected refresh");
                entry.flight = None;
                entry.refresh_failed = result.is_err();
                result
            } else {
                Err(SELECTION_CHANGED.into())
            }
        } else {
            Err(CANCELLED.into())
        };
        self.finished = true;
        self.sender.send_replace(Some(result));
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        // Includes panic, task abort and runtime shutdown. A cancelled owner
        // must never leave subsequent option loads waiting on a stuck flight.
        if !self.finished {
            self.finish(Err(CANCELLED.into()));
        }
    }
}

struct Waiter {
    cache: Weak<BrowseCache>,
    scope: String,
    id: String,
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let Some(cache) = self.cache.upgrade() else {
            return;
        };
        let mut state = cache
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = state.entries.get_mut(&self.scope) {
            if let Some(flight) = entry.flight.as_mut().filter(|flight| flight.id == self.id) {
                flight.waiters = flight.waiters.saturating_sub(1);
                if flight.waiters == 0 {
                    let flight = entry.flight.take().expect("matched flight");
                    flight.outcome.send_replace(Some(Err(CANCELLED.into())));
                    flight.abort.abort();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    fn rows(name: &str) -> Vec<SharedStorageObjectEntry> {
        vec![SharedStorageObjectEntry {
            path: format!("/apps/{name}"),
            name: name.into(),
            parent_path: "/apps".into(),
            is_dir: true,
        }]
    }

    fn refreshing(cache: &BrowseCache, scope: &str) -> bool {
        cache
            .state
            .lock()
            .unwrap()
            .entries
            .get(scope)
            .is_some_and(|entry| entry.flight.is_some())
    }

    #[tokio::test]
    async fn desktop_browse_identical_refreshes_coalesce_but_different_scopes_do_not() {
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        let started = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());
        let mut requests = Vec::new();
        for scope in ["a", "a", "b", "a"] {
            let cache = cache.clone();
            let started = started.clone();
            let release = release.clone();
            requests.push(tokio::spawn(async move {
                cache
                    .refresh(scope.into(), revision, move |_| async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Ok(rows(scope))
                    })
                    .await
            }));
        }
        while started.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
        // All caller futures are polled before releasing the two loaders.
        tokio::task::yield_now().await;
        assert!(refreshing(&cache, "a"));
        assert_eq!(started.load(Ordering::SeqCst), 2);
        release.notify_waiters();
        for request in requests {
            assert_eq!(
                request.await.unwrap().unwrap().freshness,
                SharedStorageBrowseFreshness::Fresh
            );
        }
    }

    #[tokio::test]
    async fn desktop_browse_inflight_refresh_marks_even_recent_rows_cached() {
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        cache
            .refresh("scope".into(), revision, |_| async { Ok(rows("saved")) })
            .await
            .unwrap();
        assert_eq!(
            cache.peek("scope").freshness,
            SharedStorageBrowseFreshness::Fresh
        );
        let (started, ready) = tokio::sync::oneshot::channel();
        let release = Arc::new(Notify::new());
        let request = {
            let cache = cache.clone();
            let release = release.clone();
            tokio::spawn(async move {
                cache
                    .refresh("scope".into(), revision, |_| async move {
                        let _ = started.send(());
                        release.notified().await;
                        Ok(rows("current"))
                    })
                    .await
            })
        };
        ready.await.unwrap();
        let cached = cache.peek("scope");
        assert_eq!(cached.freshness, SharedStorageBrowseFreshness::Cached);
        assert_eq!(cached.entries[0].name, "saved");
        release.notify_one();
        assert_eq!(
            request.await.unwrap().unwrap().freshness,
            SharedStorageBrowseFreshness::Fresh
        );
    }

    #[tokio::test]
    async fn desktop_browse_failed_refresh_keeps_last_known_rows_and_timestamp() {
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        let first = cache
            .refresh("scope".into(), revision, |_| async {
                Ok(rows("last-known"))
            })
            .await
            .unwrap();
        assert!(cache
            .refresh("scope".into(), revision, |_| async {
                Err("Offline; retry when this PC is available.".into())
            })
            .await
            .is_err());
        let cached = cache.peek("scope");
        assert_eq!(cached.entries, first.entries);
        assert_eq!(cached.observed_at, first.observed_at);
        assert_eq!(cached.freshness, SharedStorageBrowseFreshness::Cached);
        assert!(!refreshing(&cache, "scope"));
        Arc::make_mut(
            cache
                .state
                .lock()
                .unwrap()
                .entries
                .get_mut("scope")
                .unwrap()
                .value
                .as_mut()
                .unwrap(),
        )
        .observed = Instant::now() - FRESH_FOR - Duration::from_secs(1);
        assert_eq!(
            cache.peek("scope").freshness,
            SharedStorageBrowseFreshness::Cached
        );
    }

    #[tokio::test]
    async fn desktop_browse_invalidation_cancels_flight_and_cannot_republish_old_repository() {
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("old-account".into());
        let entered = Arc::new(Notify::new());
        let request = {
            let cache = cache.clone();
            let entered = entered.clone();
            tokio::spawn(async move {
                cache
                    .refresh("old".into(), revision, move |_| async move {
                        entered.notify_one();
                        std::future::pending::<()>().await;
                        Ok(rows("old"))
                    })
                    .await
            })
        };
        entered.notified().await;
        cache.observe_selection("new-account".into());
        assert_eq!(request.await.unwrap().unwrap_err(), SELECTION_CHANGED);
        assert!(!cache.is_current(revision));
        assert_eq!(
            cache.peek("old").freshness,
            SharedStorageBrowseFreshness::Unavailable
        );
        assert!(cache
            .refresh("old".into(), revision, |_| async { Ok(rows("old")) })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn desktop_browse_last_cancelled_waiter_aborts_loader_and_releases_flight() {
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        let entered = Arc::new(Notify::new());
        let dropped = Arc::new(Notify::new());
        struct Dropped(Arc<Notify>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }
        let request = {
            let cache = cache.clone();
            let entered = entered.clone();
            let dropped = dropped.clone();
            tokio::spawn(async move {
                cache
                    .refresh("scope".into(), revision, move |_| async move {
                        let _guard = Dropped(dropped);
                        entered.notify_one();
                        std::future::pending::<()>().await;
                        Ok(rows("never"))
                    })
                    .await
            })
        };
        entered.notified().await;
        request.abort();
        let _ = request.await;
        tokio::time::timeout(Duration::from_secs(2), dropped.notified())
            .await
            .unwrap();
        assert!(!refreshing(&cache, "scope"));
        assert_eq!(
            cache
                .refresh("scope".into(), revision, |_| async { Ok(rows("retry")) })
                .await
                .unwrap()
                .entries,
            rows("retry")
        );
    }

    #[tokio::test]
    async fn desktop_browse_cancelling_one_coalesced_waiter_keeps_the_shared_read_alive() {
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut first = Box::pin(cache.refresh("scope".into(), revision, {
            let entered = entered.clone();
            let release = release.clone();
            move |_| async move {
                entered.notify_one();
                release.notified().await;
                Ok(rows("shared"))
            }
        }));
        assert!(futures_util::poll!(first.as_mut()).is_pending());
        entered.notified().await;
        let mut second = Box::pin(cache.refresh("scope".into(), revision, |_| async {
            panic!("a coalesced waiter must never start a second loader");
        }));
        assert!(futures_util::poll!(second.as_mut()).is_pending());
        drop(first);
        assert!(refreshing(&cache, "scope"));
        release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.entries, rows("shared"));
        assert!(!refreshing(&cache, "scope"));
    }

    #[test]
    fn desktop_browse_bindings_are_context_private_and_cache_is_bounded() {
        let first = BrowseCache::new();
        let second = BrowseCache::new();
        assert_ne!(first.binding(&"password"), second.binding(&"password"));
        assert_eq!(first.binding(&"password"), first.binding(&"password"));
        let mut state = first.state.lock().unwrap();
        for id in 0..MAX_ENTRIES * 2 {
            prune(&mut state);
            state.entries.insert(
                id.to_string(),
                Entry {
                    value: None,
                    touched: Instant::now(),
                    flight: None,
                    refresh_failed: false,
                },
            );
        }
        assert_eq!(state.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn desktop_browse_snapshot_wire_contract_is_camel_case() {
        let value = serde_json::to_value(BrowseCache::new().peek("opaque")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "entries": [], "scope": "opaque", "observedAt": null,
                "freshness": "unavailable",
            })
        );
    }

    fn rows_with_spare_capacity(bytes: usize) -> Vec<SharedStorageObjectEntry> {
        let mut entries = rows("small visible row");
        let mut name = String::with_capacity(bytes);
        name.push_str("small visible row");
        entries[0].name = name;
        entries
    }

    fn assert_budget(cache: &BrowseCache) -> usize {
        let state = cache.state.lock().unwrap();
        let sum = state
            .entries
            .values()
            .filter_map(|entry| entry.value.as_ref())
            .map(|value| {
                assert!(value.charged_bytes <= MAX_SNAPSHOT_BYTES);
                value.charged_bytes
            })
            .try_fold(0usize, usize::checked_add)
            .unwrap();
        assert_eq!(state.cached_bytes, sum);
        assert!(within_cache_budget(sum));
        sum
    }

    fn touched(cache: &BrowseCache, scope: &str, seconds_ago: u64) {
        cache
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(scope)
            .unwrap()
            .touched = Instant::now() - Duration::from_secs(seconds_ago);
    }

    #[test]
    fn desktop_browse_charge_checks_capacity_and_all_arithmetic_overflow() {
        let mut entries = rows("short");
        entries.reserve(20);
        entries[0].path.reserve(101);
        entries[0].name.reserve(203);
        entries[0].parent_path.reserve(307);
        let expected = std::mem::size_of::<CachedValue>()
            + 2 * std::mem::size_of::<usize>()
            + std::mem::size_of::<String>()
            + entries.capacity() * std::mem::size_of::<SharedStorageObjectEntry>()
            + 64
            + 40
            + entries[0].path.capacity()
            + entries[0].name.capacity()
            + entries[0].parent_path.capacity();
        assert_eq!(snapshot_charge(&entries, 64, 40), Some(expected));
        assert_eq!(checked_charge(usize::MAX, 0, 0, []), None);
        assert_eq!(checked_charge(0, usize::MAX, 0, []), None);
        assert_eq!(checked_charge(0, 0, usize::MAX, []), None);
        assert_eq!(checked_charge(0, 0, 0, [usize::MAX]), None);
        assert!(!within_cache_budget(usize::MAX));
    }

    #[tokio::test]
    async fn desktop_browse_oversized_snapshot_or_row_count_preserves_old_rows_and_other_scopes() {
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        let old = cache
            .refresh("target".into(), revision, |_| async {
                Ok(rows("keep this row"))
            })
            .await
            .unwrap();
        cache
            .refresh("other".into(), revision, |_| async {
                Ok(rows("another fresh scope"))
            })
            .await
            .unwrap();
        let charged = assert_budget(&cache);
        let oversized = vec![
            rows_with_spare_capacity(MAX_SNAPSHOT_BYTES),
            (0..=MAX_ROWS).map(|_| rows("row").pop().unwrap()).collect(),
            {
                // Only one live row, but a large unused backing Vec allocation.
                let capacity =
                    MAX_SNAPSHOT_BYTES / std::mem::size_of::<SharedStorageObjectEntry>() + 1;
                let mut sparse = Vec::with_capacity(capacity);
                sparse.push(rows("one live row").pop().unwrap());
                sparse
            },
        ];
        for entries in oversized {
            let error = cache
                .refresh("target".into(), revision, |_| async move { Ok(entries) })
                .await
                .unwrap_err();
            assert_eq!(error, OVERSIZED_METADATA);
            assert!(!error.contains("keep this row"));
            let cached = cache.peek("target");
            assert_eq!(cached.entries, old.entries);
            assert_eq!(cached.observed_at, old.observed_at);
            assert_eq!(cached.freshness, SharedStorageBrowseFreshness::Cached);
            assert_eq!(
                cache.peek("other").freshness,
                SharedStorageBrowseFreshness::Fresh
            );
            assert_eq!(assert_budget(&cache), charged);
        }
    }

    #[tokio::test]
    async fn desktop_browse_aggregate_byte_admission_evicts_idle_lru_only_and_keeps_fresh_scope() {
        const PAYLOAD: usize = 6 * 1024 * 1024;
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        cache
            .refresh("oldest".into(), revision, |_| async {
                Ok(rows_with_spare_capacity(PAYLOAD))
            })
            .await
            .unwrap();
        let kept = cache
            .refresh("recent".into(), revision, |_| async {
                Ok(rows_with_spare_capacity(PAYLOAD))
            })
            .await
            .unwrap();
        touched(&cache, "oldest", 20);
        cache.peek("recent");
        assert!(assert_budget(&cache) > 2 * PAYLOAD);
        cache
            .refresh("target".into(), revision, |_| async {
                Ok(rows_with_spare_capacity(PAYLOAD))
            })
            .await
            .unwrap();
        assert_eq!(
            cache.peek("oldest").freshness,
            SharedStorageBrowseFreshness::Unavailable
        );
        let recent = cache.peek("recent");
        assert_eq!(recent.entries, kept.entries);
        assert_eq!(recent.observed_at, kept.observed_at);
        assert_eq!(recent.freshness, SharedStorageBrowseFreshness::Fresh);
        assert_eq!(
            cache.peek("target").freshness,
            SharedStorageBrowseFreshness::Fresh
        );
        assert!(assert_budget(&cache) > 2 * PAYLOAD);
        cache.invalidate();
        assert_eq!(assert_budget(&cache), 0);
    }

    #[tokio::test]
    async fn desktop_browse_byte_admission_protects_active_flights_and_credits_current_replacement()
    {
        const MIB: usize = 1024 * 1024;
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        for (scope, bytes) in [("active", 6 * MIB), ("idle", 6 * MIB), ("target", 2 * MIB)] {
            cache
                .refresh(scope.into(), revision, move |_| async move {
                    Ok(rows_with_spare_capacity(bytes))
                })
                .await
                .unwrap();
        }
        touched(&cache, "active", 40);
        touched(&cache, "idle", 20);
        let mut active = Box::pin(cache.refresh("active".into(), revision, |_| async {
            std::future::pending::<()>().await;
            Ok(rows("never"))
        }));
        assert!(futures_util::poll!(active.as_mut()).is_pending());
        // The oldest scope is protected by a running refresh. Credit target's
        // old allocation, then evict idle rather than active or current target.
        cache
            .refresh("target".into(), revision, |_| async {
                Ok(rows_with_spare_capacity(7 * MIB))
            })
            .await
            .unwrap();
        assert_eq!(
            cache.peek("idle").freshness,
            SharedStorageBrowseFreshness::Unavailable
        );
        assert_eq!(
            cache.peek("active").freshness,
            SharedStorageBrowseFreshness::Cached
        );
        assert!(refreshing(&cache, "active"));
        assert_eq!(
            cache.peek("target").freshness,
            SharedStorageBrowseFreshness::Fresh
        );
        assert_budget(&cache);
        drop(active);

        // A same-size replacement needs no eviction: old bytes are credited.
        let before = assert_budget(&cache);
        cache
            .refresh("target".into(), revision, |_| async {
                Ok(rows_with_spare_capacity(7 * MIB))
            })
            .await
            .unwrap();
        // RFC3339 fractional-second precision can change timestamp capacity.
        assert!(assert_budget(&cache).abs_diff(before) <= 128);
        assert!(cache.peek("active").observed_at.is_some());
    }

    #[tokio::test]
    async fn desktop_browse_insufficient_idle_budget_rejects_without_partial_lru_eviction() {
        const MIB: usize = 1024 * 1024;
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        for (scope, bytes) in [
            ("active-a", 6 * MIB),
            ("active-b", 6 * MIB),
            ("idle", MIB),
            ("target", MIB),
        ] {
            cache
                .refresh(scope.into(), revision, move |_| async move {
                    Ok(rows_with_spare_capacity(bytes))
                })
                .await
                .unwrap();
        }
        let old = cache.peek("target");
        let before = assert_budget(&cache);
        let mut a = Box::pin(cache.refresh("active-a".into(), revision, |_| async {
            std::future::pending::<()>().await;
            Ok(rows("never"))
        }));
        let mut b = Box::pin(cache.refresh("active-b".into(), revision, |_| async {
            std::future::pending::<()>().await;
            Ok(rows("never"))
        }));
        assert!(futures_util::poll!(a.as_mut()).is_pending());
        assert!(futures_util::poll!(b.as_mut()).is_pending());
        let error = cache
            .refresh("target".into(), revision, |_| async {
                Ok(rows_with_spare_capacity(7 * MIB))
            })
            .await
            .unwrap_err();
        assert_eq!(error, OVERSIZED_METADATA);
        let after = cache.peek("target");
        assert_eq!(after.entries, old.entries);
        assert_eq!(after.observed_at, old.observed_at);
        assert_eq!(after.freshness, SharedStorageBrowseFreshness::Cached);
        assert!(
            cache.peek("idle").observed_at.is_some(),
            "a failed eviction plan is atomic"
        );
        assert!(refreshing(&cache, "active-a"));
        assert!(refreshing(&cache, "active-b"));
        assert_eq!(assert_budget(&cache), before);
    }

    #[tokio::test]
    async fn desktop_browse_cached_dto_clones_are_individually_bounded_and_channel_shares_allocation(
    ) {
        let cache = BrowseCache::new();
        let revision = cache.observe_selection("account".into());
        let mut entries = rows("large");
        entries[0].name = "x".repeat(MAX_SNAPSHOT_BYTES - 4096);
        cache
            .refresh("scope".into(), revision, |_| async move { Ok(entries) })
            .await
            .unwrap();
        let charged = assert_budget(&cache);
        assert!(charged < MAX_SNAPSHOT_BYTES);
        for _ in 0..3 {
            let mut cloned = cache.peek("scope");
            let timestamp = cloned.observed_at.as_ref().unwrap();
            assert!(
                snapshot_charge(
                    &cloned.entries,
                    cloned.scope.capacity(),
                    timestamp.capacity()
                )
                .unwrap()
                    <= charged
            );
            cloned.entries[0].name.clear();
            assert_eq!(
                assert_budget(&cache),
                charged,
                "caller-owned DTOs cannot change retained accounting"
            );
        }
        // Completion channels carry Arc<CachedValue>, never an uncharged deep
        // snapshot copy. Cloning their success value is allocation sharing.
        let value = cache.state.lock().unwrap().entries["scope"]
            .value
            .as_ref()
            .unwrap()
            .clone();
        let (sender, receiver) = watch::channel::<Option<SharedOutcome>>(Some(Ok(value.clone())));
        let shared = receiver.borrow().clone().unwrap().unwrap();
        assert!(Arc::ptr_eq(&value, &shared));
        assert!(Arc::ptr_eq(
            &value,
            cache.state.lock().unwrap().entries["scope"]
                .value
                .as_ref()
                .unwrap()
        ));
        drop(sender);
        assert_eq!(assert_budget(&cache), charged);
    }
}
