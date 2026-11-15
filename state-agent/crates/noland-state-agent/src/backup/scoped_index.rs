//! Repository-key-bound point/batch reads with a bounded operation-local cache.
use super::*;
use std::collections::VecDeque;

const CACHE_ENTRIES: usize = 4096;
pub(super) const PREFETCH_HASHES: usize = 256;

#[cfg(test)]
mod tests;

#[derive(Default)]
struct HintCache {
    entries: HashMap<String, Option<PackIndexEntry>>,
    order: VecDeque<String>,
}

impl HintCache {
    fn evict_if_full(&mut self) {
        if self.entries.len() >= CACHE_ENTRIES {
            let old = self
                .order
                .pop_front()
                .expect("a full hint cache has an eviction candidate");
            self.entries.remove(&old);
        }
    }

    fn remember(&mut self, hash: &str, entry: Option<PackIndexEntry>) -> Option<PackIndexEntry> {
        // Another reader may have filled this hash while SQLite was unlocked.
        if let Some(cached) = self.entries.get(hash) {
            return cached.clone();
        }
        self.evict_if_full();
        self.order.push_back(hash.into());
        self.entries.insert(hash.into(), entry.clone());
        entry
    }

    fn remember_window(
        &mut self,
        hashes: &[&str],
        mut window: HashMap<&str, Option<PackIndexEntry>>,
    ) {
        // Protect every hit in this window, including old FIFO entries, before
        // inserting any misses. Otherwise prefetch can evict its own hits.
        self.order
            .retain(|hash| !window.contains_key(hash.as_str()));
        for &hash in hashes {
            // Removing the snapshot also deduplicates the eviction order.
            if let Some(entry) = window.remove(hash) {
                if !self.entries.contains_key(hash) {
                    self.evict_if_full();
                    self.entries.insert(hash.into(), entry);
                }
                self.order.push_back(hash.into());
            }
        }
    }
}

pub(super) struct ScopedIndex {
    db: noland_state_db::StateDb,
    binding: String,
    key: MasterKey,
    observed_at: chrono::DateTime<Utc>,
    cache: parking_lot::Mutex<HintCache>,
    #[cfg(test)]
    batch_reads: std::sync::atomic::AtomicUsize,
}

impl ScopedIndex {
    #[cfg(test)]
    pub(super) fn cached_entries(&self) -> usize {
        self.cache.lock().entries.len()
    }
    pub(super) fn new(
        db: noland_state_db::StateDb,
        binding: String,
        key: MasterKey,
        observed_at: chrono::DateTime<Utc>,
    ) -> Self {
        Self {
            db,
            binding,
            key,
            observed_at,
            cache: parking_lot::Mutex::default(),
            #[cfg(test)]
            batch_reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(super) fn get(&self, hash: &str) -> Result<Option<PackIndexEntry>> {
        if let Some(entry) = self.cache.lock().entries.get(hash) {
            return Ok(entry.clone());
        }
        // Neither SQLite nor receipt authentication holds the cache mutex.
        let entry = self.read(&[hash])?.remove(hash);
        Ok(self.cache.lock().remember(hash, entry))
    }

    /// Warm one bounded window immediately before consuming its lookups.
    ///
    /// Do not prefetch a whole receipt and then iterate it: later windows can
    /// evict earlier ones. These operation-local observations are upload hints,
    /// never restore authority; repository retirement invalidation must precede
    /// construction of the index, as it does for point lookups.
    pub(super) fn prefetch(&self, hashes: &[&str]) -> Result<()> {
        if hashes.len() > PREFETCH_HASHES {
            return Err(StateError::Invalid(format!(
                "scoped index prefetch window exceeds {PREFETCH_HASHES} hashes"
            )));
        }
        if hashes.is_empty() {
            return Ok(());
        }
        let mut window = HashMap::with_capacity(hashes.len());
        let mut missing = Vec::with_capacity(hashes.len());
        {
            let cache = self.cache.lock();
            for &hash in hashes {
                if window.contains_key(hash) {
                    continue;
                }
                match cache.entries.get(hash) {
                    Some(entry) => {
                        // Keep the hit even if another reader evicts it during
                        // the unlocked read of this window's missing hashes.
                        window.insert(hash, entry.clone());
                    }
                    None => {
                        window.insert(hash, None);
                        missing.push(hash);
                    }
                }
            }
        }
        let mut entries = self.read(&missing)?;
        for hash in missing {
            // Absence and invalid receipts are cached just like positive hints.
            window.insert(hash, entries.remove(hash));
        }
        self.cache.lock().remember_window(hashes, window);
        Ok(())
    }

    fn read(&self, hashes: &[&str]) -> Result<HashMap<String, PackIndexEntry>> {
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }
        #[cfg(test)]
        self.batch_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        read_entries(&self.db, &self.binding, &self.key, self.observed_at, hashes)
    }
}

impl RemoteChunkIndex {
    /// Prefetch at most `PREFETCH_HASHES` hashes, then consume that window before
    /// prefetching the next one. An already materialized index needs no reads.
    pub(super) fn prefetch(&self, hashes: &[&str]) -> Result<()> {
        match &self.scoped {
            Some(index) => index.prefetch(hashes),
            None => Ok(()),
        }
    }
}

pub(super) fn read_entries(
    db: &noland_state_db::StateDb,
    binding: &str,
    key: &MasterKey,
    now: chrono::DateTime<Utc>,
    hashes: &[&str],
) -> Result<HashMap<String, PackIndexEntry>> {
    let mut output = HashMap::new();
    for row in db.remote_content_for_hashes(binding, ContentObjectKind::Chunk, hashes)? {
        if row.state != RemoteContentState::Present || !row.is_fresh_at(now) {
            continue;
        }
        if let Some(entry) = row
            .etag
            .as_deref()
            .and_then(|payload| cache::read_index_receipt(key, binding, payload))
        {
            if entry.chunk_hash == row.content_hash
                && row.remote_path == pack_key(&entry.pack_id)
                && row.size == Some(u64::from(entry.plaintext_len))
            {
                output.insert(row.content_hash, entry);
            }
        }
    }
    Ok(output)
}
