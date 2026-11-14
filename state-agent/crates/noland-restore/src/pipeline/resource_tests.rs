use super::*;
use crate::tests::{test_dir, test_file, test_manifest, test_plan};
use noland_state_core::{PersistenceClass, SemanticRole};
use noland_storage::{Health, LocalStorage, RemoteEntry, RemoteMeta};
use std::path::Path;
use std::sync::atomic::AtomicUsize;

fn resources(cpus: usize, memory: Option<u64>, io_pressure: bool) -> WorkerResources {
    WorkerResources {
        logical_cpus: cpus,
        available_memory: memory,
        io_pressure,
    }
}

fn assert_shared_admission(
    options: PipelineOptions,
    resources: WorkerResources,
    workload: TransferWorkload,
) {
    assert_eq!(options.workload, workload);
    let slots = options.max_parallel_packs + options.reconstruction_workers + 1;
    let charge = options.working_bytes().div_ceil(slots as u64);
    assert!(
        slots <= resources.cpu_workers(workload, charge),
        "aggregate working set/CPU allowance was multiplied across pools: {options:?}"
    );
    assert_eq!(
        options.working_bytes(),
        ASSEMBLY_WORKING_BYTES
            + options.max_parallel_packs as u64 * DECODER_WORKING_BYTES
            + options.reconstruction_workers as u64 * VERIFIER_WORKING_BYTES
            + options.buffered_chunks as u64 * FASTCDC_MAX
    );
}

#[test]
fn automatic_restore_uses_more_than_four_decoders_and_sixteen_verifiers_when_admitted() {
    let resources = resources(128, Some(64 * PACK_MAX), false);
    let options = PipelineOptions::for_resources(resources, TransferWorkload::Throughput);
    assert!(options.max_parallel_packs > 4, "{options:?}");
    assert!(options.reconstruction_workers > 16, "{options:?}");
    assert!(options.buffered_chunks > 32, "{options:?}");
    assert_shared_admission(options, resources, TransferWorkload::Throughput);

    let tuning = TransferTuning {
        max_parallel_downloads: 16,
        ..TransferTuning::default()
    };
    let provider_limited =
        PipelineOptions::for_transfer_tuning(resources, TransferWorkload::Throughput, &tuning);
    assert_eq!(provider_limited.max_parallel_packs, 16);
    assert!(provider_limited.reconstruction_workers > options.reconstruction_workers);
    assert_shared_admission(provider_limited, resources, TransferWorkload::Throughput);
}

#[test]
fn restore_memory_is_one_aggregate_reservation_and_unknown_memory_is_finite() {
    let host = resources(128, Some(512 * 1024 * 1024), false);
    let options = PipelineOptions::for_resources(host, TransferWorkload::Throughput);
    let ample = PipelineOptions::for_resources(
        resources(128, Some(64 * PACK_MAX), false),
        TransferWorkload::Throughput,
    );
    assert!(options.max_parallel_packs < ample.max_parallel_packs);
    assert!(options.buffered_chunks < ample.buffered_chunks);
    assert_shared_admission(options, host, TransferWorkload::Throughput);

    let unknown = resources(128, None, false);
    let fallback = PipelineOptions::for_resources(unknown, TransferWorkload::Throughput);
    assert!(fallback.max_parallel_packs < ample.max_parallel_packs);
    assert!(fallback.working_bytes() < ample.working_bytes());
    assert_shared_admission(fallback, unknown, TransferWorkload::Throughput);
}

#[test]
fn active_gameplay_matches_throughput_while_balanced_and_io_guards_remain() {
    let host = resources(128, Some(64 * PACK_MAX), false);
    let throughput = PipelineOptions::for_resources(host, TransferWorkload::Throughput);
    let balanced = PipelineOptions::for_resources(host, TransferWorkload::Balanced);
    let gameplay = PipelineOptions::for_resources(host, TransferWorkload::GameplaySafe);
    assert!(balanced.max_parallel_packs < throughput.max_parallel_packs);
    assert_eq!(gameplay.max_parallel_packs, throughput.max_parallel_packs);
    assert_eq!(
        gameplay.reconstruction_workers,
        throughput.reconstruction_workers
    );
    assert_eq!(gameplay.buffered_chunks, throughput.buffered_chunks);
    assert_eq!(host.cpu_workers(TransferWorkload::Balanced, 1), 64);
    assert_eq!(host.cpu_workers(TransferWorkload::GameplaySafe, 1), 32);
    assert_shared_admission(balanced, host, TransferWorkload::Balanced);
    assert_shared_admission(gameplay, host, TransferWorkload::Throughput);

    let pressured = resources(128, Some(64 * PACK_MAX), true);
    let pressured_gameplay =
        PipelineOptions::for_resources(pressured, TransferWorkload::GameplaySafe);
    let pressured_throughput =
        PipelineOptions::for_resources(pressured, TransferWorkload::Throughput);
    assert_eq!(
        pressured_gameplay.max_parallel_packs,
        pressured_throughput.max_parallel_packs
    );
    assert_eq!(
        pressured_gameplay.reconstruction_workers,
        pressured_throughput.reconstruction_workers
    );
    assert_eq!(
        pressured_gameplay.buffered_chunks,
        pressured_throughput.buffered_chunks
    );
    assert!(pressured_gameplay.max_parallel_packs < gameplay.max_parallel_packs);
    assert!(pressured_gameplay.reconstruction_workers < gameplay.reconstruction_workers);
    assert!(pressured_gameplay.reconstruction_workers > 1);
    assert_shared_admission(pressured_gameplay, pressured, TransferWorkload::Throughput);
}

#[test]
fn explicit_restore_options_are_upper_bounds_and_outlandish_counts_are_clamped() {
    let host = resources(64, Some(32 * PACK_MAX), false);
    let explicit = PipelineOptions {
        workload: TransferWorkload::Throughput,
        max_parallel_packs: 3,
        reconstruction_workers: 2,
        buffered_chunks: 1,
        max_inflight_pack_bytes: PACK_MAX,
        overlap_file_syncs: false,
    };
    let resolved = explicit.admit(host, true);
    assert_eq!(resolved.max_parallel_packs, 3);
    assert_eq!(resolved.reconstruction_workers, 2);
    assert_eq!(resolved.buffered_chunks, 1);
    assert!(!resolved.overlap_file_syncs);
    assert_shared_admission(resolved, host, TransferWorkload::Throughput);

    let outrageous = PipelineOptions {
        workload: TransferWorkload::Throughput,
        max_parallel_packs: usize::MAX,
        reconstruction_workers: usize::MAX,
        buffered_chunks: usize::MAX,
        max_inflight_pack_bytes: u64::MAX,
        overlap_file_syncs: true,
    };
    let clamped = outrageous.admit(host, true);
    assert!(clamped.max_parallel_packs < usize::MAX);
    assert!(clamped.reconstruction_workers <= PUBLICATION_BATCH);
    assert!(clamped.buffered_chunks <= clamped.max_parallel_packs);
    assert_eq!(
        clamped.max_inflight_pack_bytes,
        clamped.max_parallel_packs as u64 * DECODER_RECORD_BYTES
    );
    assert_shared_admission(clamped, host, TransferWorkload::Throughput);
}

#[test]
fn nonstreaming_scratch_and_streaming_records_have_independent_weights() {
    let host = resources(64, Some(32 * PACK_MAX), false);
    let explicit = PipelineOptions {
        workload: TransferWorkload::Throughput,
        max_parallel_packs: 16,
        reconstruction_workers: 16,
        buffered_chunks: 16,
        max_inflight_pack_bytes: 2 * PACK_MAX,
        overlap_file_syncs: true,
    };
    let scratch = explicit.admit(host, false);
    let streamed = explicit.admit(host, true);
    assert_eq!(scratch.max_parallel_packs, 2);
    assert!(streamed.max_parallel_packs > scratch.max_parallel_packs);
    assert!(scratch.max_parallel_packs as u64 * PACK_MAX <= scratch.max_inflight_pack_bytes);
    assert!(
        streamed.max_parallel_packs as u64 * DECODER_RECORD_BYTES
            <= streamed.max_inflight_pack_bytes
    );
    assert_shared_admission(scratch, host, TransferWorkload::Throughput);
    assert_shared_admission(streamed, host, TransferWorkload::Throughput);
}

#[test]
fn tiny_hosts_keep_only_the_minimum_bounded_pipeline_and_one_queued_chunk() {
    for host in [
        resources(1, None, false),
        resources(128, Some(1), false),
        resources(0, Some(0), true),
    ] {
        let options = PipelineOptions::for_resources(host, TransferWorkload::GameplaySafe);
        assert_eq!(options.max_parallel_packs, 1);
        assert_eq!(options.reconstruction_workers, 1);
        assert_eq!(options.buffered_chunks, 1);
    }
}

#[tokio::test]
async fn weighted_admission_never_bypasses_the_byte_budget_for_a_single_job() {
    let started = std::sync::atomic::AtomicUsize::new(0);
    let error = run_weighted(vec![((), PACK_MAX)], 8, PACK_MAX - 1, |_| async {
        started.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("weighted restore admission"));
    assert_eq!(started.load(Ordering::Relaxed), 0);
}

/// No private limiter: restore execution must honor this wrapper's advertised
/// ceiling itself. The non-streaming implementation uses the trait's real
/// scratch-file compatibility fallback.
struct CeilingProvider {
    local: LocalStorage,
    tuning: Option<TransferTuning>,
    first_group: tokio::sync::Barrier,
    started: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
}

struct ActiveDownload<'a>(&'a AtomicUsize);

impl Drop for ActiveDownload<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl CeilingProvider {
    fn new(local: LocalStorage, tuning: Option<TransferTuning>) -> Self {
        let ceiling = tuning
            .as_ref()
            .cloned()
            .unwrap_or_default()
            .max_parallel_downloads
            .max(1);
        Self {
            local,
            tuning,
            first_group: tokio::sync::Barrier::new(ceiling),
            started: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    async fn begin(&self) -> Result<ActiveDownload<'_>> {
        let guard = ActiveDownload(&self.active);
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(active, Ordering::AcqRel);
        let order = self.started.fetch_add(1, Ordering::AcqRel);
        if order < self.transfer_tuning().max_parallel_downloads.max(1) {
            tokio::time::timeout(Duration::from_secs(10), self.first_group.wait())
                .await
                .map_err(|_| StateError::Storage("provider slots were serialized".into()))?;
        }
        Ok(guard)
    }
}

#[async_trait::async_trait]
impl SharedStorageProvider for CeilingProvider {
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
        let _active = self.begin().await?;
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
    fn transfer_tuning(&self) -> TransferTuning {
        self.tuning.clone().unwrap_or_default()
    }
}

/// A typed streaming wrapper forwards the same raw budget, without multiplying
/// it by preferred copyto width or relying on an underlying backend limiter.
struct StreamingCeiling<'a>(&'a CeilingProvider);

#[async_trait::async_trait]
impl SharedStorageProvider for StreamingCeiling<'_> {
    async fn health_check(&self) -> Result<Health> {
        self.0.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.0.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.0.stat(key).await
    }
    async fn upload_immutable(&self, path: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.0.upload_immutable(path, key).await
    }
    async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
        self.0.download(key, path).await
    }
    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.0.list_prefix(prefix).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &RemoteKey,
    ) -> Result<RemoteMeta> {
        self.0.put_small_versioned(bytes, key).await
    }
    fn transfer_tuning(&self) -> TransferTuning {
        self.0.transfer_tuning()
    }
    fn supports_streaming_downloads(&self) -> bool {
        true
    }
    async fn download_stream_observed(
        &self,
        key: &RemoteKey,
        scratch: &Path,
        consumer: &dyn DownloadStreamConsumer,
        observer: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        let _active = self.0.begin().await?;
        self.0
            .local
            .download_stream_observed(key, scratch, consumer, observer)
            .await
    }
}

#[test]
fn final_admission_retains_workload_when_io_pressure_arrives_after_planning() {
    let initial = resources(128, Some(64 * PACK_MAX), false);
    let pressured = resources(128, Some(64 * PACK_MAX), true);
    let provider = CeilingProvider::new(
        LocalStorage::new("unused-resource-policy"),
        Some(TransferTuning {
            max_parallel_downloads: 32,
            ..TransferTuning::default()
        }),
    );
    for workload in [
        TransferWorkload::Throughput,
        TransferWorkload::Balanced,
        TransferWorkload::GameplaySafe,
    ] {
        let active_workload = workload.for_active_transfer();
        let requested = PipelineOptions::for_resources(initial, workload);
        let (effective, io) = requested.resolve(pressured, &provider).unwrap();
        assert_eq!(effective.workload, active_workload);
        assert_eq!(io.workload, format!("{active_workload:?}"));
        assert!(io.io_pressure);
        assert_eq!(
            io.cpu_worker_budget,
            pressured.cpu_workers(active_workload, 1) as u64
        );
        assert!(
            effective.max_parallel_packs + effective.reconstruction_workers
                < pressured.cpu_workers(active_workload, 1)
        );
        assert!(effective.max_parallel_packs <= requested.max_parallel_packs);
        assert!(effective.reconstruction_workers <= requested.reconstruction_workers);
        assert_eq!(
            io.requested_parallel_packs,
            requested.max_parallel_packs as u64
        );
        assert_eq!(
            io.effective_parallel_packs,
            effective.max_parallel_packs as u64
        );
        assert!(!io.minimum_worker_floor);
        assert_shared_admission(effective, pressured, active_workload);
    }
}

#[test]
fn thirty_cpu_active_gameplay_uses_full_cpu_budget_but_keeps_provider_and_requested_caps() {
    let host = resources(30, Some(20 * 1024 * 1024 * 1024), false);
    assert_eq!(host.cpu_workers(TransferWorkload::GameplaySafe, 1), 8);
    assert_eq!(host.cpu_workers(TransferWorkload::Balanced, 1), 15);
    for pressure in [false, true] {
        let host = WorkerResources {
            io_pressure: pressure,
            ..host
        };
        for ceiling in [8, 16, 32] {
            let tuning = TransferTuning {
                max_parallel_downloads: ceiling,
                ..TransferTuning::default()
            };
            let gameplay =
                PipelineOptions::for_transfer_tuning(host, TransferWorkload::GameplaySafe, &tuning);
            let throughput =
                PipelineOptions::for_transfer_tuning(host, TransferWorkload::Throughput, &tuning);
            assert_eq!(gameplay.max_parallel_packs, throughput.max_parallel_packs);
            assert_eq!(
                gameplay.reconstruction_workers,
                throughput.reconstruction_workers
            );
            let provider =
                CeilingProvider::new(LocalStorage::new("unused-active-gameplay"), Some(tuning));
            // Saved/manual options must not re-enable GameplaySafe during final admission.
            let raw = PipelineOptions {
                workload: TransferWorkload::GameplaySafe,
                ..gameplay
            };
            let (effective, io) = raw.resolve(host, &StreamingCeiling(&provider)).unwrap();
            assert_eq!(io.workload, "Throughput");
            assert_eq!(io.cpu_worker_budget, if pressure { 15 } else { 30 });
            assert_eq!(io.provider_download_slots, ceiling as u64);
            assert!(effective.max_parallel_packs <= ceiling);
            assert!(effective.max_parallel_packs <= raw.max_parallel_packs);
            assert!(effective.reconstruction_workers <= raw.reconstruction_workers);
            assert!(effective.buffered_chunks <= raw.buffered_chunks);
            assert!(effective.max_inflight_pack_bytes <= raw.max_inflight_pack_bytes);
            assert!(effective.max_parallel_packs > 4);
            assert_shared_admission(effective, host, TransferWorkload::Throughput);

            let frozen = PipelineOptions {
                max_parallel_packs: 2,
                reconstruction_workers: 2,
                buffered_chunks: 1,
                ..raw
            };
            let (effective, _) = frozen.resolve(host, &StreamingCeiling(&provider)).unwrap();
            assert_eq!(effective.max_parallel_packs, 2);
            assert_eq!(effective.reconstruction_workers, 2);
            assert_eq!(effective.buffered_chunks, 1);
        }
    }
    assert_eq!(host.cpu_workers(TransferWorkload::GameplaySafe, 1), 8);
}

#[test]
fn final_diagnostics_report_the_tiny_active_transfer_floor_against_its_real_cpu_budget() {
    let host = resources(1, Some(16 * PACK_MAX), false);
    let requested = PipelineOptions::for_resources(host, TransferWorkload::GameplaySafe);
    let provider = LocalStorage::new("unused-gameplay-floor");
    let (effective, io) = requested.resolve(host, &provider).unwrap();
    assert_eq!(io.workload, "Throughput");
    assert_eq!(io.cpu_worker_budget, 1);
    assert!(io.minimum_worker_floor);
    assert_eq!(effective.max_parallel_packs, 1);
    assert_eq!(effective.reconstruction_workers, 1);
    assert_eq!(effective.buffered_chunks, 1);
    assert_eq!(io.provider_download_slots, 4);
}

#[test]
fn execution_resolution_keeps_sixteen_and_thirty_two_raw_provider_slots() {
    let host = resources(128, Some(64 * PACK_MAX), false);
    for (ceiling, inner_width) in [(16, 4), (32, 5)] {
        let provider = CeilingProvider::new(
            LocalStorage::new("unused-raw-provider-policy"),
            Some(TransferTuning {
                max_parallel_downloads: ceiling,
                rclone_download_streams: inner_width,
                ..TransferTuning::default()
            }),
        );
        let requested = PipelineOptions::for_resources(host, TransferWorkload::Throughput);
        let (effective, io) = requested
            .resolve(host, &StreamingCeiling(&provider))
            .unwrap();
        assert_eq!(effective.max_parallel_packs, ceiling);
        assert_eq!(io.provider_download_slots, ceiling as u64);
        assert_shared_admission(effective, host, TransferWorkload::Throughput);
    }
}

fn restore_fixture(
    root: &Path,
    count: usize,
) -> (
    MasterKey,
    RestorePlan,
    Vec<PackIndexEntry>,
    LogicalRootMap,
    Vec<Vec<u8>>,
    LocalStorage,
) {
    let master = MasterKey::generate();
    let remote = root.join("remote");
    let payloads = (0..count)
        .map(|index| format!("authenticated resource fixture {index}").into_bytes())
        .collect::<Vec<_>>();
    let mut index = Vec::new();
    for (number, payload) in payloads.iter().enumerate() {
        let pack = noland_pack::pack_chunks(
            &root.join(format!("built-{number}")),
            &master,
            vec![(noland_cas::blake3_hex(payload), payload.clone())],
            |_| false,
        )
        .unwrap()
        .remove(0);
        let destination = remote.join(pack_key(&pack.pack_id));
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(pack.path, destination).unwrap();
        index.extend(pack.entries);
    }
    let files = payloads
        .iter()
        .enumerate()
        .map(|(index, payload)| {
            test_file(
                &format!("game/{index}"),
                PersistenceClass::PersistentState,
                SemanticRole::UserState,
                payload,
            )
        })
        .collect();
    let plan = test_plan(&root.join("restore"), test_manifest(files));
    (
        master,
        plan,
        index,
        LogicalRootMap::from_home(root.join("home")),
        payloads,
        LocalStorage::new(remote),
    )
}

#[tokio::test]
async fn execution_honors_default_and_explicit_raw_provider_slots_through_both_wrapper_modes() {
    for tuning in [
        None,
        Some(TransferTuning {
            max_parallel_downloads: 2,
            rclone_download_streams: 2,
            ..TransferTuning::default()
        }),
        Some(TransferTuning {
            max_parallel_downloads: 5,
            rclone_download_streams: 3,
            ..TransferTuning::default()
        }),
    ] {
        for streaming in [false, true] {
            let root = test_dir("execution-provider-ceiling");
            let ceiling = tuning.clone().unwrap_or_default().max_parallel_downloads;
            let (master, plan, index, roots, payloads, local) = restore_fixture(&root, ceiling + 1);
            let wrapped = CeilingProvider::new(local, tuning.clone());
            let streamed = StreamingCeiling(&wrapped);
            let provider: &dyn SharedStorageProvider = if streaming { &streamed } else { &wrapped };
            let requested = PipelineOptions {
                workload: TransferWorkload::Throughput,
                max_parallel_packs: 8,
                reconstruction_workers: 2,
                buffered_chunks: 8,
                max_inflight_pack_bytes: 8 * PACK_MAX,
                overlap_file_syncs: true,
            };
            let report = restore_pipelined_for_resources(
                provider,
                &master,
                &plan,
                &index,
                &roots,
                None,
                None,
                requested,
                resources(64, Some(32 * PACK_MAX), false),
                |sample| {
                    assert_eq!(sample.io.provider_download_slots, ceiling as u64);
                    assert_eq!(sample.io.requested_parallel_packs, 8);
                    assert_eq!(sample.io.effective_parallel_packs, ceiling as u64);
                    assert_eq!(sample.io.requested_inflight_pack_bytes, 8 * PACK_MAX);
                    assert_eq!(
                        sample.io.max_inflight_pack_bytes,
                        ceiling as u64
                            * if streaming {
                                DECODER_RECORD_BYTES
                            } else {
                                PACK_MAX
                            }
                    );
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert_eq!(wrapped.peak.load(Ordering::Acquire), ceiling);
            assert_eq!(wrapped.active.load(Ordering::Acquire), 0);
            assert_eq!(wrapped.started.load(Ordering::Acquire), payloads.len());
            assert_eq!(report.restore.published_entries, payloads.len());
            assert_eq!(report.io.effective_verification_workers, 2);
            assert_eq!(report.io.verification_pool_workers, 2);
            for (index, payload) in payloads.iter().enumerate() {
                assert_eq!(
                    fs::read(root.join(format!("home/.local/share/game/{index}"))).unwrap(),
                    *payload
                );
            }
            assert!(!plan.staging.exists());
            assert!(!plan.pack_cache.exists());
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[tokio::test]
async fn restore_success_failure_and_cancellation_leave_normal_gameplay_policy_unchanged() {
    let host = resources(64, Some(32 * PACK_MAX), false);
    for outcome in ["success", "failure", "cancellation"] {
        let root = test_dir("operation-local-gameplay-policy");
        let (master, plan, index, roots, _, storage) = restore_fixture(&root, 1);
        let baseline = TransferWorkload::GameplaySafe;
        let baseline_workers = host.cpu_workers(baseline, 1);
        assert_eq!(baseline_workers, 16);
        let requested = PipelineOptions::for_resources(host, baseline);
        let mut saw_active_policy = false;
        let result = restore_pipelined_for_resources(
            &storage,
            &master,
            &plan,
            &index,
            &roots,
            None,
            None,
            requested,
            host,
            |sample| {
                saw_active_policy = true;
                assert_eq!(sample.io.workload, "Throughput");
                assert_eq!(sample.io.cpu_worker_budget, 64);
                assert_eq!(host.cpu_workers(baseline, 1), baseline_workers);
                match outcome {
                    "failure" => Err(StateError::Storage("injected progress failure".into())),
                    "cancellation" => Err(StateError::Storage("cancel requested by caller".into())),
                    _ => Ok(()),
                }
            },
        )
        .await;
        assert!(saw_active_policy);
        if outcome == "success" {
            assert_eq!(result.unwrap().restore.published_entries, 1);
        } else {
            let error = result.unwrap_err().to_string();
            assert!(error.contains(if outcome == "failure" {
                "injected"
            } else {
                "cancel"
            }));
        }
        assert_eq!(baseline, TransferWorkload::GameplaySafe);
        assert_eq!(host.cpu_workers(baseline, 1), baseline_workers);
        assert!(!plan.staging.exists());
        assert!(!plan.pack_cache.exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn tiny_host_floor_finishes_and_cancels_with_one_queued_chunk_in_both_overlap_modes() {
    for host in [resources(1, None, false), resources(128, Some(0), true)] {
        for overlap_file_syncs in [false, true] {
            for cancel in [false, true] {
                let root = test_dir("tiny-resource-pipeline");
                let (master, plan, index, roots, payloads, storage) = restore_fixture(&root, 3);
                let game = root.join("home/.local/share/game");
                fs::create_dir_all(&game).unwrap();
                for index in 0..payloads.len() {
                    fs::write(game.join(index.to_string()), format!("original-{index}")).unwrap();
                }
                fs::write(game.join("unrelated"), b"preserve-unrelated").unwrap();
                let requested = PipelineOptions {
                    overlap_file_syncs,
                    ..PipelineOptions::for_resources(host, TransferWorkload::GameplaySafe)
                };
                let mut cancelled_at_publication = false;
                let result = tokio::time::timeout(
                    Duration::from_secs(10),
                    restore_pipelined_for_resources(
                        &storage,
                        &master,
                        &plan,
                        &index,
                        &roots,
                        None,
                        None,
                        requested,
                        host,
                        |sample| {
                            assert_eq!(sample.io.workload, "Throughput");
                            assert_eq!(sample.io.available_memory_bytes, host.available_memory);
                            assert_eq!(sample.io.cpu_worker_budget, 1);
                            assert!(sample.io.minimum_worker_floor);
                            assert_eq!(sample.io.effective_parallel_packs, 1);
                            assert_eq!(sample.io.effective_verification_workers, 1);
                            assert_eq!(sample.io.effective_buffered_chunks, 1);
                            assert_eq!(sample.io.overlap_file_syncs, overlap_file_syncs);
                            if sample.phase != "checking_existing_files" {
                                assert_eq!(sample.io.verification_pool_workers, 1);
                            }
                            if cancel && sample.completed_entries > 0 {
                                cancelled_at_publication = true;
                                return Err(StateError::Storage(
                                    "cancel tiny restore after publication".into(),
                                ));
                            }
                            Ok(())
                        },
                    ),
                )
                .await
                .expect("minimum bounded pipeline deadlocked");
                if cancel {
                    let error = result.unwrap_err();
                    assert!(cancelled_at_publication);
                    assert!(error.to_string().contains("cancel"));
                } else {
                    let report = result.unwrap();
                    assert_eq!(report.restore.published_entries, payloads.len());
                    assert_eq!(report.io.file_syncs, payloads.len() as u64);
                    assert!(report.io.minimum_worker_floor);
                }
                for (index, payload) in payloads.iter().enumerate() {
                    let destination = game.join(index.to_string());
                    let expected = if cancel {
                        format!("original-{index}").into_bytes()
                    } else {
                        payload.clone()
                    };
                    assert_eq!(fs::read(&destination).unwrap(), expected);
                    assert!(!destination_partial_path(&destination, plan.restore_id)
                        .unwrap()
                        .exists());
                }
                assert_eq!(
                    fs::read(game.join("unrelated")).unwrap(),
                    b"preserve-unrelated"
                );
                assert!(
                    !plan.staging.exists(),
                    "rollback/commit must finish before return"
                );
                assert!(!plan.pack_cache.exists());
                fs::remove_dir_all(root).unwrap();
            }
        }
    }
}

#[test]
fn production_constructor_only_spawns_verifiers_for_eligible_nonidentical_files() {
    for (count, identical) in [(0, 0), (1, 0), (3, 0), (3, 2), (3, 3)] {
        let root = test_dir("eligible-verification-pool");
        let (_master, plan, _index, roots, payloads, provider) = restore_fixture(&root, count);
        let game = root.join("home/.local/share/game");
        fs::create_dir_all(&game).unwrap();
        for (index, payload) in payloads.iter().enumerate().take(identical) {
            fs::write(game.join(index.to_string()), payload).unwrap();
        }
        let host = resources(128, Some(64 * PACK_MAX), false);
        let requested = PipelineOptions {
            overlap_file_syncs: true,
            ..PipelineOptions::for_resources(host, TransferWorkload::Throughput)
        };
        let (effective, io) = requested.resolve(host, &provider).unwrap();
        assert!(effective.reconstruction_workers > 16);
        let expected_pool = (count - identical).max(1);
        let constructed = AtomicUsize::new(0);
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::with_verification_factory(
            &mut transaction,
            effective.reconstruction_workers,
            io,
            |workers| {
                constructed.store(workers, Ordering::Release);
                VerificationPool::new(workers)
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(constructed.load(Ordering::Acquire), expected_pool);
        for payload in payloads.iter().skip(identical) {
            pipeline
                .consume(&noland_cas::blake3_hex(payload), payload)
                .unwrap();
        }
        let report = pipeline.finish().unwrap();
        assert_eq!(report.io.verification_pool_workers, expected_pool as u64);
        assert_eq!(report.io.file_syncs, (count - identical) as u64);
        assert_eq!(report.restore.reused_entries, identical);
        assert_eq!(report.restore.published_entries, count - identical);
        transaction.commit().unwrap();
        for (index, payload) in payloads.iter().enumerate() {
            assert_eq!(fs::read(game.join(index.to_string())).unwrap(), *payload);
        }
        fs::remove_dir_all(root).unwrap();
    }
}
