use super::*;
use crate::tests::{block_on, test_dir, test_file, test_manifest, test_plan};
use noland_state_core::{
    AppIdentity, ChunkRef, FileStateTrust, ManifestFile, PersistenceClass, SemanticRole,
};
use noland_storage::{Health, LocalStorage, RemoteEntry, RemoteMeta};
use scheduler::{Mode, Scheduler, Stats};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::AtomicUsize;

fn file(name: &str, chunks: &[Vec<u8>]) -> ManifestFile {
    let mut file = test_file(
        name,
        PersistenceClass::PersistentState,
        SemanticRole::UserState,
        &chunks.concat(),
    );
    file.mode = Some(0o640);
    file.chunks = chunks
        .iter()
        .map(|chunk| ChunkRef {
            hash: noland_cas::blake3_hex(chunk),
            size: chunk.len() as u64,
        })
        .collect();
    file
}

fn synthetic(root: &Path, layout: &[Vec<usize>]) -> (RestorePlan, Vec<PackIndexEntry>) {
    let bytes = |number: usize| (number as u64).to_le_bytes().to_vec();
    let plan = test_plan(
        root,
        test_manifest(
            layout
                .iter()
                .enumerate()
                .map(|(index, chunks)| {
                    file(
                        &format!("game/{index:04}"),
                        &chunks
                            .iter()
                            .map(|&number| bytes(number))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect(),
        ),
    );
    let entries = layout
        .iter()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|number| PackIndexEntry {
            chunk_hash: noland_cas::blake3_hex(&bytes(number)),
            pack_id: format!("pack-{number:04}"),
            offset: 0,
            ciphertext_len: 24,
            plaintext_len: 8,
            compression: Default::default(),
            nonce: vec![0; 24],
        })
        .collect();
    (plan, entries)
}

fn jobs(plan: &RestorePlan, index: &[PackIndexEntry], eligible: &[usize]) -> Vec<PackJob> {
    let needed = eligible
        .iter()
        .flat_map(|&index| &plan.manifest.files[index].chunks)
        .map(|chunk| chunk.hash.clone())
        .collect();
    pack_jobs(plan, &plan.priority_plan(), index, &needed, |index| {
        eligible.binary_search(&index).is_ok()
    })
    .unwrap()
}

fn number(job: PackJob) -> usize {
    job.id.strip_prefix("pack-").unwrap().parse().unwrap()
}

fn drain(scheduler: &mut Scheduler) -> Vec<usize> {
    std::iter::from_fn(|| scheduler.next().map(number)).collect()
}

fn snapshot(stats: &Stats) -> PipelineIoMetrics {
    let mut io = PipelineIoMetrics::default();
    stats.snapshot(&mut io);
    io
}

#[test]
fn dynamic_closes_shared_partial_before_opening_more_work_with_deterministic_ties() {
    for _ in 0..8 {
        let root = test_dir("scheduling-order");
        let (plan, index) = synthetic(&root, &[vec![0, 1, 0, 2], vec![0, 3], vec![4]]);
        for (mode, expected) in [
            (Mode::Baseline, vec![0, 1, 2, 3, 4]),
            (Mode::FileAware, vec![0, 3, 1, 2, 4]),
        ] {
            let stats = Arc::new(Stats::default());
            let mut scheduler = Scheduler::new(
                jobs(&plan, &index, &[0, 1, 2]),
                &plan,
                [0, 1, 2].into_iter(),
                mode,
                scheduler::WORKING_BYTES,
                Arc::clone(&stats),
            );
            assert_eq!(drain(&mut scheduler), expected);
            let io = snapshot(&stats);
            assert_eq!(io.scheduling_decisions, 5);
            if mode == Mode::FileAware {
                assert_eq!(
                    io.scheduling_files, 2,
                    "single-pack files need no graph node"
                );
                assert_eq!(
                    io.scheduling_edges, 5,
                    "repeated hashes/packs have one incidence"
                );
                assert_eq!(io.scheduling_incidence_visits, 5);
                assert_eq!(io.scheduling_reordered_decisions, 1);
                assert!(io.scheduling_build_peak_bytes <= scheduler::WORKING_BYTES);
                assert!(io.scheduling_score_updates <= 2 * io.scheduling_edges);
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn distinct_hashes_in_the_same_pack_share_one_file_incidence() {
    let root = test_dir("scheduling-multiple-records");
    let (plan, mut index) = synthetic(&root, &[vec![0, 1, 2], vec![1, 3], vec![0, 1]]);
    index[1].pack_id = index[0].pack_id.clone();
    index[1].offset = 64;
    let jobs = jobs(&plan, &index, &[0, 1, 2]);
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[0].entries.len(), 2);
    assert_eq!(jobs[0].selected_chunk_references, 5);
    let stats = Arc::new(Stats::default());
    let mut scheduled = Scheduler::new(
        jobs,
        &plan,
        [0, 1, 2].into_iter(),
        Mode::FileAware,
        scheduler::WORKING_BYTES,
        Arc::clone(&stats),
    );
    assert_eq!(drain(&mut scheduled), vec![0, 2, 3]);
    let io = snapshot(&stats);
    assert_eq!(io.scheduling_files, 2);
    assert_eq!(io.scheduling_edges, 4);
    assert_eq!(io.scheduling_incidence_visits, 4);
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "opt-in 186k-file/360-pack metadata-only scheduling workload"]
fn large_sparse_scheduling_workload() {
    let root = test_dir("scheduling-large-sparse");
    let count = 186_664;
    let layout = (0..count)
        .map(|index| vec![index % 360, (index + 1) % 360])
        .collect::<Vec<_>>();
    let (plan, index) = synthetic(&root, &layout);
    let eligible = scheduler::Eligibility::new(count, 0..count);
    let needed = index.iter().map(|entry| entry.chunk_hash.clone()).collect();
    let jobs = pack_jobs(&plan, &plan.priority_plan(), &index, &needed, |index| {
        eligible.contains(index)
    })
    .unwrap();
    let stats = Arc::new(Stats::default());
    stats.preflight(&eligible);
    let mut scheduled = Scheduler::new(
        jobs,
        &plan,
        eligible.iter(),
        Mode::FileAware,
        scheduler::GRAPH_BYTES,
        Arc::clone(&stats),
    );
    assert!(scheduled.feedback().is_some());
    assert_eq!(
        drain(&mut scheduled)
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        360
    );
    let io = snapshot(&stats);
    assert_eq!(io.scheduling_files, count as u64);
    assert_eq!(io.scheduling_edges, count as u64 * 2);
    assert_eq!(io.scheduling_incidence_visits, io.scheduling_edges);
    assert!(io.scheduling_build_peak_bytes <= scheduler::GRAPH_BYTES);
    assert!(io.scheduling_preflight_bytes < 24 * 1024);
    assert!(io.scheduling_score_updates < 360 * 16);
    println!(
        "RESTORE_SCHEDULING_SPARSE {}",
        serde_json::json!({ "files": count, "packs": 360, "io": io })
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hints_prefer_live_inline_hashers_but_never_change_priority_or_replay_graph_edges() {
    let root = test_dir("scheduling-hints");
    let (plan, index) = synthetic(&root, &[vec![0, 1, 2], vec![0, 3]]);
    let stats = Arc::new(Stats::default());
    let mut scheduler = Scheduler::new(
        jobs(&plan, &index, &[0, 1]),
        &plan,
        [0, 1].into_iter(),
        Mode::FileAware,
        scheduler::WORKING_BYTES,
        Arc::clone(&stats),
    );
    let feedback = scheduler.feedback().unwrap();
    assert_eq!(number(scheduler.next().unwrap()), 0);
    for _ in 0..1024 {
        feedback.observe(0, false, false);
    }
    // File 0 is actually hashing; file 1 has only predicted progress. Protect
    // the live hasher's next pack before the merely predicted closing pack.
    assert_eq!(number(scheduler.next().unwrap()), 1);
    feedback.observe(0, true, false); // eviction/out-of-order is irreversible
    feedback.observe(0, false, false); // retry cannot restore inline state
    feedback.observe(1, false, false);
    assert_eq!(drain(&mut scheduler), vec![3, 2]);
    let io = snapshot(&stats);
    assert_eq!(io.scheduling_incidence_visits, 5);
    assert!(io.scheduling_coalesced_hints >= 1023);
    assert_eq!(io.scheduling_dropped_hints, 0);
    assert!(io.scheduling_hints <= 4);

    let mut prioritized = jobs(&plan, &index, &[0, 1]);
    for job in &mut prioritized {
        if job.id == "pack-0003" {
            job.priority = RestorePriority::Background;
        }
    }
    let mut scheduler = Scheduler::new(
        prioritized,
        &plan,
        [0, 1].into_iter(),
        Mode::FileAware,
        scheduler::WORKING_BYTES,
        Arc::default(),
    );
    assert_eq!(number(scheduler.next().unwrap()), 0);
    scheduler.feedback().unwrap().observe(1, false, false);
    assert_eq!(drain(&mut scheduler), vec![1, 2, 3]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fair_dispatch_bounds_bypass_without_a_priority_completion_barrier() {
    let root = test_dir("scheduling-fairness");
    let mut layout = vec![vec![0, 1, 2]];
    layout.extend((3..35).map(|tail| vec![0, tail]));
    let (plan, index) = synthetic(&root, &layout);
    let eligible = (0..layout.len()).collect::<Vec<_>>();
    let stats = Arc::new(Stats::default());
    let mut scheduled = Scheduler::new(
        jobs(&plan, &index, &eligible),
        &plan,
        eligible.iter().copied(),
        Mode::FileAware,
        scheduler::WORKING_BYTES,
        Arc::clone(&stats),
    );
    let order = drain(&mut scheduled);
    assert_eq!(order[0], 0);
    assert_eq!(order[7], 1, "oldest non-closing pack must not starve");
    assert!(order.iter().position(|&pack| pack == 2).unwrap() <= 15);
    assert_eq!(order.iter().copied().collect::<BTreeSet<_>>().len(), 35);
    let io = snapshot(&stats);
    assert_eq!(io.scheduling_fairness_decisions, 4);
    assert_eq!(io.scheduling_incidence_visits, io.scheduling_edges);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn bounded_graph_and_overflowing_coalesced_hints_fall_back_without_losing_jobs() {
    let root = test_dir("scheduling-bounds");
    let layout = (0..2048)
        .map(|number| vec![0, number + 1])
        .collect::<Vec<_>>();
    let (plan, index) = synthetic(&root, &layout);
    let eligible = (0..layout.len()).collect::<Vec<_>>();
    let baseline = jobs(&plan, &index, &eligible)
        .into_iter()
        .map(number)
        .collect::<Vec<_>>();
    for budget in [0, 64 * 1024, 2 * 1024 * 1024] {
        let stats = Arc::new(Stats::default());
        let mut scheduler = Scheduler::new(
            jobs(&plan, &index, &eligible),
            &plan,
            eligible.iter().copied(),
            Mode::FileAware,
            budget,
            Arc::clone(&stats),
        );
        assert!(scheduler.feedback().is_none());
        assert_eq!(drain(&mut scheduler), baseline);
        let io = snapshot(&stats);
        assert_eq!(io.pack_scheduling, "baseline_metadata_budget");
        assert!(io.scheduling_build_peak_bytes <= budget);
    }
    let stats = Arc::new(Stats::default());
    let mut scheduler = Scheduler::new(
        jobs(&plan, &index, &eligible),
        &plan,
        eligible.iter().copied(),
        Mode::FileAware,
        scheduler::WORKING_BYTES,
        Arc::clone(&stats),
    );
    assert_eq!(number(scheduler.next().unwrap()), 0);
    let feedback = scheduler.feedback().unwrap();
    for index in 0..layout.len() {
        feedback.observe(index, false, false);
        feedback.observe(index, false, false);
    }
    let first = scheduler.next().unwrap();
    let after_one = snapshot(&stats);
    assert_eq!(
        after_one.scheduling_hints, 512,
        "one decision has bounded hint work"
    );
    assert_eq!(after_one.scheduling_dropped_hints, 1536);
    assert!(after_one.scheduling_coalesced_hints >= layout.len() as u64);
    let mut order = vec![0, number(first)];
    order.extend(drain(&mut scheduler));
    order.sort_unstable();
    assert_eq!(order, (0..=2048).collect::<Vec<_>>());
    let io = snapshot(&stats);
    assert_eq!(io.scheduling_incidence_visits, 4096);
    assert!(io.scheduling_build_peak_bytes <= scheduler::WORKING_BYTES);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reused_preflight_files_do_not_supply_priority_or_pressure_even_with_shared_hashes() {
    let root = test_dir("scheduling-reused");
    let (mut plan, index) = synthetic(&root, &[vec![0, 1], vec![0, 2]]);
    plan.manifest.files[0].semantic_role = SemanticRole::AppContent;
    let jobs = jobs(&plan, &index, &[1]);
    assert_eq!(jobs.len(), 2);
    assert!(jobs
        .iter()
        .all(|job| job.priority == RestorePriority::Critical));
    assert!(jobs.iter().all(|job| job.first_use.0 == 1));
    assert!(jobs.iter().all(|job| job.selected_chunk_references == 1));
    let stats = Arc::new(Stats::default());
    let mut scheduler = Scheduler::new(
        jobs,
        &plan,
        [1].into_iter(),
        Mode::FileAware,
        scheduler::WORKING_BYTES,
        Arc::clone(&stats),
    );
    scheduler.feedback().unwrap().observe(0, false, false);
    assert_eq!(drain(&mut scheduler), vec![0, 2]);
    assert_eq!(snapshot(&stats).scheduling_files, 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn preflight_bitmap_is_compact_precise_and_has_a_hard_allocation_ceiling() {
    let eligible = scheduler::Eligibility::new(186_664, [0, 63, 64, 186_663].into_iter());
    assert!(eligible.precise());
    assert_eq!(
        eligible.iter().collect::<Vec<_>>(),
        vec![0, 63, 64, 186_663]
    );
    assert!(eligible.contains(63));
    assert!(!eligible.contains(65));
    assert!(!eligible.contains(186_664));
    assert!(eligible.bytes() < 24 * 1024);
    let empty = scheduler::Eligibility::new(0, std::iter::empty());
    assert_eq!(empty.bytes(), 0);
    assert_eq!(empty.iter().count(), 0);
    let oversized = scheduler::Eligibility::new(usize::MAX, std::iter::empty());
    assert!(!oversized.precise());
    assert_eq!(oversized.bytes(), 0);
    assert_eq!(oversized.iter().count(), 0);
    assert!(
        oversized.contains(123),
        "overflow retains baseline needed-hash filtering"
    );
}

#[test]
fn scored_downloads_refill_immediately_behind_a_slow_pack_and_keep_byte_bounds() {
    let root = test_dir("scheduling-slow-pack");
    let (plan, index) = synthetic(&root, &[vec![0, 1, 2], vec![0, 3]]);
    let mut scheduler = Scheduler::new(
        jobs(&plan, &index, &[0, 1]),
        &plan,
        [0, 1].into_iter(),
        Mode::FileAware,
        scheduler::WORKING_BYTES,
        Arc::default(),
    );
    let started = AtomicUsize::new(0);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let order = Mutex::new(Vec::new());
    block_on(run_admitted(
        8,
        4,
        2,
        |available| {
            (available >= 2)
                .then(|| scheduler.next())
                .flatten()
                .map(|job| (job, 2))
        },
        |job| {
            let (started, active, peak, order) = (&started, &active, &peak, &order);
            async move {
                let id = number(job);
                order.lock().unwrap().push(id);
                started.fetch_add(1, Ordering::Relaxed);
                let count = active.fetch_add(1, Ordering::Relaxed) + 1;
                peak.fetch_max(count, Ordering::Relaxed);
                let mut polls = 0;
                poll_fn(|cx| {
                    polls += 1;
                    assert!(polls < 100, "scheduler waited for the slowest active pack");
                    if id != 0 || started.load(Ordering::Relaxed) == 4 {
                        Poll::Ready(())
                    } else {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;
                active.fetch_sub(1, Ordering::Relaxed);
                Ok(())
            }
        },
    ))
    .unwrap();
    assert_eq!(*order.lock().unwrap(), vec![0, 3, 1, 2]);
    assert_eq!(
        peak.load(Ordering::Relaxed),
        2,
        "byte admission remains independent of slots"
    );
    assert_eq!(active.load(Ordering::Relaxed), 0);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_and_oversized_admission_starts_nothing_and_errors_drop_active_futures() {
    let started = AtomicUsize::new(0);
    for (limit, budget, weight) in [(0, 4, 2), (2, 0, 0), (2, 4, 5), (2, 4, u64::MAX)] {
        assert!(run_admitted(
            limit,
            budget,
            weight,
            |_| Some(((), weight)),
            |_| async {
                started.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .await
        .is_err());
    }
    // A large non-leading job is rejected before even the small head starts.
    assert!(run_weighted(vec![((), 1), ((), 5)], 2, 4, |_| async {
        started.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .await
    .is_err());
    assert_eq!(started.load(Ordering::Relaxed), 0);
    assert!(run_admitted(
        2,
        4,
        2,
        |_| Some(((), 5)),
        |_| async {
            started.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
    )
    .await
    .is_err());
    assert_eq!(started.load(Ordering::Relaxed), 0);

    struct Owned(Arc<AtomicUsize>);
    impl Drop for Owned {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    let owned = Arc::new(AtomicUsize::new(0));
    let mut jobs = vec![0, 1].into_iter();
    let result = run_admitted(
        2,
        4,
        2,
        |_| jobs.next().map(|job| (job, 2)),
        |job| {
            owned.fetch_add(1, Ordering::Relaxed);
            let guard = Owned(Arc::clone(&owned));
            async move {
                let _guard = guard;
                if job == 0 {
                    std::future::pending::<()>().await;
                }
                Err(StateError::Storage("injected transfer failure".into()))
            }
        },
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("injected"));
    assert_eq!(
        owned.load(Ordering::Relaxed),
        0,
        "failed admission loop retains no active future"
    );
    let mut jobs = vec![0, 1].into_iter();
    let cancelled = tokio::time::timeout(
        Duration::from_millis(1),
        run_admitted(
            2,
            4,
            2,
            |_| jobs.next().map(|job| (job, 2)),
            |_| {
                owned.fetch_add(1, Ordering::Relaxed);
                let guard = Owned(Arc::clone(&owned));
                async move {
                    let _guard = guard;
                    std::future::pending::<Result<()>>().await
                }
            },
        ),
    )
    .await;
    assert!(cancelled.is_err());
    assert_eq!(owned.load(Ordering::Relaxed), 0);
}

struct ReplayStorage {
    local: LocalStorage,
    objects: BTreeMap<String, Vec<u8>>,
    starts: Mutex<Vec<String>>,
    active: AtomicUsize,
}

struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[async_trait::async_trait]
impl SharedStorageProvider for ReplayStorage {
    async fn health_check(&self) -> Result<Health> {
        self.local.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.local.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.local.stat(key).await
    }
    async fn upload_immutable(&self, path: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.local.upload_immutable(path, key).await
    }
    async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
        self.local.download(key, path).await
    }
    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.local.list_prefix(prefix).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &RemoteKey,
    ) -> Result<RemoteMeta> {
        self.local.put_small_versioned(bytes, key).await
    }
    fn supports_streaming_downloads(&self) -> bool {
        true
    }
    async fn download_stream_observed(
        &self,
        key: &RemoteKey,
        _scratch: &Path,
        consumer: &dyn DownloadStreamConsumer,
        _observer: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        self.active.fetch_add(1, Ordering::Relaxed);
        let _active = Active(&self.active);
        self.starts.lock().unwrap().push(key.as_str().to_string());
        let bytes = &self.objects[key.as_str()];
        consumer.restart().await?;
        consumer.write(bytes).await?;
        // Replay a fully authenticated prefix after a simulated transport retry.
        consumer.restart().await?;
        consumer.write(bytes).await?;
        consumer.finish().await
    }
}

fn encrypted_fixture(
    root: &Path,
) -> (
    MasterKey,
    RestorePlan,
    Vec<PackIndexEntry>,
    ReplayStorage,
    Vec<Vec<u8>>,
) {
    let master = MasterKey::generate();
    let chunks = (0..6)
        .map(|number| format!("authenticated-chunk-{number}").into_bytes())
        .collect::<Vec<_>>();
    let layout = [vec![0, 1, 2], vec![0, 3], vec![4, 5, 4], vec![0, 3]];
    let files = layout
        .iter()
        .enumerate()
        .map(|(index, layout)| {
            file(
                &format!("game/{index}"),
                &layout
                    .iter()
                    .map(|&number| chunks[number].clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let payloads = layout
        .iter()
        .map(|layout| {
            layout
                .iter()
                .flat_map(|&number| chunks[number].clone())
                .collect()
        })
        .collect();
    let plan = test_plan(&root.join("restore"), test_manifest(files));
    let mut index = Vec::new();
    let mut objects = BTreeMap::new();
    for (number, chunk) in chunks.into_iter().enumerate() {
        let pack = noland_pack::pack_chunks(
            &root.join(format!("built-{number}")),
            &master,
            vec![(noland_cas::blake3_hex(&chunk), chunk)],
            |_| false,
        )
        .unwrap()
        .remove(0);
        objects.insert(pack_key(&pack.pack_id), fs::read(pack.path).unwrap());
        index.extend(pack.entries);
    }
    (
        master,
        plan,
        index,
        ReplayStorage {
            local: LocalStorage::new(root.join("unused-remote")),
            objects,
            starts: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
        },
        payloads,
    )
}

fn fixture_resources() -> WorkerResources {
    WorkerResources {
        logical_cpus: 16,
        available_memory: Some(4 * 1024 * 1024 * 1024),
        io_pressure: false,
    }
}

#[tokio::test]
async fn baseline_and_dynamic_authenticated_retry_shared_repeated_and_reused_files_are_equivalent()
{
    for mode in [Mode::Baseline, Mode::FileAware] {
        let root = test_dir("scheduling-equivalence");
        let (master, plan, index, storage, payloads) = encrypted_fixture(&root);
        let home = root.join("home");
        let game = home.join(".local/share/game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("3"), &payloads[3]).unwrap();
        fs::set_permissions(game.join("3"), fs::Permissions::from_mode(0o640)).unwrap();
        let db = StateDb::open_in_memory().unwrap();
        db.upsert_app(&AppIdentity::new(
            plan.manifest.app.app_id.clone(),
            "Scheduling",
        ))
        .unwrap();
        let mut final_progress = PipelineProgress::default();
        let report = restore_pipelined_with_scheduler(
            &storage,
            &master,
            &plan,
            &index,
            &LogicalRootMap::from_home(home),
            Some(&db),
            None,
            PipelineOptions {
                max_parallel_packs: 1,
                overlap_file_syncs: true,
                ..PipelineOptions::default()
            },
            fixture_resources(),
            mode,
            |sample| {
                final_progress = sample;
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(report.restore.reused_entries, 1);
        assert_eq!(report.restore.published_entries, 3);
        assert_eq!(report.packs_downloaded, 6);
        assert_eq!(
            storage.starts.lock().unwrap().len(),
            6,
            "scheduling creates no extra requests"
        );
        assert_eq!(storage.active.load(Ordering::Relaxed), 0);
        assert_eq!(
            report.io.file_syncs, 3,
            "retry must not verify/sync a placement twice"
        );
        assert_eq!(
            final_progress.assembled_bytes,
            payloads[..3]
                .iter()
                .map(|bytes| bytes.len() as u64)
                .sum::<u64>()
        );
        assert_eq!(
            final_progress.verified_bytes,
            final_progress.assembled_bytes
        );
        assert_eq!(report.io.readback_bytes, payloads[2].len() as u64);
        assert_eq!(
            report.io.readback_out_of_order_bytes,
            report.io.readback_bytes
        );
        assert_eq!(report.io.readback_hash_eviction_bytes, 0);
        assert_eq!(report.io.readback_missing_hash_bytes, 0);
        assert_eq!(report.io.readback_bytes, final_progress.readback_bytes);
        assert_eq!(report.io.scheduling_preflight_bytes, 8);
        assert!(report.io.estimated_working_set_bytes >= scheduler::WORKING_BYTES);
        if mode == Mode::FileAware {
            assert_eq!(report.io.pack_scheduling, "file_aware");
            assert_eq!(
                report.io.scheduling_files, 3,
                "reused file has no graph node"
            );
            assert_eq!(report.io.scheduling_edges, 7);
            assert_eq!(report.io.scheduling_incidence_visits, 7);
            assert!(
                report.io.scheduling_build_peak_bytes <= report.io.scheduling_memory_budget_bytes
            );
        } else {
            assert_eq!(report.io.pack_scheduling, "baseline_requested");
        }
        assert!(report.io.peak_active_hashers <= MAX_ACTIVE_HASHERS as u64);
        assert!(report.io.peak_assembly_handles <= MAX_ASSEMBLY_HANDLES as u64);
        assert!(report.io.peak_unpublished_files <= MAX_UNPUBLISHED_FILES as u64);
        assert!(report.io.peak_unpublished_bytes <= MAX_UNPUBLISHED_BYTES);
        assert_eq!(report.io.unfinished_files, 0);
        assert_eq!(report.io.unfinished_bytes, 0);
        let states = db
            .list_file_states(&plan.manifest.app.app_id, None)
            .unwrap();
        assert_eq!(states.len(), 4);
        assert!(states
            .iter()
            .all(|state| state.trust == FileStateTrust::Trusted));
        for (index, payload) in payloads.iter().enumerate() {
            assert_eq!(fs::read(game.join(index.to_string())).unwrap(), *payload);
        }
        assert!(!plan.staging.exists());
        assert!(!plan.pack_cache.exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn baseline_and_dynamic_corruption_and_cancellation_join_then_restore_originals() {
    for mode in [Mode::Baseline, Mode::FileAware] {
        for outcome in ["authentication", "full_hash", "cancellation"] {
            let root = test_dir("scheduling-rollback");
            let (master, mut plan, index, mut storage, payloads) = encrypted_fixture(&root);
            if outcome == "authentication" {
                *storage
                    .objects
                    .values_mut()
                    .next()
                    .unwrap()
                    .last_mut()
                    .unwrap() ^= 1;
            } else if outcome == "full_hash" {
                plan.manifest.files[2].file_hash = noland_cas::blake3_hex(b"wrong full-file hash");
            }
            let home = root.join("home");
            let game = home.join(".local/share/game");
            fs::create_dir_all(&game).unwrap();
            for number in 0..payloads.len() {
                fs::write(game.join(number.to_string()), format!("original-{number}")).unwrap();
            }
            fs::write(game.join("unrelated"), b"never touch").unwrap();
            let mut cancelled = false;
            let error = restore_pipelined_with_scheduler(
                &storage,
                &master,
                &plan,
                &index,
                &LogicalRootMap::from_home(home),
                None,
                None,
                PipelineOptions {
                    max_parallel_packs: 2,
                    overlap_file_syncs: true,
                    ..PipelineOptions::default()
                },
                fixture_resources(),
                mode,
                |sample| {
                    if outcome == "cancellation" && sample.completed_entries != 0 {
                        cancelled = true;
                        Err(StateError::Storage("cancel scheduled restore".into()))
                    } else {
                        Ok(())
                    }
                },
            )
            .await
            .unwrap_err();
            if outcome == "cancellation" {
                assert!(cancelled);
                assert!(error.to_string().contains("cancel"));
            } else if outcome == "full_hash" {
                assert!(error.to_string().contains("file hash mismatch"), "{error}");
            }
            assert_eq!(storage.active.load(Ordering::Relaxed), 0);
            for number in 0..payloads.len() {
                assert_eq!(
                    fs::read(game.join(number.to_string())).unwrap(),
                    format!("original-{number}").as_bytes()
                );
            }
            assert_eq!(fs::read(game.join("unrelated")).unwrap(), b"never touch");
            assert!(
                !plan.staging.exists(),
                "workers must join and rollback before return"
            );
            for number in 0..payloads.len() {
                assert!(
                    !destination_partial_path(&game.join(number.to_string()), plan.restore_id)
                        .unwrap()
                        .exists()
                );
            }
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn synthetic_baseline_dynamic_equivalence_reduces_eviction_readback_and_unfinished_pressure() {
    let mut results = Vec::new();
    // Pack 1's shared chunk starts another hash-budget-sized fanout. Static
    // manifest ordering puts it before pack 3, which closes the first fanout.
    // Dynamic admission closes that live group first, without retaining data.
    let mut layout = vec![vec![0, 1, 2]];
    layout.extend((0..64).map(|_| vec![0, 3]));
    layout.extend((0..MAX_ACTIVE_HASHERS - 1).map(|_| vec![1, 4]));
    for mode in [Mode::Baseline, Mode::FileAware] {
        let root = test_dir("scheduling-pressure-equivalence");
        let (plan, index) = synthetic(&root, &layout);
        let roots = LogicalRootMap::from_home(root.join("home"));
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
        pipeline.overlap_file_syncs = true;
        let eligible = pipeline.pending.keys().copied().collect::<Vec<_>>();
        let stats = Arc::new(Stats::default());
        let mut scheduled = Scheduler::new(
            jobs(&plan, &index, &eligible),
            &plan,
            eligible.iter().copied(),
            mode,
            scheduler::WORKING_BYTES,
            Arc::clone(&stats),
        );
        pipeline.scheduling_stats = stats;
        pipeline.scheduling_feedback = scheduled.feedback();
        let mut order = Vec::new();
        while let Some(job) = scheduled.next() {
            let number = number(job);
            order.push(number);
            let plain = (number as u64).to_le_bytes();
            pipeline
                .consume(&noland_cas::blake3_hex(&plain), &plain)
                .unwrap();
            // Retrying the same authenticated chunk has no assembly, pressure
            // or feedback effect, including after the partial was published.
            pipeline
                .consume(&noland_cas::blake3_hex(&plain), &plain)
                .unwrap();
            pipeline.dispatch_assembled().unwrap();
            pipeline.tick().unwrap();
        }
        let report = pipeline.finish().unwrap();
        transaction.commit().unwrap();
        assert_eq!(report.restore.published_entries, layout.len());
        assert_eq!(report.io.file_syncs, layout.len() as u64);
        assert!(report.io.peak_active_hashers <= MAX_ACTIVE_HASHERS as u64);
        assert!(report.io.peak_assembly_handles <= MAX_ASSEMBLY_HANDLES as u64);
        assert_eq!(
            report.io.readback_hash_eviction_bytes,
            report.io.readback_bytes
        );
        assert_eq!(report.io.readback_out_of_order_bytes, 0);
        assert_eq!(report.io.readback_missing_hash_bytes, 0);
        assert_eq!(report.io.unfinished_files, 0);
        assert_eq!(report.io.unfinished_bytes, 0);
        for (index, chunks) in layout.iter().enumerate() {
            let expected = chunks
                .iter()
                .flat_map(|&number| (number as u64).to_le_bytes())
                .collect::<Vec<_>>();
            assert_eq!(
                fs::read(root.join(format!("home/.local/share/game/{index:04}"))).unwrap(),
                expected
            );
        }
        println!(
            "RESTORE_SCHEDULING {}",
            serde_json::json!({
                "mode": format!("{mode:?}"), "files": layout.len(), "order": order, "io": report.io,
            })
        );
        results.push(report.io);
        fs::remove_dir_all(root).unwrap();
    }
    assert_eq!(results[0].hash_state_evictions, 64);
    assert_eq!(results[0].readback_bytes, 129 * 8);
    assert_eq!(results[1].hash_state_evictions, 0);
    assert_eq!(results[1].readback_bytes, 0);
    assert_eq!(results[0].peak_unfinished_files, 192);
    assert_eq!(results[1].peak_unfinished_files, 128);
}
