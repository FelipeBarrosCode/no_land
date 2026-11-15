use super::*;
use std::sync::{atomic::Ordering, Barrier};

fn fixture() -> ScopedIndex {
    ScopedIndex::new(
        noland_state_db::StateDb::open_in_memory().unwrap(),
        "repository-a:key-a".into(),
        MasterKey([7; 32]),
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    )
}

fn hash(id: usize) -> String {
    format!("blake3:{id:064x}")
}

fn entry(id: usize) -> PackIndexEntry {
    PackIndexEntry {
        chunk_hash: hash(id),
        pack_id: Uuid::from_u128(id as u128 + 1).to_string(),
        offset: 24,
        ciphertext_len: 48,
        plaintext_len: 32,
        compression: noland_pack::PackCompression::None,
        nonce: vec![3; 24],
    }
}

fn row(index: &ScopedIndex, entry: &PackIndexEntry) -> RemoteContentEntry {
    RemoteContentEntry {
        storage_id: index.binding.clone(),
        object_kind: ContentObjectKind::Chunk,
        content_hash: entry.chunk_hash.clone(),
        remote_path: pack_key(&entry.pack_id),
        size: Some(u64::from(entry.plaintext_len)),
        etag: Some(
            cache::index_receipt(&index.key, &index.binding, "authenticated-index", entry).unwrap(),
        ),
        state: RemoteContentState::Present,
        observed_at: index.observed_at,
        expires_at: None,
    }
}

fn reads(index: &ScopedIndex) -> usize {
    index.batch_reads.load(Ordering::Relaxed)
}

fn assert_entry(actual: Option<PackIndexEntry>, expected: &PackIndexEntry) {
    assert_eq!(
        serde_json::to_value(actual.unwrap()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

fn assert_cache_consistent(index: &ScopedIndex) {
    let cache = index.cache.lock();
    assert!(cache.entries.len() <= CACHE_ENTRIES);
    let ordered: BTreeSet<_> = cache.order.iter().map(String::as_str).collect();
    let stored: BTreeSet<_> = cache.entries.keys().map(String::as_str).collect();
    assert_eq!(cache.order.len(), cache.entries.len());
    assert_eq!(ordered.len(), cache.order.len());
    assert_eq!(ordered, stored);
}

#[test]
fn prefetch_warms_positive_and_cold_negative_hints_in_one_batch() {
    let index = fixture();
    let positive = entry(1);
    let negative = entry(2);
    index
        .db
        .upsert_remote_content_entry(&row(&index, &positive))
        .unwrap();
    index
        .prefetch(&[
            &positive.chunk_hash,
            &negative.chunk_hash,
            &positive.chunk_hash,
        ])
        .unwrap();
    assert_eq!(reads(&index), 1);
    assert_eq!(index.cached_entries(), 2);

    // Mutating the disposable DB makes any accidental per-hash read observable.
    // These are operation-local hints, not fresh restore/presence assertions.
    index
        .db
        .remove_remote_content_entry(
            &index.binding,
            ContentObjectKind::Chunk,
            &positive.chunk_hash,
        )
        .unwrap();
    index
        .db
        .upsert_remote_content_entry(&row(&index, &negative))
        .unwrap();
    assert_entry(index.get(&positive.chunk_hash).unwrap(), &positive);
    assert!(index.get(&negative.chunk_hash).unwrap().is_none());
    assert_eq!(reads(&index), 1);
    assert_cache_consistent(&index);

    let next_operation = ScopedIndex::new(
        index.db.clone(),
        index.binding.clone(),
        index.key.clone(),
        index.observed_at,
    );
    next_operation
        .prefetch(&[&positive.chunk_hash, &negative.chunk_hash])
        .unwrap();
    assert!(next_operation.get(&positive.chunk_hash).unwrap().is_none());
    assert_entry(next_operation.get(&negative.chunk_hash).unwrap(), &negative);
}

#[test]
fn prefetch_deduplicates_and_does_not_reread_warm_or_empty_windows() {
    let index = fixture();
    let first = hash(1);
    let second = hash(2);
    let repeated = vec![first.as_str(); PREFETCH_HASHES];
    index.prefetch(&repeated).unwrap();
    assert_eq!(reads(&index), 1);
    assert_eq!(index.cached_entries(), 1);
    index.prefetch(&[]).unwrap();
    index.prefetch(&repeated).unwrap();
    assert_eq!(reads(&index), 1);
    index.prefetch(&[&first, &second, &second]).unwrap();
    assert_eq!(reads(&index), 2);
    assert_eq!(index.cached_entries(), 2);
    index.prefetch(&[&second, &first]).unwrap();
    assert_eq!(reads(&index), 2);
    assert_cache_consistent(&index);
}

#[test]
fn oversized_prefetch_is_rejected_before_reading_or_caching() {
    let index = fixture();
    let hashes = vec!["duplicate"; PREFETCH_HASHES + 1];
    assert!(index.prefetch(&hashes).is_err());
    assert_eq!(reads(&index), 0);
    assert_eq!(index.cached_entries(), 0);
    assert_cache_consistent(&index);
}

#[test]
fn large_receipts_consumed_by_windows_do_not_fall_back_to_point_reads() {
    let index = fixture();
    let hashes: Vec<_> = (0..CACHE_ENTRIES + 2 * PREFETCH_HASHES).map(hash).collect();
    for (batch, window) in hashes.chunks(PREFETCH_HASHES).enumerate() {
        let refs: Vec<_> = window.iter().map(String::as_str).collect();
        index.prefetch(&refs).unwrap();
        assert_eq!(reads(&index), batch + 1);
        for hash in window {
            assert!(index.get(hash).unwrap().is_none());
        }
        assert_eq!(reads(&index), batch + 1);
        assert_cache_consistent(&index);
    }
    assert_eq!(index.cached_entries(), CACHE_ENTRIES);
    assert!(!index.cache.lock().entries.contains_key(&hashes[0]));
    let before = reads(&index);
    assert!(index.get(&hashes[0]).unwrap().is_none());
    assert_eq!(reads(&index), before + 1);
    assert_cache_consistent(&index);
}

#[test]
fn mixed_window_keeps_oldest_cached_hit_resident_when_the_cache_is_full() {
    let index = fixture();
    let positive = entry(0);
    index
        .db
        .upsert_remote_content_entry(&row(&index, &positive))
        .unwrap();
    let hashes: Vec<_> = (0..CACHE_ENTRIES).map(hash).collect();
    for window in hashes.chunks(PREFETCH_HASHES) {
        index
            .prefetch(&window.iter().map(String::as_str).collect::<Vec<_>>())
            .unwrap();
    }
    let mut window: Vec<_> = (CACHE_ENTRIES..CACHE_ENTRIES + PREFETCH_HASHES - 1)
        .map(hash)
        .collect();
    // Put the old hit last: inserting misses must not evict it first.
    window.push(positive.chunk_hash.clone());
    let refs: Vec<_> = window.iter().map(String::as_str).collect();
    let before = reads(&index);
    index.prefetch(&refs).unwrap();
    assert_eq!(reads(&index), before + 1);
    for hash in &window[..window.len() - 1] {
        assert!(index.get(hash).unwrap().is_none());
    }
    assert_entry(index.get(&positive.chunk_hash).unwrap(), &positive);
    assert_eq!(reads(&index), before + 1);
    assert!(!index.cache.lock().entries.contains_key(&hashes[1]));
    assert_eq!(index.cached_entries(), CACHE_ENTRIES);
    assert_cache_consistent(&index);
}

#[test]
fn prefetched_receipts_are_bound_to_repository_and_key() {
    let index = fixture();
    let positive = entry(1);
    let original = row(&index, &positive);
    index.db.upsert_remote_content_entry(&original).unwrap();
    let mut replay = original;
    replay.storage_id = "repository-b:key-a".into();
    index.db.upsert_remote_content_entry(&replay).unwrap();
    let other_repository = ScopedIndex::new(
        index.db.clone(),
        replay.storage_id,
        index.key.clone(),
        index.observed_at,
    );
    let other_key = ScopedIndex::new(
        index.db.clone(),
        index.binding.clone(),
        MasterKey([8; 32]),
        index.observed_at,
    );
    for rejected in [&other_repository, &other_key] {
        rejected.prefetch(&[&positive.chunk_hash]).unwrap();
        assert!(rejected.get(&positive.chunk_hash).unwrap().is_none());
        assert_eq!(reads(rejected), 1);
    }
    index.prefetch(&[&positive.chunk_hash]).unwrap();
    assert_entry(index.get(&positive.chunk_hash).unwrap(), &positive);
}

#[test]
fn prefetch_rejects_forged_expired_missing_and_mismatched_hints() {
    let index = fixture();
    let reasons = [
        "forged",
        "malformed",
        "no-receipt",
        "expired",
        "expiry-boundary",
        "missing",
        "unknown",
        "wrong-hash",
        "wrong-path",
        "wrong-length",
        "ciphertext-length",
        "no-length",
        "wrong-kind",
    ];
    let mut rejected = Vec::new();
    for (id, reason) in reasons.into_iter().enumerate() {
        let entry = entry(id + 1);
        let mut row = row(&index, &entry);
        match reason {
            "forged" => {
                let mut payload: serde_json::Value =
                    serde_json::from_str(row.etag.as_deref().unwrap()).unwrap();
                payload["entry"]["offset"] = serde_json::json!(999);
                row.etag = Some(payload.to_string());
            }
            "malformed" => row.etag = Some("not a receipt".into()),
            "no-receipt" => row.etag = None,
            "expired" => row.expires_at = Some(index.observed_at - chrono::Duration::seconds(1)),
            "expiry-boundary" => row.expires_at = Some(index.observed_at),
            "missing" => row.state = RemoteContentState::Missing,
            "unknown" => row.state = RemoteContentState::Unknown,
            "wrong-hash" => row.content_hash = hash(1000),
            "wrong-path" => row.remote_path = format!("./{}", row.remote_path),
            "wrong-length" => row.size = Some(u64::from(entry.plaintext_len) + 1),
            "ciphertext-length" => row.size = Some(u64::from(entry.ciphertext_len)),
            "no-length" => row.size = None,
            "wrong-kind" => row.object_kind = ContentObjectKind::Pack,
            _ => unreachable!(),
        }
        rejected.push((reason, row));
    }
    let positive = entry(100);
    let mut fresh = row(&index, &positive);
    fresh.expires_at = Some(index.observed_at + chrono::Duration::seconds(1));
    let mut rows: Vec<_> = rejected.iter().map(|(_, row)| row.clone()).collect();
    rows.push(fresh);
    index.db.upsert_remote_content_entries(&rows).unwrap();
    let refs: Vec<_> = rows.iter().map(|row| row.content_hash.as_str()).collect();
    index.prefetch(&refs).unwrap();
    for (reason, row) in rejected {
        assert!(
            index.get(&row.content_hash).unwrap().is_none(),
            "accepted {reason} hint"
        );
    }
    assert_entry(index.get(&positive.chunk_hash).unwrap(), &positive);
    assert_eq!(reads(&index), 1);
    assert_cache_consistent(&index);
}

#[test]
fn retirement_invalidation_excludes_all_hints_in_a_retired_pack() {
    let index = fixture();
    let retired = entry(1);
    let mut same_pack = entry(2);
    same_pack.pack_id = retired.pack_id.clone();
    let live = entry(3);
    let rows: Vec<_> = [&retired, &same_pack, &live]
        .into_iter()
        .map(|entry| row(&index, entry))
        .collect();
    index.db.upsert_remote_content_entries(&rows).unwrap();
    // The repository admission path invalidates retired/tombstoned pack paths
    // before an operation starts using its scoped index.
    assert_eq!(
        index
            .db
            .invalidate_remote_pack_paths(&index.binding, &[pack_key(&retired.pack_id)])
            .unwrap(),
        2
    );
    index
        .prefetch(&[&retired.chunk_hash, &same_pack.chunk_hash, &live.chunk_hash])
        .unwrap();
    assert!(index.get(&retired.chunk_hash).unwrap().is_none());
    assert!(index.get(&same_pack.chunk_hash).unwrap().is_none());
    assert_entry(index.get(&live.chunk_hash).unwrap(), &live);
    assert_eq!(reads(&index), 1);
}

#[test]
fn window_snapshot_restores_a_hit_evicted_while_missing_hashes_are_read() {
    let index = fixture();
    let positive = entry(1);
    let missing = hash(2);
    index
        .cache
        .lock()
        .remember(&positive.chunk_hash, Some(positive.clone()));
    let mut snapshot = HashMap::new();
    snapshot.insert(
        positive.chunk_hash.as_str(),
        index.cache.lock().entries[&positive.chunk_hash].clone(),
    );
    snapshot.insert(missing.as_str(), None);
    // Deterministically model other readers filling the cache during SQLite IO.
    for id in 0..CACHE_ENTRIES {
        index.cache.lock().remember(&hash(id + 100), None);
    }
    assert!(!index
        .cache
        .lock()
        .entries
        .contains_key(&positive.chunk_hash));
    index
        .cache
        .lock()
        .remember_window(&[&missing, &positive.chunk_hash], snapshot);
    assert_entry(index.get(&positive.chunk_hash).unwrap(), &positive);
    assert!(index.get(&missing).unwrap().is_none());
    assert_eq!(reads(&index), 0);
    assert_cache_consistent(&index);
}

#[test]
fn racing_fill_results_preserve_existing_positive_and_negative_hints() {
    let index = fixture();
    let negative = entry(1);
    let positive = entry(2);
    {
        let mut cache = index.cache.lock();
        assert!(cache.remember(&negative.chunk_hash, None).is_none());
        assert!(cache
            .remember(&negative.chunk_hash, Some(negative.clone()))
            .is_none());
        assert_entry(
            cache.remember(&positive.chunk_hash, Some(positive.clone())),
            &positive,
        );
        assert_entry(cache.remember(&positive.chunk_hash, None), &positive);
    }
    assert_eq!(index.cached_entries(), 2);
    assert_cache_consistent(&index);
}

#[test]
fn concurrent_point_fill_results_do_not_duplicate_eviction_order_entries() {
    let index = fixture();
    for id in 0..CACHE_ENTRIES {
        index.cache.lock().remember(&hash(id), None);
    }
    let positive = entry(CACHE_ENTRIES);
    let barrier = Barrier::new(8);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let index = &index;
            let positive = &positive;
            let barrier = &barrier;
            scope.spawn(move || {
                // Each reader already has a result from the same unlocked read.
                let stale_result = Some(positive.clone());
                barrier.wait();
                assert_entry(
                    index
                        .cache
                        .lock()
                        .remember(&positive.chunk_hash, stale_result),
                    positive,
                );
            });
        }
    });
    assert_eq!(index.cached_entries(), CACHE_ENTRIES);
    assert_cache_consistent(&index);
}

#[test]
fn concurrent_prefetch_and_lookup_keep_positive_negative_cache_and_order_bounded() {
    const WORKERS: usize = 8;
    const ROUNDS: usize = 24;
    let index = fixture();
    let positive = entry(0);
    index
        .db
        .upsert_remote_content_entry(&row(&index, &positive))
        .unwrap();
    let barrier = Barrier::new(WORKERS);
    std::thread::scope(|scope| {
        for worker in 0..WORKERS {
            let index = &index;
            let positive = &positive;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                for round in 0..ROUNDS {
                    let mut hashes: Vec<_> = (0..16).map(hash).collect();
                    hashes.extend(
                        (0..64).map(|id| hash(100_000 + (worker * ROUNDS + round) * 64 + id)),
                    );
                    hashes.push(positive.chunk_hash.clone());
                    let refs: Vec<_> = hashes.iter().map(String::as_str).collect();
                    index.prefetch(&refs).unwrap();
                    assert_entry(index.get(&positive.chunk_hash).unwrap(), positive);
                    for hash in &hashes[1..hashes.len() - 1] {
                        assert!(index.get(hash).unwrap().is_none());
                    }
                    assert_cache_consistent(index);
                }
            });
        }
    });
    // Arbitrarily delayed concurrent readers may legitimately have their
    // windows evicted by other threads before they consume them.
    assert!(reads(&index) >= WORKERS * ROUNDS);
    assert_eq!(index.cached_entries(), CACHE_ENTRIES);
    assert_cache_consistent(&index);
}

#[test]
fn remote_chunk_index_forwards_prefetch_and_preserves_materialized_lookup() {
    let scoped = fixture();
    let positive = entry(1);
    scoped
        .db
        .upsert_remote_content_entry(&row(&scoped, &positive))
        .unwrap();
    let remote = RemoteChunkIndex {
        entries: HashMap::new(),
        scoped: Some(scoped),
    };
    remote.prefetch(&[&positive.chunk_hash]).unwrap();
    assert_entry(remote.lookup(&positive.chunk_hash).unwrap(), &positive);
    assert_eq!(reads(remote.scoped.as_ref().unwrap()), 1);
    assert!(remote
        .prefetch(&vec![positive.chunk_hash.as_str(); PREFETCH_HASHES + 1])
        .is_err());
    assert_eq!(reads(remote.scoped.as_ref().unwrap()), 1);
    let mut materialized = RemoteChunkIndex::empty();
    materialized
        .entries
        .insert(positive.chunk_hash.clone(), positive.clone());
    materialized
        .prefetch(&[&positive.chunk_hash, "missing"])
        .unwrap();
    assert_entry(
        materialized.lookup(&positive.chunk_hash).unwrap(),
        &positive,
    );
    assert!(materialized.lookup("missing").unwrap().is_none());
}
