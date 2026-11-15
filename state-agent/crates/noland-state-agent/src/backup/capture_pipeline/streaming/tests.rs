use super::super::tests::admission;
use super::*;
use async_trait::async_trait;
use noland_storage::{
    Health, ImmutableUpload, ImmutableUploadCompletion, ImmutableUploadObserver, RemoteEntry,
    RemoteKey, RemoteMeta,
};
use std::io::Write;

mod small_files;

#[derive(Debug)]
enum Event {
    Capture(usize),
    SourceRead(usize),
    Codec(usize),
    Sealed(usize),
    Persisting(usize),
    PlanDurable(usize),
    Upload(usize),
    Group(usize, usize),
    Durable(String),
}

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: std::sync::Condvar,
}
impl Gate {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self, stop: &AtomicBool) -> Result<()> {
        self.wait_cancelled(&|| stop.load(Ordering::Acquire))
    }

    fn wait_cancelled(&self, cancelled: &dyn Fn() -> bool) -> Result<()> {
        let mut open = self.open.lock().unwrap();
        while !*open && !cancelled() {
            // Only a cancellation check, never a performance assertion.
            open = self
                .changed
                .wait_timeout(open, Duration::from_millis(10))
                .unwrap()
                .0;
        }
        if cancelled() {
            return Err(cancellation_error());
        }
        Ok(())
    }
}

pub(super) struct Hooks {
    events: tokio::sync::mpsc::UnboundedSender<Event>,
    codecs: HashMap<usize, Arc<Gate>>,
    plans: HashMap<usize, Arc<Gate>>,
    after_plans: HashMap<usize, Arc<Gate>>,
    captures: HashMap<usize, Arc<Gate>>,
    sources: HashMap<usize, Arc<Gate>>,
}
impl Hooks {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }

    pub(super) fn before_capture(&self, index: usize, stop: &AtomicBool) -> Result<()> {
        self.send(Event::Capture(index));
        if let Some(gate) = self.captures.get(&index) {
            gate.wait(stop)?;
        }
        Ok(())
    }

    pub(super) fn after_source(&self, index: usize, cancelled: &dyn Fn() -> bool) -> Result<()> {
        self.send(Event::SourceRead(index));
        if let Some(gate) = self.sources.get(&index) {
            gate.wait_cancelled(cancelled)?;
        }
        Ok(())
    }

    pub(super) fn before_codec(&self, index: usize, stop: &AtomicBool) -> Result<()> {
        self.send(Event::Codec(index));
        if let Some(gate) = self.codecs.get(&index) {
            gate.wait(stop)?;
        }
        Ok(())
    }

    pub(super) fn sealed(&self, index: usize) {
        self.send(Event::Sealed(index));
    }

    pub(super) fn before_plan(&self, index: usize, stop: &AtomicBool) -> Result<()> {
        self.send(Event::Persisting(index));
        if let Some(gate) = self.plans.get(&index) {
            gate.wait(stop)?;
        }
        Ok(())
    }

    pub(super) fn after_plan(&self, index: usize, stop: &AtomicBool) -> Result<()> {
        self.send(Event::PlanDurable(index));
        if let Some(gate) = self.after_plans.get(&index) {
            gate.wait(stop)?;
        }
        Ok(())
    }
}

struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Provider {
    inner: LocalStorage,
    calls: AtomicUsize,
    remote_calls: AtomicUsize,
    fail: AtomicBool,
    slow_first: bool,
    release: tokio::sync::Semaphore,
    active: AtomicUsize,
    peak: AtomicUsize,
    uploaded: Mutex<Vec<(String, String)>>,
    tuning: TransferTuning,
    hooks: Option<Arc<Hooks>>,
    hold_uploads: bool,
    parallel_groups: bool,
    hold_first_groups: bool,
    group_calls: AtomicUsize,
    active_groups: AtomicUsize,
    group_release: tokio::sync::Semaphore,
}
#[async_trait]
impl SharedStorageProvider for Provider {
    async fn health_check(&self) -> Result<Health> {
        self.remote_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.remote_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.remote_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.stat(key).await
    }
    async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
        self.remote_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.download(key, path).await
    }
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.remote_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.list_prefix(key).await
    }
    async fn put_small_versioned(&self, _: bytes::Bytes, _: &RemoteKey) -> Result<RemoteMeta> {
        panic!("stream capture cannot publish a manifest")
    }
    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        self.inner.storage_identity()
    }
    fn supports_targeted_uploads(&self) -> bool {
        true
    }
    fn transfer_tuning(&self) -> TransferTuning {
        self.tuning.clone()
    }
    fn supports_parallel_upload_groups(&self) -> bool {
        self.parallel_groups
    }
    async fn upload_immutable_bulk_observed(
        &self,
        uploads: &[ImmutableUpload],
        observer: Option<&(dyn ImmutableUploadObserver + '_)>,
    ) -> Result<Vec<RemoteMeta>> {
        self.active_groups.fetch_add(1, Ordering::SeqCst);
        let _active = Active(&self.active_groups);
        let group = self.group_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(hooks) = &self.hooks {
            hooks.send(Event::Group(group, uploads.len()));
        }
        let mut metas = Vec::new();
        for (index, upload) in uploads.iter().enumerate() {
            if index == 1 && self.hold_first_groups && group <= 2 {
                self.group_release.acquire().await.unwrap().forget();
            }
            if let Some(observer) = observer {
                observer.transfer_started()?;
            }
            metas.push(self.upload_immutable(&upload.local, &upload.key).await?);
            if let Some(observer) = observer {
                observer.completed(
                    std::slice::from_ref(upload),
                    ImmutableUploadCompletion::Uploaded,
                )?;
            }
            if let Some(hooks) = &self.hooks {
                hooks.send(Event::Durable(upload.key.as_str().to_owned()));
            }
        }
        Ok(metas)
    }
    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.remote_calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.uploaded
            .lock()
            .unwrap()
            .push((key.as_str().into(), noland_cas::blake3_file(local)?));
        if self.fail.load(Ordering::SeqCst) {
            return Err(StateError::Storage("injected transfer interruption".into()));
        }
        if let Some(hooks) = &self.hooks {
            hooks.send(Event::Upload(call));
        }
        if self.hold_uploads {
            self.release.acquire().await.unwrap().forget();
        } else if self.slow_first && call == 1 {
            tokio::time::timeout(Duration::from_secs(20), self.release.acquire())
                .await
                .map_err(|_| StateError::Storage("straggler prevented slot refill".into()))?
                .unwrap()
                .forget();
        } else if call >= 3 {
            self.release.add_permits(1);
        }
        self.inner.upload_immutable(local, key).await
    }
}

fn fixture(
    files: usize,
    length: u64,
) -> (
    PathBuf,
    StateAgent,
    MasterKey,
    Uuid,
    PreparedBackup,
    Provider,
) {
    let (root, agent, master, operation, mut prepared) = super::super::tests::fixture(files);
    let jobs = prepared
        .capture
        .take()
        .unwrap()
        .jobs
        .into_iter()
        .map(|(job, _)| job)
        .collect::<Vec<_>>();
    for (i, job) in jobs.iter().enumerate() {
        let mut file = std::fs::File::create(&job.source).unwrap();
        // Distinct long runs produce bounded deterministic chunks and cheap
        // ciphertext. The sparse tail makes the pre-EOF fixture genuinely >2 GiB.
        let mut remaining = length.min(192 << 20);
        let mut block = 0u8;
        while remaining > 0 {
            let n = remaining.min(8 << 20) as usize;
            file.write_all(&vec![block.wrapping_add(i as u8); n])
                .unwrap();
            block = block.wrapping_add(1);
            remaining -= n as u64;
        }
        file.set_len(length).unwrap();
    }
    prepared.version = 3;
    prepared.capture = Some(
        State::new_streaming(
            jobs,
            None,
            preparation::Budget {
                hashing: 2,
                packing: 1,
            },
        )
        .unwrap(),
    );
    write_prepared_backup(&agent, &master, operation, &prepared).unwrap();
    let provider = Provider {
        inner: LocalStorage::new(root.join("remote")),
        calls: 0.into(),
        remote_calls: 0.into(),
        fail: false.into(),
        slow_first: false,
        release: tokio::sync::Semaphore::new(0),
        active: 0.into(),
        peak: 0.into(),
        uploaded: Mutex::default(),
        tuning: TransferTuning {
            max_parallel_uploads: 2,
            max_bulk_files: 2,
            max_bulk_bytes: 20 << 20,
            max_attempts: 1,
            ..Default::default()
        },
        hooks: None,
        hold_uploads: false,
        parallel_groups: false,
        hold_first_groups: false,
        group_calls: 0.into(),
        active_groups: 0.into(),
        group_release: tokio::sync::Semaphore::new(0),
    };
    (root, agent, master, operation, prepared, provider)
}

fn resume(
    agent: &StateAgent,
    provider: &Provider,
    master: &MasterKey,
    operation: Uuid,
    prepared: &PreparedBackup,
) -> PreparedBackup {
    read_prepared_backup(
        agent,
        provider,
        master,
        operation,
        &prepared.manifest.app.app_id,
        prepared.manifest.mode,
    )
    .unwrap()
    .unwrap()
}

fn incompressible_fixture(
    files: usize,
    length: u64,
) -> (
    PathBuf,
    StateAgent,
    MasterKey,
    Uuid,
    PreparedBackup,
    Provider,
) {
    let (root, agent, master, operation, mut prepared, provider) = fixture(files, 0);
    let jobs = prepared
        .capture
        .take()
        .unwrap()
        .jobs
        .into_iter()
        .map(|(job, _)| job)
        .collect::<Vec<_>>();
    for (index, job) in jobs.iter().enumerate() {
        let mut file = std::fs::File::create(&job.source).unwrap();
        let mut hasher = blake3::Hasher::new();
        hasher.update(format!("streaming-incompressible-fixture:{index}").as_bytes());
        let mut random = hasher.finalize_xof();
        let mut block = vec![0; 1024 * 1024];
        let mut remaining = length;
        while remaining > 0 {
            let n = remaining.min(block.len() as u64) as usize;
            random.fill(&mut block[..n]);
            file.write_all(&block[..n]).unwrap();
            remaining -= n as u64;
        }
    }
    prepared.capture = Some(
        State::new_streaming(
            jobs,
            None,
            preparation::Budget {
                hashing: 2,
                packing: 1,
            },
        )
        .unwrap(),
    );
    write_prepared_backup(&agent, &master, operation, &prepared).unwrap();
    (root, agent, master, operation, prepared, provider)
}

/// Make genuine old-format frozen receipts without relying on runtime races to
/// accumulate a ready group. Inputs and ciphertext remain authenticated exactly
/// as on a retry; only the first `seal` plans have already been compressed.
fn freeze_plans(
    agent: &StateAgent,
    master: &MasterKey,
    operation: Uuid,
    prepared: &PreparedBackup,
    seal: usize,
) -> Vec<BuiltPack> {
    freeze_plans_inner(agent, master, operation, prepared, seal, false)
}

fn freeze_plans_inner(
    agent: &StateAgent,
    master: &MasterKey,
    operation: Uuid,
    prepared: &PreparedBackup,
    seal: usize,
    whole_files: bool,
) -> Vec<BuiltPack> {
    let state = prepared.capture.as_ref().unwrap();
    let directory = prepared_backup_path(agent, operation).with_extension("stream");
    let output = agent
        .config
        .paths
        .packs
        .join(prepared.manifest.bundle_id.to_string());
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                for work in 0..state.ranges.len() {
                    capture_work(
                        &agent.config.paths.cache.join("cas/chunks"),
                        &directory,
                        operation,
                        work,
                        state,
                        master,
                        &Counters::default(),
                        &|| false,
                        &sender,
                    )
                    .unwrap();
                }
            })
            .join()
            .unwrap();
    });
    drop(sender);
    let mut chunks = Vec::new();
    while let Ok(receipt) = receiver.try_recv() {
        chunks.extend(receipt.unwrap().chunks);
    }
    let groups: Vec<Vec<CapturedChunk>> = if whole_files {
        let mut files = std::collections::BTreeMap::<usize, Vec<CapturedChunk>>::new();
        for chunk in chunks {
            files.entry(chunk.file).or_default().push(chunk);
        }
        files.into_values().collect()
    } else {
        chunks.into_iter().map(|chunk| vec![chunk]).collect()
    };
    let mut frozen = Vec::new();
    for (index, chunks) in groups.into_iter().enumerate() {
        let plan = Plan { index, chunks };
        write_batch(
            &directory.join("plans").join(format!("{index}.enc")),
            &format!("backup-stream:{operation}:plan:{index}"),
            master,
            &plan,
        )
        .unwrap();
        if index < seal {
            let mut packs = pack_chunk_files_with_limits(
                &output,
                master,
                plan.chunks
                    .iter()
                    .map(|chunk| (chunk.hash.clone(), chunk.source.clone())),
                |_| false,
                constants::PACK_MAX,
                constants::PACK_MAX,
            )
            .unwrap();
            assert_eq!(packs.len(), 1);
            let pack = packs.pop().unwrap();
            packing::write_receipt(
                &directory.join("sealed").join(format!("{index}.enc")),
                master,
                &format!("backup-stream:{operation}:sealed:{index}"),
                &pack,
            )
            .unwrap();
            frozen.push(pack);
        }
    }
    frozen
}

fn hooks(
    codecs: HashMap<usize, Arc<Gate>>,
    plans: HashMap<usize, Arc<Gate>>,
) -> (Arc<Hooks>, tokio::sync::mpsc::UnboundedReceiver<Event>) {
    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    (
        Arc::new(Hooks {
            events,
            codecs,
            plans,
            after_plans: HashMap::new(),
            captures: HashMap::new(),
            sources: HashMap::new(),
        }),
        receiver,
    )
}

fn counters(hooks: Arc<Hooks>) -> Arc<Counters> {
    Arc::new(Counters {
        hooks: Some(hooks),
        ..Default::default()
    })
}

fn receipt_snapshot(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, receipts: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, receipts);
            } else if path.extension().is_some_and(|extension| extension == "enc") {
                receipts.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut receipts = BTreeMap::new();
    visit(directory, directory, &mut receipts);
    receipts
}

fn expected_sources(prepared: &PreparedBackup) -> BTreeMap<String, Vec<u8>> {
    prepared
        .capture
        .as_ref()
        .unwrap()
        .jobs
        .iter()
        .map(|(job, _)| {
            (
                job.logical.relative_path.clone(),
                std::fs::read(&job.source).unwrap(),
            )
        })
        .collect()
}

fn assert_restored_sources(
    prepared: &PreparedBackup,
    master: &MasterKey,
    remote: &Path,
    expected: &BTreeMap<String, Vec<u8>>,
) {
    assert_eq!(prepared.manifest.files.len(), expected.len());
    for file in &prepared.manifest.files {
        let mut restored = Vec::new();
        for chunk in &file.chunks {
            let entry = prepared
                .pack_index
                .iter()
                .find(|entry| entry.chunk_hash == chunk.hash)
                .unwrap();
            let payload =
                noland_pack::extract_chunk(&remote.join(pack_key(&entry.pack_id)), entry, master)
                    .unwrap();
            assert_eq!(payload.len() as u64, chunk.size);
            assert_eq!(noland_cas::blake3_hex(&payload), chunk.hash);
            restored.extend(payload);
        }
        assert_eq!(restored.len() as u64, file.size);
        assert_eq!(noland_cas::blake3_hex(&restored), file.file_hash);
        assert_eq!(&restored, expected.get(&file.relative_path).unwrap());
    }
}

#[tokio::test]
async fn balanced_host_shrink_replay_uses_current_pools_without_changing_frozen_identity() {
    let (root, agent, master, operation, mut prepared, mut provider) = fixture(64, 128);
    let balanced = |cpus| {
        preparation::Budget::for_worker_resources(
            noland_rclone_adapter::WorkerResources {
                logical_cpus: cpus,
                available_memory: Some(64 << 30),
                io_pressure: false,
            },
            BackupPerformanceMode::Balanced,
            false,
        )
    };
    let state = prepared.capture.as_mut().unwrap();
    state.budget = balanced(128);
    assert_eq!((state.budget.hashing, state.budget.packing), (32, 32));
    state.ranges = (0..state.jobs.len())
        .map(|index| (index, index + 1))
        .collect();
    state.validate().unwrap();
    write_prepared_backup(&agent, &master, operation, &prepared).unwrap();
    let checkpoint = std::fs::read(prepared_backup_path(&agent, operation)).unwrap();
    let bundle_id = prepared.manifest.bundle_id;
    let commit_id = prepared.manifest.commit_id;
    let expected = expected_sources(&prepared);
    let frozen = freeze_plans(&agent, &master, operation, &prepared, 2);
    assert_eq!(frozen.len(), 2);
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let receipts = receipt_snapshot(&directory);
    let ciphertext = frozen
        .iter()
        .map(|pack| (pack.path.clone(), std::fs::read(&pack.path).unwrap()))
        .collect::<Vec<_>>();
    provider.tuning.max_parallel_uploads = 32;
    provider.tuning.max_bulk_files = 32;
    provider.hold_uploads = true;
    let (hooks, mut events) = hooks(HashMap::new(), HashMap::new());
    provider.hooks = Some(hooks.clone());
    let state = prepared.capture.take().unwrap();
    let current = balanced(64);
    assert_eq!((current.hashing, current.packing), (16, 16));
    let execute = run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        current,
        state,
        counters(hooks),
    );
    let control = async {
        while let Some(event) = events.recv().await {
            if matches!(event, Event::Upload(_)) {
                assert_eq!(
                    std::fs::read(prepared_backup_path(&agent, operation)).unwrap(),
                    checkpoint
                );
                for (path, bytes) in &receipts {
                    assert_eq!(&std::fs::read(directory.join(path)).unwrap(), bytes);
                }
                for (path, bytes) in &ciphertext {
                    assert_eq!(&std::fs::read(path).unwrap(), bytes);
                }
                provider.release.add_permits(1024);
                return;
            }
        }
        panic!("replay never reached its retained ciphertext");
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::join!(execute, control)
    })
    .await
    .expect("Balanced replay failed to drain its admitted pools");
    result.unwrap();
    let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
    let pipeline = &progress.detail_json["streaming_pipeline"];
    assert_eq!(pipeline["reader_limit"], 16);
    assert_eq!(pipeline["codec_limit"], 16);
    assert_eq!(pipeline["capture_queue_limit"], 32);
    assert_eq!(prepared.manifest.bundle_id, bundle_id);
    assert_eq!(prepared.manifest.commit_id, commit_id);
    for pack in &frozen {
        assert_eq!(prepared.checksums.get(&pack.pack_id), Some(&pack.checksum));
    }
    for (path, bytes) in &ciphertext {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    assert_restored_sources(&prepared, &master, &root.join("remote"), &expected);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn lookahead_accounts_for_indivisible_chunks_and_tiny_or_frozen_packs_can_drain() {
    let unit = scheduler::MAX_CHUNK_PACK_BYTES;
    let limits = Limits::new(2, 3 * unit, PACK_BYTES);
    assert_eq!(limits.target, unit);
    assert_eq!(limits.lookahead_bytes, unit);
    let mut window = noland_storage::RollingUploadWindow::<()>::new(2, 3 * unit);
    window.push(2 * unit + 1, std::future::pending()).unwrap();
    assert!(!limits.can_upload(&window, 1, 1, true));
    drop(window);
    let empty = noland_storage::RollingUploadWindow::<()>::new(2, 3 * unit);
    assert!(limits.can_upload(&empty, 1, 3 * unit, true));
    assert!(!limits.can_upload(&empty, 2, 3 * unit, true));
    let tiny = Limits::new(2, 1, PACK_BYTES);
    assert_eq!((tiny.target, tiny.lookahead_bytes), (1, 0));
    assert!(tiny.can_upload(
        &noland_storage::RollingUploadWindow::<()>::new(2, 1),
        1,
        1,
        true
    ));
    let small = Limits::new(2, 20 << 20, PACK_BYTES);
    assert!(small.target < unit);
    assert_eq!(small.lookahead_bytes, unit);
}

#[test]
fn grouping_uses_only_the_ready_prefix_and_preserves_object_weighted_capacity() {
    let item = |index, bytes| {
        Ready::new(
            index,
            BuiltPack {
                pack_id: format!("fixture-{index}"),
                path: PathBuf::new(),
                bytes,
                entries: Vec::new(),
                checksum: String::new(),
            },
        )
    };
    let limits = Limits::new(4, 100 << 20, PACK_BYTES);
    let mut uploads = noland_storage::RollingUploadWindow::<()>::new(4, 100 << 20);
    let mut ready = VecDeque::from([item(0, 10 << 20)]);
    assert_eq!(
        scheduler::ready_group_len(&ready, &uploads, &limits, 2, true),
        1
    );
    ready.push_back(item(1, 10 << 20));
    assert_eq!(
        scheduler::ready_group_len(&ready, &uploads, &limits, 2, true),
        2
    );
    uploads
        .push_count(3, 30 << 20, std::future::pending())
        .unwrap();
    assert_eq!(
        scheduler::ready_group_len(&ready, &uploads, &limits, 2, true),
        1
    );
    drop(uploads);
    let uploads = noland_storage::RollingUploadWindow::<()>::new(4, 100 << 20);
    let ready = VecDeque::from([item(0, 40 << 20), item(1, 40 << 20)]);
    assert_eq!(
        scheduler::ready_group_len(&ready, &uploads, &limits, 2, true),
        1
    );
}

#[test]
fn sealed_only_tail_releases_soft_lookahead_without_exceeding_absolute_limits() {
    let unit = scheduler::MAX_CHUNK_PACK_BYTES;
    let limits = Limits::new(2, 3 * unit, PACK_BYTES);
    let mut uploads = noland_storage::RollingUploadWindow::<()>::new(2, 3 * unit);
    uploads.push(2 * unit, std::future::pending()).unwrap();
    assert!(!limits.can_upload(&uploads, 1, unit, true));
    assert!(limits.can_upload(&uploads, 1, unit, false));
    assert!(!limits.can_upload(&uploads, 1, unit + 1, false));
    assert!(!limits.can_upload(&uploads, 2, unit, false));
}

#[tokio::test]
async fn frozen_sealed_tail_fills_spare_slots_without_waiting_for_the_first_upload() {
    let (root, agent, master, operation, mut prepared, mut provider) =
        incompressible_fixture(2, 20 << 20);
    let frozen = freeze_plans_inner(&agent, &master, operation, &prepared, 2, true);
    assert_eq!(frozen.len(), 2);
    provider.tuning.max_bulk_bytes = frozen.iter().map(|pack| pack.bytes).sum();
    let limits = Limits::new(2, provider.tuning.max_bulk_bytes, PACK_BYTES);
    assert!(limits.lookahead_bytes > 0);
    assert!(frozen.iter().all(|pack| pack.bytes > limits.target));
    provider.hold_uploads = true;
    let (hooks, mut events) = hooks(HashMap::new(), HashMap::new());
    provider.hooks = Some(hooks.clone());
    let state = prepared.capture.take().unwrap();
    let execute = run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission(),
        state,
        counters(hooks),
    );
    let control = async {
        let mut started = 0;
        while let Some(event) = events.recv().await {
            if matches!(event, Event::Upload(_)) {
                started += 1;
                if started == 2 {
                    assert_eq!(provider.active.load(Ordering::SeqCst), 2);
                    provider.release.add_permits(1024);
                    return;
                }
            }
        }
        panic!("both frozen uploads must start before either is allowed to finish");
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::join!(execute, control)
    })
    .await
    .expect("sealed-only tail retained unnecessary preparation headroom");
    result.unwrap();
    for pack in frozen {
        assert_eq!(prepared.checksums.get(&pack.pack_id), Some(&pack.checksum));
    }
    let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
    assert_eq!(
        progress.detail_json["streaming_pipeline"]["peak_uploads"],
        2
    );
    assert_eq!(
        progress.detail_json["streaming_pipeline"]["codec_lookahead_required"],
        false
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn member_receipts_release_shared_bytes_but_retain_the_child_process_slot() {
    let mut uploads = noland_storage::RollingUploadWindow::<()>::new(3, 30);
    let (finish, exited) = tokio::sync::oneshot::channel::<()>();
    uploads
        .push_tracked(2, 20, |reservation| async move {
            reservation.completed(2, 20);
            exited.await.unwrap();
            Ok(())
        })
        .unwrap();
    let local = HashMap::from([(7, 10)]);
    assert_eq!(reservation_snapshot(&local, &uploads), (3, 30));
    assert!(matches!(
        scheduler::upload_event(&mut uploads).await,
        UploadEvent::CapacityReleased
    ));
    assert_eq!(reservation_snapshot(&local, &uploads), (2, 10));
    assert_eq!((uploads.len(), uploads.bytes()), (1, 0));
    uploads.push(20, async { Ok(()) }).unwrap();
    assert!(matches!(
        scheduler::upload_event(&mut uploads).await,
        UploadEvent::Completed(Ok(()))
    ));
    assert_eq!((uploads.len(), uploads.bytes()), (1, 0));
    finish.send(()).unwrap();
    assert!(matches!(
        scheduler::upload_event(&mut uploads).await,
        UploadEvent::Completed(Ok(()))
    ));
    assert!(uploads.is_empty());
}

#[tokio::test]
async fn two_codecs_seal_and_upload_out_of_order_then_restore_exact_files_and_chunks() {
    let (root, agent, master, operation, prepared, provider) = incompressible_fixture(2, 9 << 20);
    let expected = expected_sources(&prepared);
    assert!(freeze_plans_inner(&agent, &master, operation, &prepared, 0, true).is_empty());
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let output = agent
        .config
        .paths
        .packs
        .join(prepared.manifest.bundle_id.to_string());
    let plans = load_plans(&directory, &master, operation).unwrap();
    assert_eq!(plans.len(), 2);
    assert!(plans.iter().all(|plan| plan.chunks.len() >= 2));
    let first = Arc::new(Gate::default());
    let second = Arc::new(Gate::default());
    let (hooks, mut events) = hooks(
        HashMap::from([(0, first.clone()), (1, second.clone())]),
        HashMap::new(),
    );
    let stats = counters(hooks);
    // Exercise the real workers directly: the host's current CPU/memory budget
    // must not reduce this regression to a single-codec test.
    let mut workers = pack_workers(
        &agent,
        &master,
        operation,
        &directory,
        &output,
        2,
        2,
        stats.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    for plan in plans {
        assert!(workers.sender.as_ref().unwrap().try_send(plan).is_ok());
    }
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut entered = [false; 2];
        while !entered.iter().all(|entered| *entered) {
            match events.recv().await.unwrap() {
                Event::Codec(index) => entered[index] = true,
                Event::Sealed(index) => panic!("codec {index} bypassed its closed gate"),
                _ => {}
            }
        }
    })
    .await
    .expect("both real codec workers did not enter their gates");
    assert_eq!(stats.active_codecs.load(Ordering::Relaxed), 2);
    assert_eq!(stats.peak_codecs.load(Ordering::Relaxed), 2);

    second.release();
    let (index, second_pack, _) =
        tokio::time::timeout(Duration::from_secs(120), workers.receiver.recv())
            .await
            .expect("codec 1 waited for the held codec 0")
            .unwrap()
            .unwrap();
    assert_eq!(index, 1);
    assert!(!*first.open.lock().unwrap());
    assert!(!directory.join("sealed/0.enc").exists());
    let session =
        noland_storage::PackUploadSession::new(&provider, Some(&agent.db), Some(operation));
    session
        .upload(&[(second_pack.pack_id.clone(), second_pack.path.clone())])
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(agent
        .db
        .sync_journal_completed(operation, &pack_key(&second_pack.pack_id))
        .unwrap());
    assert!(!directory.join("sealed/0.enc").exists());
    assert!(!*first.open.lock().unwrap());

    first.release();
    let (index, first_pack, _) =
        tokio::time::timeout(Duration::from_secs(120), workers.receiver.recv())
            .await
            .expect("codec 0 did not finish after its gate was released")
            .unwrap()
            .unwrap();
    assert_eq!(index, 0);
    session
        .upload(&[(first_pack.pack_id.clone(), first_pack.path.clone())])
        .await
        .unwrap();
    drop(workers);
    assert_eq!(stats.active_codecs.load(Ordering::Relaxed), 0);
    let sealed = [second_pack, first_pack];
    assert_eq!(
        provider
            .uploaded
            .lock()
            .unwrap()
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>(),
        sealed
            .iter()
            .map(|pack| pack_key(&pack.pack_id))
            .collect::<Vec<_>>()
    );
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission(),
    )
    .await
    .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    for pack in sealed {
        assert_eq!(resumed.checksums.get(&pack.pack_id), Some(&pack.checksum));
    }
    assert_restored_sources(&resumed, &master, &root.join("remote"), &expected);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn incompressible_codec_progresses_while_all_upload_slots_are_held_at_exact_budget() {
    let (root, agent, master, operation, mut prepared, mut provider) =
        incompressible_fixture(1, 64 << 20);
    let unit = scheduler::MAX_CHUNK_PACK_BYTES;
    provider.tuning.max_bulk_bytes = 3 * unit;
    provider.hold_uploads = true;
    let third = Arc::new(Gate::default());
    let (hooks, mut events) = hooks(HashMap::from([(2, third.clone())]), HashMap::new());
    provider.hooks = Some(hooks.clone());
    let state = prepared.capture.take().unwrap();
    let execute = run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission(),
        state,
        counters(hooks),
    );
    let control = async {
        let mut uploads = 0;
        let mut queued = false;
        loop {
            match events.recv().await.unwrap() {
                Event::Upload(call) if call <= 2 => uploads += 1,
                Event::Codec(2) => queued = true,
                Event::Sealed(2) => {
                    assert!(queued);
                    assert_eq!(uploads, 2);
                    assert_eq!(provider.active.load(Ordering::SeqCst), 2);
                    provider.release.add_permits(1024);
                    break;
                }
                _ => {}
            }
            if uploads == 2 && queued {
                third.release();
            }
        }
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::join!(execute, control)
    })
    .await
    .expect("codec and network gate dependency did not resolve");
    result.unwrap();
    assert!(prepared
        .pack_index
        .iter()
        .all(|entry| entry.compression == noland_pack::PackCompression::None));
    let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
    let pipeline = &progress.detail_json["streaming_pipeline"];
    assert_eq!(pipeline["pack_target_bytes"], unit);
    assert_eq!(pipeline["codec_lookahead_bytes"], unit);
    assert_eq!(pipeline["peak_uploads"], 2);
    assert!(pipeline["peak_reserved_bytes"].as_u64().unwrap() <= 3 * unit);
    assert!(pipeline["peak_reserved_count"].as_u64().unwrap() <= 4);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn upload_futures_continue_while_the_next_plan_is_not_yet_durable() {
    let (root, agent, master, operation, mut prepared, mut provider) =
        incompressible_fixture(1, 40 << 20);
    provider.hold_uploads = true;
    let writing = Arc::new(Gate::default());
    let (hooks, mut events) = hooks(HashMap::new(), HashMap::from([(1, writing.clone())]));
    provider.hooks = Some(hooks.clone());
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let state = prepared.capture.take().unwrap();
    let execute = run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission(),
        state,
        counters(hooks),
    );
    let control = async {
        let mut uploading = false;
        let mut persisting = false;
        loop {
            match events.recv().await.unwrap() {
                Event::Upload(1) => uploading = true,
                Event::Persisting(1) => persisting = true,
                Event::Durable(key) => {
                    assert!(persisting && uploading);
                    assert!(agent.db.sync_journal_completed(operation, &key).unwrap());
                    assert!(!directory.join("plans/1.enc").exists());
                    assert!(!directory.join("sealed/1.enc").exists());
                    writing.release();
                    provider.release.add_permits(1024);
                    break;
                }
                _ => {}
            }
            if persisting && uploading {
                provider.release.add_permits(1);
            }
        }
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::join!(execute, control)
    })
    .await
    .expect("upload was not driven while plan persistence was pending");
    result.unwrap();
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancellation_joins_the_pending_plan_writer_without_publishing_or_consuming_its_plan() {
    let (root, agent, master, operation, mut prepared, provider) =
        incompressible_fixture(1, 40 << 20);
    let expected =
        noland_cas::blake3_file(&prepared.capture.as_ref().unwrap().jobs[0].0.source).unwrap();
    let writing = Arc::new(Gate::default());
    let (hooks, mut events) = hooks(HashMap::new(), HashMap::from([(0, writing)]));
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let state = prepared.capture.take().unwrap();
    let mut execute = Box::pin(run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission(),
        state,
        counters(hooks),
    ));
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            tokio::select! {
                result = &mut execute => panic!("pending plan writer unexpectedly exited: {result:?}"),
                event = events.recv() => {
                    if matches!(event.unwrap(), Event::Persisting(0)) {
                        break;
                    }
                }
            }
        }
    })
    .await
    .expect("plan writer was never reached");
    drop(execute);
    assert!(!directory.join("plans/0.enc").exists());
    assert!(!directory.join("sealed/0.enc").exists());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.active_groups.load(Ordering::SeqCst), 0);
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.manifest.files[0].file_hash, expected);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancellation_after_plan_fsync_before_ack_preserves_the_plan_and_retry_reuses_it() {
    let (root, agent, master, operation, mut prepared, mut provider) =
        incompressible_fixture(1, 9 << 20);
    let expected = expected_sources(&prepared);
    let before_ack = Arc::new(Gate::default());
    let (mut initial_hooks, mut events) = hooks(HashMap::new(), HashMap::new());
    Arc::get_mut(&mut initial_hooks)
        .unwrap()
        .after_plans
        .insert(0, before_ack.clone());
    let stats = counters(initial_hooks);
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let path = directory.join("plans/0.enc");
    let state = prepared.capture.take().unwrap();
    let mut execute = Box::pin(run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission(),
        state,
        stats.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            tokio::select! {
                result = &mut execute => panic!("unacknowledged plan unexpectedly finished: {result:?}"),
                event = events.recv() => match event.unwrap() {
                    Event::PlanDurable(0) => break,
                    Event::Codec(index) => panic!("codec {index} consumed an unacknowledged plan"),
                    _ => {}
                }
            }
        }
    })
    .await
    .expect("post-fsync, pre-acknowledgement boundary was never reached");
    // The real write_batch, including file and directory fsync, has returned.
    // Only acknowledgement is gated; this does not simulate power loss or an
    // operating-system fsync that stalls or fails.
    let receipt = std::fs::read(&path).unwrap();
    let frozen: Plan = read(&path, &master, &format!("backup-stream:{operation}:plan:0"))
        .unwrap()
        .unwrap();
    assert_eq!(frozen.index, 0);
    assert!(!frozen.chunks.is_empty());
    assert_eq!(stats.active_codecs.load(Ordering::Relaxed), 0);
    assert_eq!(provider.remote_calls.load(Ordering::SeqCst), 0);
    assert!(!directory.join("sealed/0.enc").exists());
    drop(execute);
    assert!(!*before_ack.open.lock().unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), receipt);
    assert_eq!(stats.active_codecs.load(Ordering::Relaxed), 0);
    assert!(!directory.join("sealed/0.enc").exists());
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, Event::Codec(_)),
            "cancellation allowed a codec to consume the unacknowledged plan"
        );
    }

    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    let (retry_hooks, mut retry_events) = hooks(HashMap::new(), HashMap::new());
    provider.hooks = Some(retry_hooks.clone());
    provider.hold_uploads = true;
    let state = resumed.capture.take().unwrap();
    let execute = run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission(),
        state,
        counters(retry_hooks),
    );
    let control = async {
        let mut consumed_frozen = false;
        loop {
            match retry_events.recv().await.unwrap() {
                Event::Persisting(0) | Event::PlanDurable(0) => {
                    panic!("retry rewrote the already-durable plan")
                }
                Event::Codec(0) => consumed_frozen = true,
                Event::Upload(1) => {
                    assert!(consumed_frozen);
                    assert_eq!(std::fs::read(&path).unwrap(), receipt);
                    let replayed: Plan =
                        read(&path, &master, &format!("backup-stream:{operation}:plan:0"))
                            .unwrap()
                            .unwrap();
                    assert_eq!(
                        serde_json::to_value(&replayed).unwrap(),
                        serde_json::to_value(&frozen).unwrap()
                    );
                    let sealed = packing::read_receipt(
                        &directory.join("sealed/0.enc"),
                        &master,
                        &format!("backup-stream:{operation}:sealed:0"),
                    )
                    .unwrap()
                    .unwrap();
                    assert_eq!(
                        sealed
                            .entries
                            .iter()
                            .map(|entry| entry.chunk_hash.as_str())
                            .collect::<Vec<_>>(),
                        frozen
                            .chunks
                            .iter()
                            .map(|chunk| chunk.hash.as_str())
                            .collect::<Vec<_>>()
                    );
                    provider.release.add_permits(1024);
                    break;
                }
                _ => {}
            }
        }
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::join!(execute, control)
    })
    .await
    .expect("retry did not consume the retained durable plan");
    result.unwrap();
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    assert_restored_sources(&resumed, &master, &root.join("remote"), &expected);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ready_groups_refill_on_member_receipts_and_cancel_retry_keeps_frozen_ciphertext() {
    let (root, agent, master, operation, mut prepared, mut provider) =
        incompressible_fixture(6, 128 << 10);
    let frozen = freeze_plans(&agent, &master, operation, &prepared, 4);
    assert_eq!(frozen.len(), 4);
    let object_bytes = (128 << 10) + 76;
    assert!(frozen.iter().all(|pack| pack.bytes == object_bytes));
    provider.tuning.max_parallel_uploads = 4;
    provider.tuning.max_bulk_files = 4;
    provider.tuning.max_bulk_bytes = 4 * object_bytes;
    provider.parallel_groups = true;
    provider.hold_first_groups = true;
    let (hooks, mut events) = hooks(HashMap::new(), HashMap::new());
    provider.hooks = Some(hooks.clone());
    let state = prepared.capture.take().unwrap();
    let mut execute = Box::pin(run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission(),
        state,
        counters(hooks),
    ));
    let mut durable = Vec::new();
    let control = async {
        let mut initial_groups = 0;
        loop {
            match events.recv().await.unwrap() {
                Event::Group(group, count) if group <= 2 => {
                    assert_eq!(count, 2, "already-ready small packs must be grouped");
                    initial_groups += 1;
                }
                Event::Durable(key) => durable.push(key),
                Event::Group(3, _) => {
                    assert_eq!(initial_groups, 2);
                    assert!(durable.len() >= 2);
                    assert!(provider.active_groups.load(Ordering::SeqCst) >= 2);
                    for key in &durable {
                        assert!(agent.db.sync_journal_completed(operation, key).unwrap());
                    }
                    // Four original packs exactly filled the shared byte
                    // budget. A fifth pack cannot have sealed/refilled unless
                    // the first groups' member receipts released outer bytes.
                    assert!(prepared_backup_path(&agent, operation)
                        .with_extension("stream")
                        .join("sealed/4.enc")
                        .exists());
                    break;
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(120), async {
        tokio::select! {
            result = &mut execute => panic!("stragglers unexpectedly exited: {result:?}"),
            () = control => {}
        }
    })
    .await
    .expect("member receipts did not refill the shared count/byte window");
    drop(execute);
    assert_eq!(provider.active_groups.load(Ordering::SeqCst), 0);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    provider.hold_first_groups = false;
    provider.hooks = None;
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission(),
    )
    .await
    .unwrap();
    for pack in &frozen {
        assert_eq!(resumed.checksums.get(&pack.pack_id), Some(&pack.checksum));
    }
    for key in durable {
        assert_eq!(
            provider
                .uploaded
                .lock()
                .unwrap()
                .iter()
                .filter(|(actual, _)| actual == &key)
                .count(),
            1,
            "a durable member receipt must survive group cancellation"
        );
    }
    let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
    let pipeline = &progress.detail_json["streaming_pipeline"];
    assert_eq!(pipeline["peak_upload_group_size"], 2);
    assert!(pipeline["peak_reserved_bytes"].as_u64().unwrap() <= 4 * object_bytes);
    assert_eq!(provider.active_groups.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn retained_compressed_pack_can_resume_below_its_original_plaintext_reservation() {
    let (root, agent, master, operation, mut prepared, mut provider) = fixture(1, 8 << 20);
    let frozen = freeze_plans(&agent, &master, operation, &prepared, 1);
    assert_eq!(frozen.len(), 1);
    provider.tuning.max_bulk_bytes = frozen[0].bytes;
    assert!(frozen[0].bytes < 8 << 20);
    super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission(),
    )
    .await
    .unwrap();
    assert_eq!(
        prepared.checksums.get(&frozen[0].pack_id),
        Some(&frozen[0].checksum)
    );
    let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
    let pipeline = &progress.detail_json["streaming_pipeline"];
    assert_eq!(pipeline["peak_codecs"], 0);
    assert_eq!(pipeline["peak_reserved_bytes"], frozen[0].bytes);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn frozen_unsealed_plan_one_byte_over_budget_is_retained_and_exact_boundary_can_resume() {
    let (root, agent, master, operation, prepared, mut provider) =
        incompressible_fixture(1, 9 << 20);
    let expected = expected_sources(&prepared);
    assert!(freeze_plans_inner(&agent, &master, operation, &prepared, 0, true).is_empty());
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let plans = load_plans(&directory, &master, operation).unwrap();
    assert_eq!(plans.len(), 1);
    assert!(plans[0].chunks.len() >= 2);
    let boundary = plans[0].bytes().unwrap();
    let receipts = receipt_snapshot(&directory);
    provider.tuning.max_bulk_bytes = boundary - 1;
    let mut rejected = resume(&agent, &provider, &master, operation, &prepared);
    let error = tokio::time::timeout(
        Duration::from_secs(120),
        super::super::run(
            &agent,
            &provider,
            &master,
            operation,
            &mut rejected,
            admission(),
        ),
    )
    .await
    .expect("oversized unsealed plan stalled instead of failing")
    .unwrap_err();
    assert!(
        matches!(&error, StateError::Invalid(message) if message.contains("retained streaming plan exceeds transfer byte budget")),
        "{error:?}"
    );
    assert_eq!(provider.remote_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(receipt_snapshot(&directory), receipts);
    assert!(!directory.join("sealed/0.enc").exists());
    assert_eq!(
        load_plans(&directory, &master, operation).unwrap()[0]
            .bytes()
            .unwrap(),
        boundary
    );

    // The original, multi-chunk plan fits at equality. Stop at the first
    // transfer so its new durable ciphertext can be checked before a retry.
    provider.tuning.max_bulk_bytes = boundary;
    provider.fail.store(true, Ordering::SeqCst);
    let mut interrupted = resume(&agent, &provider, &master, operation, &prepared);
    let error = tokio::time::timeout(
        Duration::from_secs(120),
        super::super::run(
            &agent,
            &provider,
            &master,
            operation,
            &mut interrupted,
            admission(),
        ),
    )
    .await
    .expect("exact-boundary unsealed plan did not reach its first transfer")
    .unwrap_err();
    assert!(
        matches!(&error, StateError::Storage(message) if message.contains("injected transfer interruption"))
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    for (path, bytes) in &receipts {
        assert_eq!(&std::fs::read(directory.join(path)).unwrap(), bytes);
    }
    let pack = packing::read_receipt(
        &directory.join("sealed/0.enc"),
        &master,
        &format!("backup-stream:{operation}:sealed:0"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(pack.bytes, boundary);
    assert!(pack
        .entries
        .iter()
        .all(|entry| entry.compression == noland_pack::PackCompression::None));
    let ciphertext = std::fs::read(&pack.path).unwrap();
    provider.fail.store(false, Ordering::SeqCst);
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    tokio::time::timeout(
        Duration::from_secs(120),
        super::super::run(
            &agent,
            &provider,
            &master,
            operation,
            &mut resumed,
            admission(),
        ),
    )
    .await
    .expect("exact-boundary frozen plan did not resume")
    .unwrap();
    assert_eq!(resumed.checksums.get(&pack.pack_id), Some(&pack.checksum));
    assert_eq!(std::fs::read(&pack.path).unwrap(), ciphertext);
    let uploads = provider.uploaded.lock().unwrap();
    assert_eq!(uploads.len(), 2);
    assert!(uploads
        .iter()
        .all(|(key, checksum)| key == &pack_key(&pack.pack_id) && checksum == &pack.checksum));
    drop(uploads);
    assert_restored_sources(&resumed, &master, &root.join("remote"), &expected);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn frozen_sealed_pack_one_byte_over_budget_is_retained_and_exact_boundary_keeps_identity() {
    let (root, agent, master, operation, prepared, mut provider) =
        incompressible_fixture(1, 128 << 10);
    let expected = expected_sources(&prepared);
    let frozen = freeze_plans(&agent, &master, operation, &prepared, 1);
    assert_eq!(frozen.len(), 1);
    let pack = &frozen[0];
    assert!(pack
        .entries
        .iter()
        .all(|entry| entry.compression == noland_pack::PackCompression::None));
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let receipts = receipt_snapshot(&directory);
    let ciphertext = std::fs::read(&pack.path).unwrap();
    provider.tuning.max_bulk_bytes = pack.bytes - 1;
    let mut rejected = resume(&agent, &provider, &master, operation, &prepared);
    let error = tokio::time::timeout(
        Duration::from_secs(120),
        super::super::run(
            &agent,
            &provider,
            &master,
            operation,
            &mut rejected,
            admission(),
        ),
    )
    .await
    .expect("oversized sealed pack stalled instead of failing")
    .unwrap_err();
    assert!(
        matches!(&error, StateError::Invalid(message) if message.contains("retained streaming plan exceeds transfer byte budget")),
        "{error:?}"
    );
    assert_eq!(provider.remote_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(receipt_snapshot(&directory), receipts);
    assert_eq!(std::fs::read(&pack.path).unwrap(), ciphertext);
    let retained = packing::read_receipt(
        &directory.join("sealed/0.enc"),
        &master,
        &format!("backup-stream:{operation}:sealed:0"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(retained.pack_id, pack.pack_id);
    assert_eq!(retained.checksum, pack.checksum);

    provider.tuning.max_bulk_bytes = pack.bytes;
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    tokio::time::timeout(
        Duration::from_secs(120),
        super::super::run(
            &agent,
            &provider,
            &master,
            operation,
            &mut resumed,
            admission(),
        ),
    )
    .await
    .expect("exact-boundary sealed pack did not resume")
    .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *provider.uploaded.lock().unwrap(),
        vec![(pack_key(&pack.pack_id), pack.checksum.clone())]
    );
    assert_eq!(resumed.checksums.get(&pack.pack_id), Some(&pack.checksum));
    assert_eq!(std::fs::read(&pack.path).unwrap(), ciphertext);
    let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
    let pipeline = &progress.detail_json["streaming_pipeline"];
    assert_eq!(pipeline["peak_codecs"], 0);
    assert_eq!(pipeline["peak_reserved_bytes"], pack.bytes);
    assert_restored_sources(&resumed, &master, &root.join("remote"), &expected);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn corrupt_frozen_plan_sealed_receipt_or_ciphertext_fails_before_remote_effects() {
    let (root, agent, master, operation, prepared, provider) = incompressible_fixture(2, 128 << 10);
    let frozen = freeze_plans(&agent, &master, operation, &prepared, 2);
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    for path in [
        directory.join("plans/0.enc"),
        directory.join("sealed/0.enc"),
        frozen[0].path.clone(),
    ] {
        let original = std::fs::read(&path).unwrap();
        let mut corrupt = original.clone();
        let end = corrupt.len() - 1;
        corrupt[end] ^= 1;
        std::fs::write(&path, &corrupt).unwrap();
        let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
        assert!(super::super::run(
            &agent,
            &provider,
            &master,
            operation,
            &mut resumed,
            admission()
        )
        .await
        .is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
        std::fs::write(&path, &original).unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn upload_starts_before_multi_gigabyte_eof_and_changed_partial_revision_is_rejected() {
    let (root, agent, master, operation, mut prepared, provider) = fixture(1, (2 << 30) + 1);
    let source = prepared.capture.as_ref().unwrap().jobs[0].0.source.clone();
    provider.fail.store(true, Ordering::SeqCst);
    let failure = tokio::time::timeout(
        Duration::from_secs(30),
        super::super::run(
            &agent,
            &provider,
            &master,
            operation,
            &mut prepared,
            admission(),
        ),
    )
    .await
    .unwrap();
    assert!(failure.is_err());
    assert!(provider.calls.load(Ordering::SeqCst) > 0);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    let capture = prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .join("capture/0");
    let mut captured = 0;
    let mut sequence = 0;
    while let Some(receipt) = read::<Receipt>(
        &capture.join(format!("{sequence}.enc")),
        &master,
        &format!("backup-stream:{operation}:capture:0:{sequence}"),
    )
    .unwrap()
    {
        assert!(!receipt.done, "upload waited for whole-file capture");
        captured += receipt
            .chunks
            .iter()
            .map(|c| c.source.len().unwrap())
            .sum::<u64>();
        sequence += 1;
    }
    assert!(captured > 0 && captured < 2 << 30);
    std::fs::write(&source, b"new generation must survive").unwrap();
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    // No old/new source concatenation is permitted, even though prefix packs exist.
    let state = resumed.capture.take().unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let error = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                capture_work(
                    &agent.config.paths.cache.join("cas/chunks"),
                    &prepared_backup_path(&agent, operation).with_extension("stream"),
                    operation,
                    0,
                    &state,
                    &master,
                    &Counters::default(),
                    &|| false,
                    &sender,
                )
            })
            .join()
            .unwrap()
    })
    .unwrap_err();
    assert!(matches!(error, StateError::Conflict(_)));
    receiver.close();
    assert_eq!(
        std::fs::read(source).unwrap(),
        b"new generation must survive"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn independent_slots_resume_frozen_ciphertext_and_restore_exact_files() {
    let (root, agent, master, operation, mut prepared, mut provider) = fixture(2, 72 << 20);
    let expected = prepared
        .capture
        .as_ref()
        .unwrap()
        .jobs
        .iter()
        .map(|(job, _)| {
            (
                job.logical.relative_path.clone(),
                noland_cas::blake3_file(&job.source).unwrap(),
            )
        })
        .collect::<HashMap<_, _>>();
    provider.fail.store(true, Ordering::SeqCst);
    assert!(super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission()
    )
    .await
    .is_err());
    let failed = provider.uploaded.lock().unwrap().clone();
    assert!(!failed.is_empty());
    provider.fail.store(false, Ordering::SeqCst);
    provider.calls.store(0, Ordering::SeqCst);
    provider.slow_first = true;
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission(),
    )
    .await
    .unwrap();
    assert_eq!(
        provider.peak.load(Ordering::SeqCst),
        2,
        "one codec must feed both independent upload slots"
    );
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    for (key, checksum) in failed {
        let calls = provider.uploaded.lock().unwrap();
        let matches = calls
            .iter()
            .filter(|(candidate, _)| candidate == &key)
            .collect::<Vec<_>>();
        assert!(matches.len() >= 2);
        assert!(matches.iter().all(|(_, actual)| actual == &checksum));
    }
    assert_eq!(resumed.manifest.files.len(), 2);
    let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
    let pipeline = &progress.detail_json["streaming_pipeline"];
    assert!(pipeline["peak_reserved_bytes"].as_u64().unwrap() <= 20 << 20);
    assert_eq!(pipeline["active_readers"], 0);
    assert_eq!(pipeline["active_codecs"], 0);
    assert_eq!(pipeline["selected_files"], 2);
    assert_eq!(pipeline["read_bytes"], 144 << 20);
    assert_eq!(pipeline["durable_bytes"], 144 << 20);
    assert_eq!(pipeline["reserved_bytes"], 0);
    assert_eq!(pipeline["ready_packs"], 0);
    assert_eq!(pipeline["retained_capture_logical_bytes"], 0);
    assert_eq!(pipeline["waiting_on"], "manifest_commit");
    let mut buffer = Vec::new();
    for file in &resumed.manifest.files {
        buffer.clear();
        for chunk in &file.chunks {
            let entry = resumed
                .pack_index
                .iter()
                .find(|entry| entry.chunk_hash == chunk.hash)
                .unwrap();
            let bytes = noland_pack::extract_chunk(
                &root.join("remote").join(pack_key(&entry.pack_id)),
                entry,
                &master,
            )
            .unwrap();
            buffer.extend(bytes);
        }
        assert_eq!(buffer.len() as u64, file.size);
        assert_eq!(noland_cas::blake3_hex(&buffer), file.file_hash);
        assert_eq!(expected.get(&file.relative_path), Some(&file.file_hash));
    }
    assert!(!prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .exists());
    assert!(
        std::fs::read_dir(agent.config.paths.cache.join("cas/chunks/.leases"))
            .unwrap()
            .next()
            .is_none()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn authenticated_receipt_corruption_cannot_be_recaptured_or_committed() {
    let (root, agent, master, operation, mut prepared, provider) = fixture(1, 40 << 20);
    provider.fail.store(true, Ordering::SeqCst);
    assert!(super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut prepared,
        admission()
    )
    .await
    .is_err());
    let receipt = prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .join("capture/0/0.enc");
    let mut bytes = std::fs::read(&receipt).unwrap();
    let end = bytes.len() - 1;
    bytes[end] ^= 1;
    std::fs::write(&receipt, &bytes).unwrap();
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    provider.fail.store(false, Ordering::SeqCst);
    assert!(super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission()
    )
    .await
    .is_err());
    assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
    bytes[end] ^= 1;
    std::fs::write(&receipt, &bytes).unwrap();
    let lease_receipt = receipt.with_file_name("lease.enc");
    let lease_bytes = std::fs::read(&lease_receipt).unwrap();
    let calls = provider.calls.load(Ordering::SeqCst);
    for replacement in [None, Some(b"corrupt lease".as_slice())] {
        std::fs::remove_file(&lease_receipt).unwrap();
        if let Some(bytes) = replacement {
            std::fs::write(&lease_receipt, bytes).unwrap();
        }
        let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
        assert!(super::super::run(
            &agent,
            &provider,
            &master,
            operation,
            &mut resumed,
            admission()
        )
        .await
        .is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
        assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
        // Keep the authenticated identity available for the next failure case.
        std::fs::write(&lease_receipt, &lease_bytes).unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancellation_at_durable_handoff_replays_prefix_and_joins_every_writer() {
    let (root, agent, master, operation, mut prepared, provider) = fixture(1, 80 << 20);
    let state = prepared.capture.take().unwrap();
    let expected = noland_cas::blake3_file(&state.jobs[0].0.source).unwrap();
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let cas_root = agent.config.paths.cache.join("cas/chunks");
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let stopped = stop.clone();
    let key = master.clone();
    let thread = std::thread::spawn(move || {
        capture_work(
            &cas_root,
            &directory,
            operation,
            0,
            &state,
            &key,
            &Counters::default(),
            &|| stopped.load(Ordering::Acquire),
            &sender,
        )
    });
    let first = receiver.recv().await.unwrap().unwrap();
    assert!(!first.done && first.files.is_empty());
    assert!(
        first
            .chunks
            .iter()
            .map(|chunk| chunk.source.len().unwrap())
            .sum::<u64>()
            <= SEGMENT_BYTES
    );
    stop.store(true, Ordering::Release);
    receiver.close();
    assert!(thread.join().unwrap().is_err());
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.manifest.files[0].file_hash, expected);
    let mut hasher = blake3::Hasher::new();
    let mut length = 0u64;
    for chunk in &resumed.manifest.files[0].chunks {
        let entry = resumed
            .pack_index
            .iter()
            .find(|entry| entry.chunk_hash == chunk.hash)
            .unwrap();
        let bytes = noland_pack::extract_chunk(
            &root.join("remote").join(pack_key(&entry.pack_id)),
            entry,
            &master,
        )
        .unwrap();
        length += bytes.len() as u64;
        hasher.update(&bytes);
    }
    assert_eq!(length, 80 << 20);
    assert_eq!(format!("blake3:{}", hasher.finalize().to_hex()), expected);
    assert!(!prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .exists());
    std::fs::remove_dir_all(root).unwrap();
}
