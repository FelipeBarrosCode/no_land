//! Opt-in synthetic workload. Uses an isolated fixture and the real library
//! capture/restore paths; it never starts an application or uses live saves.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

struct DiskSampler {
    stopped: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DiskSampler {
    fn start(path: PathBuf) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let worker_stop = Arc::clone(&stopped);
        let worker_peak = Arc::clone(&peak);
        let initial = disk::available(&path).unwrap().1;
        let thread = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                if let Ok((_, free)) = disk::available(&path) {
                    worker_peak.fetch_max(initial.saturating_sub(free), Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Self {
            stopped,
            peak,
            thread: Some(thread),
        }
    }
}

impl Drop for DiskSampler {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

#[tokio::test]
#[ignore = "opt-in synthetic file-count scalability measurement"]
async fn synthetic_storage_workload() {
    let count = std::env::var("NOLAND_SYNTHETIC_FILES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4096)
        .max(32);
    let root = std::env::current_dir()
        .unwrap()
        .join("target/noland-scale")
        .join(Uuid::new_v4().to_string());
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
    let app = AppId::desktop("synthetic-storage");
    agent
        .db
        .upsert_app(&AppIdentity::new(app.clone(), "Synthetic Storage"))
        .unwrap();
    let install = agent.config.home.join("Games/SyntheticStorage");
    agent
        .db
        .add_known_root(&app, "install", install.to_str().unwrap())
        .unwrap();
    let mut changed_path = PathBuf::new();
    for index in 0..count {
        let path = install.join(format!("{:04}/asset-{index:06}.dat", index / 512));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = if index < 4 {
            vec![index as u8; 8 * 1024 * 1024]
        } else {
            format!("synthetic-{index:06}-{}", "content".repeat(32)).into_bytes()
        };
        std::fs::write(&path, bytes).unwrap();
        changed_path = path;
    }
    let sampler = DiskSampler::start(root.clone());
    let master = MasterKey::generate();
    let storage = LocalStorage::new(root.join("cloud"));
    let mut snapshots = Vec::new();
    let mut backup_metrics = Vec::new();
    for round in 0..3 {
        if round == 2 {
            std::fs::write(&changed_path, b"one changed save").unwrap();
            let record = agent
                .db
                .get_path_by_canonical(changed_path.to_str().unwrap())
                .unwrap()
                .unwrap();
            agent
                .db
                .mark_dirty(&app, Some(record.path_id), false)
                .unwrap();
            agent
                .db
                .append_app_mutation(&AppMutationRecord::new(
                    app.clone(),
                    changed_path.to_str().unwrap(),
                    AppMutationKind::Modify,
                ))
                .unwrap();
        }
        let operation = Uuid::new_v4();
        let started = Instant::now();
        let manifest = run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            Some(operation),
        )
        .await
        .unwrap();
        assert_eq!(manifest.files.len(), count);
        let metrics = operation_metrics(&agent.db.get_operation(operation).unwrap().unwrap());
        println!(
            "STORAGE_SCALE {}",
            serde_json::json!({
                "phase": (["first_capture", "unchanged_capture", "one_save_capture"][round]),
                "files": count, "elapsed_ms": elapsed_ms(started), "metrics": metrics,
                "filesystem_peak_delta_bytes": sampler.peak.load(Ordering::Relaxed),
            })
        );
        snapshots.push(manifest);
        backup_metrics.push(metrics);
    }
    assert_eq!(snapshots[0].bundle_id, snapshots[1].bundle_id);
    assert_eq!(backup_metrics[0].num_files_rehashed, count as u64);
    assert_eq!(backup_metrics[1].num_files_rehashed, 0);
    assert_eq!(backup_metrics[1].bytes_hashed, 0);
    assert_eq!(backup_metrics[2].num_files_rehashed, 1);
    assert_eq!(
        backup_metrics[2].bytes_hashed,
        b"one changed save".len() as u64
    );
    let index = read_pack_index(&storage, &master, &app, snapshots[2].bundle_id)
        .await
        .unwrap();
    let home = root.join("restored-home");
    for round in 0..2 {
        let plan = noland_restore::prepare_restore(
            &storage,
            &master,
            &agent.config.paths,
            &app,
            snapshots[2].bundle_id,
            RestoreMode::CompleteApplication,
        )
        .await
        .unwrap();
        let started = Instant::now();
        let mut last = noland_restore::PipelineProgress::default();
        let report = noland_restore::restore_pipelined(
            &storage,
            &master,
            &plan,
            &index,
            &LogicalRootMap::from_home(&home),
            None,
            None,
            noland_restore::PipelineOptions::default(),
            |sample| {
                last = sample;
                Ok(())
            },
        )
        .await
        .unwrap();
        let duration = started.elapsed();
        println!(
            "STORAGE_SCALE {}",
            serde_json::json!({
                "phase": (["restore", "unchanged_restore"][round]), "files": count,
                "elapsed_ms": duration.as_millis(), "files_per_second": count as f64 / duration.as_secs_f64(),
                "assembled_bytes": last.assembled_bytes, "readback_bytes": last.readback_bytes,
                "verified_bytes": last.verified_bytes, "io": report.io,
                "published_bytes": last.published_bytes, "reused_files": report.restore.reused_entries,
                "packs_downloaded": report.packs_downloaded,
                "filesystem_peak_delta_bytes": sampler.peak.load(Ordering::Relaxed),
            })
        );
        if round == 0 {
            assert_eq!(report.restore.published_entries, count);
        } else {
            assert_eq!(report.restore.reused_entries, count);
            assert_eq!(report.packs_downloaded, 0);
        }
        assert!(!plan.staging.exists());
    }
    let restored_save = home.join(changed_path.strip_prefix(&agent.config.home).unwrap());
    assert_eq!(std::fs::read(restored_save).unwrap(), b"one changed save");
    drop(sampler);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
