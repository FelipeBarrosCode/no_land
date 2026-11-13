//! Repository-key-bound point/batch reads with a bounded operation-local cache.
use super::*;
use std::collections::VecDeque;

const CACHE_ENTRIES: usize = 4096;

#[derive(Default)]
struct HintCache {
    entries: HashMap<String, Option<PackIndexEntry>>,
    order: VecDeque<String>,
}

pub(super) struct ScopedIndex {
    db: noland_state_db::StateDb,
    binding: String,
    key: MasterKey,
    observed_at: chrono::DateTime<Utc>,
    cache: parking_lot::Mutex<HintCache>,
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
        }
    }

    pub(super) fn get(&self, hash: &str) -> Result<Option<PackIndexEntry>> {
        let mut cache = self.cache.lock();
        if let Some(entry) = cache.entries.get(hash) {
            return Ok(entry.clone());
        }
        let entry = read_entries(
            &self.db,
            &self.binding,
            &self.key,
            self.observed_at,
            &[hash],
        )?
        .remove(hash);
        if cache.entries.len() == CACHE_ENTRIES {
            if let Some(old) = cache.order.pop_front() {
                cache.entries.remove(&old);
            }
        }
        cache.order.push_back(hash.into());
        cache.entries.insert(hash.into(), entry.clone());
        Ok(entry)
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
