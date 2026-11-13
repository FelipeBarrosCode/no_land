//! Upload supervision. Byte progress, local observer work, provider finalization
//! and process/pipe exit are separate phases with separate deadlines.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UploadTransferDiagnostics {
    pub attempt: u32,
    pub attempt_bytes: u64,
    /// Rclone-reported payload bytes, including failed attempts (not protocol overhead).
    pub sent_bytes: u64,
    pub transfer_wait_us: u64,
    pub observer_us: u64,
    pub local_io_wait_us: u64,
    pub finalization_wait_us: u64,
    pub process_exit_wait_us: u64,
    pub limiter_wait_us: u64,
    pub retry_wait_us: u64,
    pub idle_restarts: u32,
    pub slow_restarts: u32,
    pub finalization_restarts: u32,
    pub exit_restarts: u32,
    pub completed_objects: usize,
    pub phase: String,
}

#[derive(Clone, Copy)]
pub(super) struct UploadPolicy {
    pub idle: Duration,
    pub finalization: Duration,
    pub exit: Duration,
    pub slow_window: Duration,
    pub tick: Duration,
    /// Production samples kernel pressure; fixtures must not depend on host load.
    pub io_pressure: fn() -> bool,
}

impl Default for UploadPolicy {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(60),
            finalization: Duration::from_secs(300),
            exit: Duration::from_secs(30),
            slow_window: Duration::from_secs(30),
            tick: Duration::from_millis(500),
            io_pressure,
        }
    }
}

fn copied_object(value: &serde_json::Value) -> Option<&str> {
    // Only rclone's post-verification success messages are receipts. Stats at
    // 100%, arbitrary log text and a failed process are never completion receipts.
    let message = value.get("msg")?.as_str()?;
    (value.get("level")?.as_str()? == "info" && message.starts_with("Copied ("))
        .then(|| value.get("object")?.as_str())
        .flatten()
}

fn io_pressure() -> bool {
    std::fs::read_to_string("/proc/pressure/io")
        .ok()
        .is_some_and(|text| {
            text.lines()
                .filter(|line| line.starts_with("some "))
                .any(|line| {
                    line.split_whitespace()
                        .find_map(|part| part.strip_prefix("avg10="))
                        .and_then(|value| value.parse::<f64>().ok())
                        .is_some_and(|value| value >= 5.0)
                })
        })
}

impl RcloneStorage {
    /// One retry owner. Internal rclone retries are disabled so all attempts are
    /// visible, bounded, and reaped before replaying the same sealed ciphertext.
    pub(super) async fn run_upload(
        &self,
        mut args: Vec<String>,
        objects: &[ImmutableUpload],
        observer: Option<&dyn ImmutableUploadObserver>,
        mut release: impl FnMut(&ImmutableUpload, u64) -> Result<()>,
        limiter_wait: Duration,
        policy: UploadPolicy,
    ) -> Result<Duration> {
        forbid_rclone_sync(&args)?;
        if let Some(arg) =
            self.extra_args.iter().chain(args.iter()).find(|arg| {
                !noland_rclone_adapter::provider_arg_is_allowed(self.provider_kind, arg)
            })
        {
            return Err(StateError::Invalid(format!(
                "invalid provider option {arg}"
            )));
        }
        args.extend([
            "--retries".into(),
            "1".into(),
            "--low-level-retries".into(),
            "1".into(),
            "--log-level".into(),
            "INFO".into(),
            "--stats".into(),
            "500ms".into(),
            "--use-json-log".into(),
        ]);
        let sizes = objects
            .iter()
            .map(|upload| {
                Ok((
                    upload.key.as_str().to_owned(),
                    std::fs::metadata(&upload.local)?.len(),
                ))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let mut completed = HashSet::new();
        let mut diagnostics = UploadTransferDiagnostics {
            limiter_wait_us: stream::micros(limiter_wait),
            ..Default::default()
        };
        for attempt in 1..=self.tuning.max_attempts.max(1) {
            let wait = Instant::now();
            self.retry_gate.wait_for_turn().await;
            diagnostics.retry_wait_us += stream::micros(wait.elapsed());
            diagnostics.attempt = attempt;
            diagnostics.attempt_bytes = 0;
            let expected = sizes
                .iter()
                .filter(|(key, _)| !completed.contains(*key))
                .map(|(_, size)| *size)
                .sum::<u64>();
            diagnostics.phase = "starting".into();
            let local = Instant::now();
            if let Some(observer) = observer {
                observer.transfer_started()?;
                observer.transfer_diagnostics(&diagnostics)?;
            }
            diagnostics.observer_us += stream::micros(local.elapsed());
            self.metrics
                .rclone_invocations
                .fetch_add(1, Ordering::Relaxed);
            #[cfg(test)]
            let mut command = if let Some(script) = &self.upload_executable {
                let mut command = Command::new("/bin/sh");
                command.arg(script);
                command
            } else {
                Command::new("rclone")
            };
            #[cfg(not(test))]
            let mut command = Command::new("rclone");
            command
                .args(&self.extra_args)
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = ReapedChild(command.spawn()?);
            let mut lines = BufReader::new(child.stderr.take().expect("piped stderr")).lines();
            let started = Instant::now();
            let observer_before = diagnostics.observer_us;
            let mut last_progress = started;
            let mut last_tick = started;
            let mut slow_start = started;
            let mut slow_bytes = 0;
            let mut finalizing_since = None;
            let mut exit_since = None;
            let mut status = None;
            let mut eof = false;
            let mut error_output = String::new();
            let mut attempt_copied = 0u64;
            let mut tick = tokio::time::interval(policy.tick);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let outcome: Result<Option<RcloneFailure>> = async {
                loop {
                    if eof && status.is_some() { break; }
                    tokio::select! {
                        line = lines.next_line(), if !eof => {
                            match line? {
                                None => { eof = true; exit_since.get_or_insert(Instant::now()); }
                                Some(line) => {
                                    if line.len() > 256 * 1024 { return Err(StateError::Storage("oversized rclone progress record".into())); }
                                    let value = serde_json::from_str::<serde_json::Value>(&line).ok();
                                    if let Some(bytes) = value.as_ref().and_then(|v| v.get("stats")).and_then(|s| s.get("bytes")).and_then(|b| b.as_u64()) {
                                        let delta = bytes.saturating_sub(diagnostics.attempt_bytes);
                                        diagnostics.attempt_bytes = diagnostics.attempt_bytes.max(bytes);
                                        diagnostics.sent_bytes = diagnostics.sent_bytes.saturating_add(delta);
                                        self.metrics.bytes_uploaded.fetch_add(delta, Ordering::Relaxed);
                                        if delta > 0 { last_progress = Instant::now(); }
                                        let object_finalizing = value.as_ref().and_then(|v| v.get("stats")).and_then(|s| s.get("transferring")).and_then(|v| v.as_array()).is_some_and(|transfers| transfers.iter().any(|transfer| {
                                            transfer.get("percentage").and_then(|v| v.as_f64()).is_some_and(|percent| percent >= 100.0)
                                                || transfer.get("size").and_then(|v| v.as_u64()).is_some_and(|size| size > 0 && transfer.get("bytes").and_then(|v| v.as_u64()).is_some_and(|bytes| bytes >= size))
                                        }));
                                        if bytes >= expected || object_finalizing {
                                            finalizing_since.get_or_insert(Instant::now());
                                        } else {
                                            finalizing_since = None;
                                        }
                                    } else if let Some(key) = value.as_ref().and_then(copied_object) {
                                        if let Some(upload) = objects.iter().find(|upload| upload.key.as_str() == key) {
                                            if completed.insert(key.to_owned()) {
                                                attempt_copied = attempt_copied.saturating_add(sizes[key]);
                                                let local = Instant::now();
                                                if let Some(observer) = observer { observer.completed(std::slice::from_ref(upload), ImmutableUploadCompletion::Uploaded)?; }
                                                release(upload, sizes[key])?;
                                                let elapsed = local.elapsed();
                                                diagnostics.observer_us += stream::micros(elapsed);
                                                last_progress = Instant::now();
                                                slow_start += elapsed;
                                                last_tick += elapsed;
                                                diagnostics.completed_objects = completed.len();
                                                if completed.len() == objects.len() { exit_since.get_or_insert(Instant::now()); }
                                            }
                                        }
                                    } else {
                                        error_output.push_str(&line);
                                        error_output.push('\n');
                                        if error_output.len() > 16 * 1024 {
                                            let mut boundary = error_output.len() - 16 * 1024;
                                            while !error_output.is_char_boundary(boundary) { boundary += 1; }
                                            error_output.drain(..boundary);
                                        }
                                    }
                                }
                            }
                        }
                        result = child.wait(), if status.is_none() => {
                            status = Some(result?); exit_since.get_or_insert(Instant::now());
                        }
                        _ = tick.tick() => {
                            let now = Instant::now();
                            let elapsed = stream::micros(now.saturating_duration_since(last_tick));
                            last_tick = now;
                            let local_io = (policy.io_pressure)();
                            diagnostics.phase = if exit_since.is_some() {
                                diagnostics.process_exit_wait_us += elapsed; "waiting_exit"
                            } else if finalizing_since.is_some() {
                                diagnostics.finalization_wait_us += elapsed; "provider_finalizing"
                            } else if local_io {
                                diagnostics.local_io_wait_us += elapsed; "local_io_pressure"
                            } else {
                                diagnostics.transfer_wait_us += elapsed; "transferring"
                            }.into();
                            let reason = if exit_since.is_some_and(|time| time.elapsed() >= policy.exit) {
                                diagnostics.exit_restarts += 1; Some("upload process or stderr exit timeout")
                            } else if finalizing_since.is_some_and(|time| time.elapsed() >= policy.finalization) {
                                diagnostics.finalization_restarts += 1; Some("upload provider finalization timeout")
                            } else if finalizing_since.is_none() && exit_since.is_none() && last_progress.elapsed() >= policy.idle && !local_io {
                                diagnostics.idle_restarts += 1; Some("upload byte-progress idle timeout")
                            } else { None };
                            let slow = finalizing_since.is_none() && exit_since.is_none() && diagnostics.slow_restarts == 0
                                && slow_start.elapsed() >= policy.slow_window && !local_io
                                && self.upload_peers.baseline().is_some_and(|baseline| baseline >= 256.0 * 1024.0
                                    && (diagnostics.attempt_bytes.saturating_sub(slow_bytes) as f64 / slow_start.elapsed().as_secs_f64()) < baseline / 8.0);
                            let reason = reason.or_else(|| if slow { diagnostics.slow_restarts += 1; Some("upload slower than healthy peers") } else { None });
                            if slow_start.elapsed() >= policy.slow_window { slow_start = now; slow_bytes = diagnostics.attempt_bytes; }
                            let local = Instant::now();
                            if let Some(observer) = observer { observer.transfer_diagnostics(&diagnostics)?; }
                            let elapsed = local.elapsed();
                            diagnostics.observer_us += stream::micros(elapsed);
                            last_progress += elapsed; slow_start += elapsed; last_tick += elapsed;
                            if let Some(time) = finalizing_since.as_mut() { *time += elapsed; }
                            if let Some(time) = exit_since.as_mut() { *time += elapsed; }
                            if let Some(reason) = reason { return Ok(Some(RcloneFailure { class: RemoteErrorClass::Retryable, message: reason.into() })); }
                        }
                    }
                }
                let status = status.expect("exit observed");
                Ok(if status.success() { None } else {
                    Some(RcloneFailure { class: classify_remote_error(status.code(), &error_output), message: error_output.trim().into() })
                })
            }.await;
            if !matches!(&outcome, Ok(None)) {
                kill_and_reap_for_retry(&mut child).await?;
            }
            if diagnostics.attempt_bytes < attempt_copied {
                let delta = attempt_copied - diagnostics.attempt_bytes;
                self.metrics
                    .bytes_uploaded
                    .fetch_add(delta, Ordering::Relaxed);
                diagnostics.sent_bytes += delta;
                diagnostics.attempt_bytes = attempt_copied;
            }
            let failure = match outcome {
                Ok(failure) => failure,
                Err(error) => return Err(error),
            };
            if let Some(failure) = failure {
                diagnostics.phase = "retrying".into();
                if let Some(observer) = observer {
                    observer.transfer_diagnostics(&diagnostics)?;
                }
                self.record_remote_error(failure.class);
                if !failure.class.is_retryable() || attempt == self.tuning.max_attempts.max(1) {
                    return Err(failure.into_state_error());
                }
                // Completed objects stay durable. Remove only our staging links
                // after reaping, so a retry cannot consume released group slots.
                for upload in objects
                    .iter()
                    .filter(|upload| completed.contains(upload.key.as_str()))
                {
                    match std::fs::remove_file(&upload.local) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                let wait = Instant::now();
                self.retry_gate.wait_for_retry(attempt, failure.class).await;
                diagnostics.retry_wait_us += stream::micros(wait.elapsed());
                continue;
            }
            // Fast copies may exit before the first stats tick. Successful new
            // copies give a conservative payload lower bound without counting
            // reused immutable remote objects as new traffic.
            diagnostics.phase = "complete".into();
            if let Some(observer) = observer {
                let reused = objects
                    .iter()
                    .filter(|upload| !completed.contains(upload.key.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                if !reused.is_empty() {
                    observer.completed(&reused, ImmutableUploadCompletion::ReusedRemote)?;
                }
            }
            if let Some(observer) = observer {
                observer.transfer_diagnostics(&diagnostics)?;
            }
            let elapsed = started.elapsed();
            if !(policy.io_pressure)() {
                let transfer = finalizing_since
                    .unwrap_or_else(Instant::now)
                    .saturating_duration_since(started)
                    .saturating_sub(Duration::from_micros(
                        diagnostics.observer_us.saturating_sub(observer_before),
                    ));
                self.upload_peers
                    .record(diagnostics.attempt_bytes, transfer, Duration::ZERO);
                self.upload_limiter
                    .record_success(diagnostics.attempt_bytes, transfer);
            }
            return Ok(elapsed);
        }
        unreachable!("at least one upload attempt")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Observer {
        snapshots: Mutex<Vec<UploadTransferDiagnostics>>,
        completed: Mutex<Vec<String>>,
        local_delay: Duration,
    }
    impl ImmutableUploadObserver for Observer {
        fn transfer_diagnostics(&self, value: &UploadTransferDiagnostics) -> Result<()> {
            self.snapshots.lock().unwrap().push(value.clone());
            std::thread::sleep(self.local_delay);
            Ok(())
        }
        fn completed(
            &self,
            uploads: &[ImmutableUpload],
            _: ImmutableUploadCompletion,
        ) -> Result<()> {
            self.completed
                .lock()
                .unwrap()
                .extend(uploads.iter().map(|u| u.key.0.clone()));
            Ok(())
        }
    }
    fn fixture(script: &str, attempts: u32) -> (PathBuf, RcloneStorage, Vec<ImmutableUpload>) {
        let root = std::env::temp_dir().join(format!("upload-watchdog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("fake.sh");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\ncd '{}'\n{script}\n", root.display()),
        )
        .unwrap();
        let mut storage =
            RcloneStorage::new("fixture", "root").with_transfer_tuning(TransferTuning {
                max_attempts: attempts,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
                ..Default::default()
            });
        storage.upload_executable = Some(executable);
        let file = root.join("pack");
        std::fs::write(&file, b"12345678").unwrap();
        (
            root,
            storage,
            vec![ImmutableUpload::new(
                file,
                RemoteKey::new("packs/test.pack"),
            )],
        )
    }
    fn policy() -> UploadPolicy {
        UploadPolicy {
            idle: Duration::from_millis(80),
            finalization: Duration::from_millis(400),
            exit: Duration::from_millis(80),
            slow_window: Duration::from_millis(50),
            tick: Duration::from_millis(10),
            io_pressure: || false,
        }
    }
    async fn run(
        storage: &RcloneStorage,
        objects: &[ImmutableUpload],
        observer: &Observer,
        policy: UploadPolicy,
    ) -> Result<Duration> {
        tokio::time::timeout(
            Duration::from_secs(5),
            storage.run_upload(
                vec!["copy".into()],
                objects,
                Some(observer),
                |_, _| Ok(()),
                Duration::ZERO,
                policy,
            ),
        )
        .await
        .expect("bounded upload")
    }

    #[tokio::test]
    async fn idle_retry_reaps_old_process_and_counts_failed_attempt_payload() {
        let (root, storage, objects) = fixture(
            r#"
if [ ! -f previous ]; then
  echo $$ > previous
  echo '{"stats":{"bytes":3}}' >&2
  exec sleep 10
fi
if kill -0 "$(cat previous)" 2>/dev/null; then echo 'old process still alive' >&2; exit 9; fi
echo '{"stats":{"bytes":8}}' >&2
echo '{"level":"info","object":"packs/test.pack","msg":"Copied (new)"}' >&2
"#,
            2,
        );
        let observer = Observer::default();
        run(&storage, &objects, &observer, policy()).await.unwrap();
        let snapshots = observer.snapshots.lock().unwrap();
        let last = snapshots.last().unwrap();
        assert_eq!(
            (last.attempt, last.sent_bytes, last.idle_restarts),
            (2, 11, 1)
        );
        assert_eq!(storage.operation_metrics().bytes_uploaded, 11);
        assert_eq!(std::fs::read(&objects[0].local).unwrap(), b"12345678");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn provider_finalization_and_local_observer_wait_are_not_network_stalls() {
        for delay in [Duration::ZERO, Duration::from_millis(100)] {
            let (root, storage, objects) = fixture(
                r#"
echo '{"stats":{"bytes":8}}' >&2
sleep 0.16
echo '{"level":"info","object":"packs/test.pack","msg":"Copied (new)"}' >&2
"#,
                1,
            );
            let observer = Observer {
                local_delay: delay,
                ..Default::default()
            };
            run(&storage, &objects, &observer, policy()).await.unwrap();
            let snapshots = observer.snapshots.lock().unwrap();
            if delay.is_zero() {
                assert!(snapshots.iter().any(|d| d.phase == "provider_finalizing"));
            }
            assert_eq!(snapshots.last().unwrap().idle_restarts, 0);
            assert!(snapshots.last().unwrap().observer_us > 0);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn local_io_pressure_suppresses_idle_and_slow_peer_restarts() {
        let (root, storage, objects) = fixture(
            r#"
echo '{"stats":{"bytes":1}}' >&2
sleep 0.16
echo '{"stats":{"bytes":8}}' >&2
echo '{"level":"info","object":"packs/test.pack","msg":"Copied (new)"}' >&2
"#,
            1,
        );
        for _ in 0..3 {
            storage
                .upload_peers
                .record(8 * 1024 * 1024, Duration::from_secs(1), Duration::ZERO);
        }
        let observer = Observer::default();
        let pressured = UploadPolicy {
            io_pressure: || true,
            ..policy()
        };
        run(&storage, &objects, &observer, pressured).await.unwrap();
        let snapshots = observer.snapshots.lock().unwrap();
        assert!(snapshots.iter().any(|d| d.local_io_wait_us > 0));
        let last = snapshots.last().unwrap();
        assert_eq!((last.idle_restarts, last.slow_restarts), (0, 0));
        assert_eq!(last.sent_bytes, 8);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn finalization_and_inherited_stderr_have_separate_bounded_deadlines() {
        for (script, field) in [
            (
                "echo '{\"stats\":{\"bytes\":8}}' >&2\nexec sleep 10",
                "finalization",
            ),
            ("sleep 0.3 &\nexit 0", "exit"),
        ] {
            let (root, storage, objects) = fixture(script, 1);
            let observer = Observer::default();
            let error = run(&storage, &objects, &observer, policy())
                .await
                .unwrap_err();
            assert!(error.to_string().contains(field), "{error}");
            assert!(observer.completed.lock().unwrap().is_empty());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn immutable_conflicts_do_not_retry_or_publish_stats_as_completions() {
        let (root, storage, objects) = fixture(
            "echo '{\"stats\":{\"bytes\":8}}' >&2\necho 'immutable file modified' >&2\nexit 3",
            4,
        );
        let observer = Observer::default();
        assert!(run(&storage, &objects, &observer, policy()).await.is_err());
        assert_eq!(storage.operation_metrics().rclone_invocations, 1);
        assert!(observer.completed.lock().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn pack_sessions_use_targeted_immutable_checks_without_a_repository_inventory() {
        let (root, storage, objects) = fixture(
            r#"
case " $* " in *" --no-traverse "*) ;; *) exit 9 ;; esac
case " $* " in *" --immutable "*) ;; *) exit 9 ;; esac
case " $* " in *" --retries 1 "*) ;; *) exit 9 ;; esac
echo '{"level":"info","object":"packs/te/test.pack","msg":"Copied (new)"}' >&2
"#,
            1,
        );
        storage.root_ready.set(()).unwrap();
        crate::PackUploadSession::new(&storage, None, None)
            .upload(&[("test".into(), objects[0].local.clone())])
            .await
            .unwrap();
        let metrics = storage.operation_metrics();
        assert_eq!(metrics.remote_list_calls, 0);
        assert_eq!(metrics.remote_stat_calls, 0);
        assert_eq!(metrics.rclone_invocations, 1);
        assert_eq!(metrics.bytes_uploaded, 8);
        assert_eq!(std::fs::read(&objects[0].local).unwrap(), b"12345678");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn finalizing_one_object_does_not_require_the_whole_group_to_reach_100_percent() {
        let (root, storage, mut objects) = fixture(
            r#"
echo '{"stats":{"bytes":8,"transferring":[{"name":"packs/test.pack","bytes":8,"size":8,"percentage":100}]}}' >&2
sleep 0.16
echo '{"level":"info","object":"packs/test.pack","msg":"Copied (new)"}' >&2
echo '{"stats":{"bytes":12}}' >&2
echo '{"level":"info","object":"packs/second.pack","msg":"Copied (new)"}' >&2
"#,
            1,
        );
        std::fs::write(root.join("second"), b"more").unwrap();
        objects.push(ImmutableUpload::new(
            root.join("second"),
            RemoteKey::new("packs/second.pack"),
        ));
        let observer = Observer::default();
        run(&storage, &objects, &observer, policy()).await.unwrap();
        assert_eq!(observer.completed.lock().unwrap().len(), 2);
        assert_eq!(
            observer
                .snapshots
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .idle_restarts,
            0
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_group_releases_only_confirmed_objects_and_retry_omits_their_staging_links() {
        let (root, storage, mut objects) = fixture(
            r#"
if [ ! -f retried ]; then
  touch retried
  echo '{"level":"info","object":"packs/test.pack","msg":"Copied (new)"}' >&2
  echo 'connection reset' >&2
  exit 5
fi
test ! -f pack || exit 9
test -f second || exit 9
echo '{"level":"info","object":"packs/second.pack","msg":"Copied (new)"}' >&2
"#,
            2,
        );
        std::fs::write(root.join("second"), b"more").unwrap();
        objects.push(ImmutableUpload::new(
            root.join("second"),
            RemoteKey::new("packs/second.pack"),
        ));
        let observer = Observer::default();
        let mut released = Vec::new();
        storage
            .run_upload(
                vec!["copy".into()],
                &objects,
                Some(&observer),
                |u, bytes| {
                    released.push((u.key.clone(), bytes));
                    Ok(())
                },
                Duration::ZERO,
                policy(),
            )
            .await
            .unwrap();
        assert_eq!(released.len(), 2);
        assert_eq!(storage.operation_metrics().bytes_uploaded, 12);
        assert_eq!(observer.completed.lock().unwrap().len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn slow_peer_restart_is_allowed_only_once() {
        let (root, storage, objects) =
            fixture("echo '{\"stats\":{\"bytes\":1}}' >&2\nexec sleep 10", 2);
        for _ in 0..3 {
            storage
                .upload_peers
                .record(8 * 1024 * 1024, Duration::from_secs(1), Duration::ZERO);
        }
        let observer = Observer::default();
        assert!(run(&storage, &objects, &observer, policy()).await.is_err());
        let snapshots = observer.snapshots.lock().unwrap();
        let last = snapshots.last().unwrap();
        assert_eq!(last.slow_restarts, 1);
        assert_eq!(last.idle_restarts, 1);
        assert_eq!(last.sent_bytes, 2);
        std::fs::remove_dir_all(root).unwrap();
    }
}
