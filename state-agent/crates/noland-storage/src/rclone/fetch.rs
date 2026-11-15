//! Exact small-object reads with bounded pipes and one retry owner. No plaintext,
//! stderr, command arguments, credentials or signed URLs enter errors/telemetry.

use super::*;
use crate::fetch::{absent, validate_small_read, SmallReadBuffer};
use std::future::Future;
use std::time::Instant;
use tokio::io::AsyncWriteExt;

#[derive(Default)]
pub(super) struct MetadataReadCounters {
    read_calls: AtomicU64,
    attempts: AtomicU64,
    bytes_received: AtomicU64,
    process_wait_us: AtomicU64,
    local_buffer_us: AtomicU64,
    admission_wait_us: AtomicU64,
    retry_wait_us: AtomicU64,
}

impl MetadataReadCounters {
    pub(super) fn snapshot(&self) -> crate::MetadataReadMetrics {
        crate::MetadataReadMetrics {
            read_calls: self.read_calls.load(Ordering::Relaxed),
            attempts: self.attempts.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            process_wait_us: self.process_wait_us.load(Ordering::Relaxed),
            local_buffer_us: self.local_buffer_us.load(Ordering::Relaxed),
            admission_wait_us: self.admission_wait_us.load(Ordering::Relaxed),
            retry_wait_us: self.retry_wait_us.load(Ordering::Relaxed),
        }
    }
}

struct WaitTimer<'a> {
    counter: &'a AtomicU64,
    started: Instant,
}

impl Drop for WaitTimer<'_> {
    fn drop(&mut self) {
        self.counter
            .fetch_add(stream::micros(self.started.elapsed()), Ordering::Relaxed);
    }
}

async fn measured<T>(counter: &AtomicU64, future: impl Future<Output = T>) -> T {
    let _timer = WaitTimer {
        counter,
        started: Instant::now(),
    };
    future.await
}

struct ProcessTimer<'a> {
    metrics: &'a MetadataReadCounters,
    local_us: &'a AtomicU64,
    started: Instant,
}

impl Drop for ProcessTimer<'_> {
    fn drop(&mut self) {
        self.metrics.process_wait_us.fetch_add(
            stream::micros(self.started.elapsed())
                .saturating_sub(self.local_us.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
    }
}

enum ReadFailure {
    Local(StateError),
    Remote(RemoteErrorClass),
}

/// Scan *all* stderr while retaining only a bounded tail. An early auth/network
/// failure must not disappear behind a long suffix containing "not found".
struct ReadErrors {
    tail: Vec<u8>,
    signal: Option<RemoteErrorClass>,
}

fn priority(class: RemoteErrorClass) -> u8 {
    match class {
        RemoteErrorClass::Authentication => 4,
        RemoteErrorClass::ImmutableConflict => 3,
        RemoteErrorClass::RateLimited => 2,
        RemoteErrorClass::Retryable => 1,
        _ => 0,
    }
}

async fn read_errors(mut stderr: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<ReadErrors> {
    let mut errors = ReadErrors {
        tail: Vec::new(),
        signal: None,
    };
    let mut buffer = [0; 4096];
    loop {
        let count = stderr.read(&mut buffer).await?;
        if count == 0 {
            return Ok(errors);
        }
        errors.tail.extend_from_slice(&buffer[..count]);
        let class = classify_remote_error(None, &String::from_utf8_lossy(&errors.tail));
        if priority(class) > errors.signal.map(priority).unwrap_or(0) {
            errors.signal = Some(class);
        }
        if errors.tail.len() > 16 * 1024 {
            errors.tail.drain(..errors.tail.len() - 16 * 1024);
        }
    }
}

async fn read_stdout(
    mut stdout: impl tokio::io::AsyncRead + Unpin,
    bound: usize,
    counters: &StorageCounters,
    local_us: &AtomicU64,
) -> Result<Vec<u8>> {
    let mut bytes = SmallReadBuffer::new(bound);
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = bytes.next_read_len(buffer.len());
        let count = stdout.read(&mut buffer[..count]).await?;
        if count == 0 {
            return Ok(bytes.finish());
        }
        counters
            .bytes_downloaded
            .fetch_add(count as u64, Ordering::Relaxed);
        counters
            .metadata_reads
            .bytes_received
            .fetch_add(count as u64, Ordering::Relaxed);
        let local = Instant::now();
        let result = bytes.push(&buffer[..count]);
        let elapsed = stream::micros(local.elapsed());
        local_us.fetch_add(elapsed, Ordering::Relaxed);
        counters
            .metadata_reads
            .local_buffer_us
            .fetch_add(elapsed, Ordering::Relaxed);
        result?;
    }
}

impl RcloneStorage {
    async fn small_read_attempt(
        &self,
        args: &[String],
        key: &RemoteKey,
        bound: usize,
    ) -> std::result::Result<Vec<u8>, ReadFailure> {
        let local_us = AtomicU64::new(0);
        let _timer = ProcessTimer {
            metrics: &self.metrics.metadata_reads,
            local_us: &local_us,
            started: Instant::now(),
        };
        self.metrics
            .rclone_invocations
            .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .metadata_reads
            .attempts
            .fetch_add(1, Ordering::Relaxed);
        let mut command = self.command(true);
        command
            .args(&self.extra_args)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = ReapedChild(command.spawn().map_err(|error| {
            ReadFailure::Remote(classify_remote_error(None, &error.to_string()))
        })?);
        let mut stdin = child.stdin.take().expect("piped metadata stdin");
        let stdout = child.stdout.take().expect("piped metadata stdout");
        let stderr = child.stderr.take().expect("piped metadata stderr");
        let input = async move {
            stdin.write_all(key.as_str().as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.shutdown().await?;
            drop(stdin); // files-from must see EOF before rclone opens the object
            Ok::<_, std::io::Error>(())
        };
        let result = tokio::time::timeout(self.control_timeout, async {
            // A limit violation aborts immediately, not after draining an
            // unbounded stdout or waiting for the child to finish producing it.
            let (bytes, (errors, status, input)) = tokio::try_join!(
                read_stdout(stdout, bound, &self.metrics, &local_us),
                async {
                    let (errors, status, input) =
                        tokio::join!(read_errors(stderr), child.wait(), input);
                    Ok::<_, StateError>((errors?, status?, input))
                },
            )?;
            Ok::<_, StateError>((bytes, errors, status, input))
        })
        .await;
        let (bytes, errors, status, input) = match result {
            Ok(Ok(result)) => result,
            failed => {
                // No retry may overlap a still-live child using this session.
                kill_and_reap_for_retry(&mut child)
                    .await
                    .map_err(ReadFailure::Local)?;
                return Err(match failed {
                    Ok(Err(StateError::Io(_))) | Err(_) => {
                        ReadFailure::Remote(RemoteErrorClass::Retryable)
                    }
                    Ok(Err(error)) => ReadFailure::Local(error),
                    Ok(Ok(_)) => unreachable!(),
                });
            }
        };
        if status.success() {
            if input.is_err() {
                return Err(ReadFailure::Remote(RemoteErrorClass::Retryable));
            }
            return Ok(bytes);
        }
        let mut class = errors.signal.unwrap_or_else(|| {
            classify_remote_error(status.code(), &String::from_utf8_lossy(&errors.tail))
        });
        // --files-from skips missing exact objects; --error-on-no-transfer
        // provides a structured absence status even on virtual-directory/S3
        // backends. Only this unfiltered single-object command interprets 9.
        // Existing empty objects count as transfers and succeed with zero bytes.
        if status.code() == Some(9) && bytes.is_empty() && errors.tail.is_empty() && input.is_ok() {
            class = RemoteErrorClass::NotFound;
        }
        if class == RemoteErrorClass::NotFound && (!bytes.is_empty() || input.is_err()) {
            // Never turn partial output or an undelivered exact key into absence.
            class = RemoteErrorClass::Permanent;
        }
        Err(ReadFailure::Remote(class))
    }

    pub(super) async fn read_small_bounded(&self, key: &RemoteKey, limit: u64) -> Result<Vec<u8>> {
        let bound = validate_small_read(key, limit)?;
        let args = self.command_builder.read_small_args(self.root_remote());
        forbid_rclone_sync(&args)?;
        if self
            .extra_args
            .iter()
            .chain(&args)
            .any(|arg| !noland_rclone_adapter::provider_arg_is_allowed(self.provider_kind, arg))
        {
            return Err(StateError::Invalid(
                "unsupported provider metadata option".into(),
            ));
        }
        let counters = &self.metrics.metadata_reads;
        counters.read_calls.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .remote_download_calls
            .fetch_add(1, Ordering::Relaxed);
        self.download_limiter
            .observe_local_pressure((self.stream_policy.io_pressure)());
        let _permit =
            measured(&counters.admission_wait_us, self.download_limiter.acquire()).await?;
        for attempt in 1..=self.tuning.max_attempts.max(1) {
            measured(&counters.retry_wait_us, self.retry_gate.wait_for_turn()).await;
            match self.small_read_attempt(&args, key, bound).await {
                Ok(bytes) => {
                    // Control traffic does not train pack throughput/admission.
                    self.download_limiter
                        .observe_local_pressure((self.stream_policy.io_pressure)());
                    return Ok(bytes);
                }
                Err(ReadFailure::Local(error)) => return Err(error),
                Err(ReadFailure::Remote(class)) => {
                    self.record_remote_error(class);
                    if class.is_retryable() && attempt < self.tuning.max_attempts.max(1) {
                        measured(
                            &counters.retry_wait_us,
                            self.retry_gate.wait_for_retry(attempt, class),
                        )
                        .await;
                        continue;
                    }
                    if class == RemoteErrorClass::NotFound {
                        return Err(absent());
                    }
                    // Never expose backend stderr (tokens, signed URLs, config).
                    return Err(StateError::Storage(format!(
                        "rclone metadata read failed ({class:?}); provider diagnostics withheld"
                    )));
                }
            }
        }
        unreachable!("metadata reads always make at least one attempt")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Fixture {
        root: PathBuf,
        storage: RcloneStorage,
    }

    impl Fixture {
        fn new(body: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("noland-small-process-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let script = root.join("fixture");
            std::fs::write(&script, format!(
                "set -eu\nprintf '%s\\n' \"$$\" >> {pids:?}\nprintf '%s\\n' \"$@\" > {args:?}\nIFS= read -r key\nif IFS= read -r extra; then exit 77; fi\nprintf '%s' \"$key\" > {key:?}\n{body}\n",
                pids = root.join("pids"), args = root.join("args"), key = root.join("key"),
            )).unwrap();
            let mut storage = RcloneStorage::new("fixture", "approved-root").with_transfer_tuning(
                TransferTuning {
                    max_attempts: 2,
                    initial_backoff_ms: 1,
                    max_backoff_ms: 1,
                    rate_limit_cooldown_ms: 1,
                    ..Default::default()
                },
            );
            storage.control_executable = Some(script);
            storage.control_timeout = Duration::from_secs(2);
            storage.stream_policy.io_pressure = || false;
            Self { root, storage }
        }

        fn assert_reaped(&self, count: usize) {
            let pids = std::fs::read_to_string(self.root.join("pids")).unwrap();
            assert_eq!(pids.lines().count(), count);
            for pid in pids.lines() {
                let status = std::process::Command::new("kill")
                    .args(["-0", pid])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap();
                assert!(
                    !status.success(),
                    "metadata child {pid} is still live or unreaped"
                );
            }
            assert_eq!(self.storage.download_limiter.active(), 0);
            assert_eq!(self.storage.download_limiter.queued(), 0);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn direct_read_is_binary_exact_single_process_and_has_no_stat_or_temp_download() {
        let fixture = Fixture::new("printf '\\000\\377abc'");
        let key = RemoteKey::new("catalog/ literal[?];key");
        assert_eq!(
            fixture.storage.read_small(&key, 5).await.unwrap(),
            b"\0\xffabc"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("key")).unwrap(),
            key.as_str()
        );
        let args = std::fs::read_to_string(fixture.root.join("args")).unwrap();
        assert!(args.starts_with("cat\n--files-from-raw\n-\n"));
        assert!(args.contains("--error-on-no-transfer\n"));
        assert!(args.ends_with("fixture:approved-root\n"));
        let operations = fixture.storage.operation_metrics();
        assert_eq!(
            (
                operations.rclone_invocations,
                operations.remote_stat_calls,
                operations.remote_download_calls
            ),
            (1, 0, 1)
        );
        assert_eq!(operations.bytes_downloaded, 5);
        let metadata = fixture.storage.metadata_read_metrics();
        assert_eq!(
            (
                metadata.read_calls,
                metadata.attempts,
                metadata.bytes_received
            ),
            (1, 1, 5)
        );
        assert!(metadata.process_wait_us > 0);
        fixture.assert_reaped(1);
    }

    #[tokio::test]
    async fn oversize_reads_stop_at_one_probe_byte_and_do_not_retry_or_train_network_limits() {
        for limit in [0, 5] {
            let fixture = Fixture::new("printf abcdef; exec sleep 30");
            assert!(matches!(
                fixture
                    .storage
                    .read_small(&RemoteKey::new("catalog/test"), limit)
                    .await,
                Err(StateError::Integrity(_))
            ));
            assert_eq!(
                fixture.storage.metadata_read_metrics().bytes_received,
                limit + 1
            );
            assert_eq!(fixture.storage.metadata_read_metrics().attempts, 1);
            assert_eq!(fixture.storage.download_limiter.current(), 4);
            fixture.assert_reaped(1);
        }
    }

    #[tokio::test]
    async fn authoritative_notfound_is_distinct_from_empty_objects_auth_and_partial_failures() {
        for body in ["exit 9", "echo 'object not found' >&2; exit 4", "exit 3"] {
            let fixture = Fixture::new(body);
            assert!(matches!(
                fixture
                    .storage
                    .read_small(&RemoteKey::new("catalog/test"), 16)
                    .await,
                Err(StateError::NotFound(_))
            ));
            fixture.assert_reaped(1);
        }
        let empty = Fixture::new("exit 0");
        assert!(empty
            .storage
            .read_small(&RemoteKey::new("catalog/test"), 0)
            .await
            .unwrap()
            .is_empty());
        empty.assert_reaped(1);
        for body in [
            "echo 'object not found: 403 Forbidden signed-url=secret' >&2; exit 4",
            "echo 'permission denied token=secret' >&2; exit 9",
            "echo 'config file not found secret' >&2; exit 1",
            "printf abc; echo 'object not found secret' >&2; exit 4",
        ] {
            let fixture = Fixture::new(body);
            let error = fixture
                .storage
                .read_small(&RemoteKey::new("catalog/test"), 16)
                .await
                .unwrap_err();
            assert!(matches!(error, StateError::Storage(_)));
            assert!(!error.to_string().contains("secret"));
            fixture.assert_reaped(1);
        }
    }

    #[tokio::test]
    async fn early_auth_signals_survive_bounded_stderr_tail_and_never_leak_credentials() {
        let fixture = Fixture::new(
            "printf '403 Forbidden access_token=do-not-leak\\n' >&2\nn=0\nwhile [ \"$n\" -lt 3000 ]; do printf 'object not found padded diagnostic line....................\\n' >&2; n=$((n + 1)); done\nexit 4"
        );
        let error = fixture
            .storage
            .read_small(&RemoteKey::new("catalog/test"), 16)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Authentication"));
        assert!(!error.to_string().contains("do-not-leak"));
        fixture.assert_reaped(1);
    }

    #[tokio::test]
    async fn an_undelivered_exact_key_is_never_an_authoritative_absence() {
        let fixture = Fixture::new("exit 0");
        let script = fixture.storage.control_executable.as_ref().unwrap();
        std::fs::write(
            script,
            format!(
                "printf '%s\\n' \"$$\" >> {:?}\nexit 9\n",
                fixture.root.join("pids"),
            ),
        )
        .unwrap();
        // Larger than the pipe capacity, so early exit cannot accept the whole
        // request. No raw command/key contents may escape in the error.
        let key = RemoteKey::new(format!("catalog/{}", "x".repeat(1024 * 1024)));
        assert!(matches!(
            fixture.storage.read_small(&key, 0).await,
            Err(StateError::Storage(_))
        ));
        fixture.assert_reaped(1);
    }

    #[tokio::test]
    async fn retry_discards_partial_output_and_accounts_all_received_bytes() {
        let mut fixture = Fixture::new("exit 0");
        let body = format!(
            "if [ ! -f {once:?} ]; then\n : > {once:?}\n printf '%s' \"$$\" > {first:?}\n printf abc\n echo '429 rate limit token=secret' >&2\n exit 5\nfi\nif kill -0 \"$(cat {first:?})\" 2>/dev/null; then exit 77; fi\nprintf defghi",
            once=fixture.root.join("once"), first=fixture.root.join("first"),
        );
        // Preserve the standard fixture's exact-key/EOF checks.
        let script = fixture.storage.control_executable.as_ref().unwrap();
        let original = std::fs::read_to_string(script).unwrap();
        std::fs::write(
            script,
            original.strip_suffix("exit 0\n").unwrap().to_owned() + &body,
        )
        .unwrap();
        let before = fixture.storage.metadata_read_metrics();
        assert_eq!(
            fixture
                .storage
                .read_small(&RemoteKey::new("catalog/test"), 6)
                .await
                .unwrap(),
            b"defghi"
        );
        let metrics = fixture
            .storage
            .metadata_read_metrics()
            .saturating_sub(before);
        assert_eq!(
            (metrics.read_calls, metrics.attempts, metrics.bytes_received),
            (1, 2, 9)
        );
        assert!(metrics.retry_wait_us > 0);
        assert_eq!(fixture.storage.operation_metrics().bytes_downloaded, 9);
        assert_eq!(
            fixture.storage.download_limiter.current(),
            2,
            "real rate limits still reduce shared admission"
        );
        fixture.assert_reaped(2);
        // Invalid bounds fail before admission or process launch.
        assert!(matches!(
            fixture
                .storage
                .read_small(&RemoteKey::new("catalog/test"), u64::MAX)
                .await,
            Err(StateError::Invalid(_))
        ));
        assert_eq!(fixture.storage.metadata_read_metrics().attempts, 2);
        fixture.storage.extra_args.push("--drive-chunk-size".into());
        assert!(matches!(
            fixture
                .storage
                .read_small(&RemoteKey::new("catalog/test"), 16)
                .await,
            Err(StateError::Invalid(_))
        ));
        fixture.assert_reaped(2);
    }

    #[tokio::test]
    async fn read_timeout_kills_and_reaps_before_replaying_with_a_fresh_buffer() {
        let fixture = Fixture::new("exit 0");
        let body = format!(
            "if [ ! -f {once:?} ]; then\n : > {once:?}\n printf '%s' \"$$\" > {first:?}\n printf abc\n exec sleep 30\nfi\nif kill -0 \"$(cat {first:?})\" 2>/dev/null; then exit 77; fi\nprintf defghi",
            once=fixture.root.join("once"), first=fixture.root.join("first"),
        );
        let script = fixture.storage.control_executable.as_ref().unwrap();
        let original = std::fs::read_to_string(script).unwrap();
        std::fs::write(
            script,
            original.strip_suffix("exit 0\n").unwrap().to_owned() + &body,
        )
        .unwrap();
        assert_eq!(
            fixture
                .storage
                .read_small(&RemoteKey::new("catalog/test"), 6)
                .await
                .unwrap(),
            b"defghi"
        );
        assert_eq!(fixture.storage.metadata_read_metrics().bytes_received, 9);
        fixture.assert_reaped(2);
    }

    #[tokio::test]
    async fn cancellation_reaps_the_active_child_and_releases_admission() {
        let mut fixture = Fixture::new("printf abc; exec sleep 30");
        fixture.storage.control_timeout = Duration::from_secs(30);
        assert!(tokio::time::timeout(
            Duration::from_secs(2),
            fixture
                .storage
                .read_small(&RemoteKey::new("catalog/test"), 16),
        )
        .await
        .is_err());
        assert_eq!(fixture.storage.metadata_read_metrics().bytes_received, 3);
        assert!(fixture.storage.metadata_read_metrics().process_wait_us > 0);
        fixture.assert_reaped(1);
    }

    #[tokio::test]
    async fn metadata_uses_one_shared_slot_and_cancelled_queued_reads_never_spawn() {
        let fixture = Fixture::new("printf abc");
        let busy = fixture
            .storage
            .download_limiter
            .acquire_slots(3)
            .await
            .unwrap();
        assert_eq!(
            fixture
                .storage
                .read_small(&RemoteKey::new("catalog/test"), 3)
                .await
                .unwrap(),
            b"abc"
        );
        assert_eq!(fixture.storage.download_limiter.active(), 3);
        drop(busy);
        let all = fixture
            .storage
            .download_limiter
            .acquire_slots(4)
            .await
            .unwrap();
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            fixture
                .storage
                .read_small(&RemoteKey::new("catalog/test"), 3),
        )
        .await
        .is_err());
        assert_eq!(fixture.storage.metadata_read_metrics().attempts, 1);
        assert_eq!(fixture.storage.download_limiter.active(), 4);
        assert_eq!(fixture.storage.download_limiter.queued(), 0);
        drop(all);
        fixture.assert_reaped(1);
    }

    #[test]
    fn exact_metadata_command_is_provider_neutral_including_s3_compatible_fallbacks() {
        for provider in [
            ProviderKind::Local,
            ProviderKind::GoogleDrive,
            ProviderKind::MicrosoftOneDrive,
            ProviderKind::AmazonS3,
            ProviderKind::GenericS3,
            ProviderKind::CloudflareR2,
            ProviderKind::Wasabi,
            ProviderKind::DigitalOceanSpaces,
            ProviderKind::Sftp,
            ProviderKind::Webdav,
            ProviderKind::BackblazeB2,
        ] {
            let builder = RcloneCommandBuilder::new(
                provider,
                provider.backend_type(),
                TransferProfile::for_provider(provider, provider.backend_type()).unwrap(),
            )
            .unwrap();
            let args = builder.read_small_args("fixture:root".into());
            assert_eq!(args[0], "cat");
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--files-from-raw", "-"]));
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--low-level-retries", "1"]));
            assert!(args
                .iter()
                .all(|arg| noland_rclone_adapter::provider_arg_is_allowed(provider, arg)));
            assert!(!args
                .iter()
                .any(|arg| arg == "sync" || arg == "--head" || arg == "--count"));
            if provider != ProviderKind::GoogleDrive {
                assert!(!args.iter().any(|arg| arg.starts_with("--drive-")));
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires rclone on PATH; fresh local fixture only, no cloud credentials"]
    async fn real_rclone_exact_metadata_handles_absence_empty_directory_and_literal_names() {
        let root = std::env::temp_dir().join(format!("noland-small-real-{}", uuid::Uuid::new_v4()));
        let remote = root.join("cloud");
        std::fs::create_dir_all(remote.join("catalog/dir")).unwrap();
        let config = root.join("local.conf");
        std::fs::write(&config, "[fixture]\ntype = local\n").unwrap();
        let mut storage = RcloneStorage::new("fixture", remote.to_string_lossy());
        storage.extra_args = vec!["--config".into(), config.to_string_lossy().into()];
        for (name, bytes) in [
            ("literal[?];key", b"abc".as_slice()),
            ("empty", b"".as_slice()),
            ("dir/child", b"do not concatenate".as_slice()),
        ] {
            std::fs::write(remote.join("catalog").join(name), bytes).unwrap();
        }
        assert_eq!(
            storage
                .read_small(&RemoteKey::new("catalog/literal[?];key"), 3)
                .await
                .unwrap(),
            b"abc"
        );
        assert!(storage
            .read_small(&RemoteKey::new("catalog/empty"), 0)
            .await
            .unwrap()
            .is_empty());
        assert!(matches!(
            storage
                .read_small(&RemoteKey::new("catalog/missing"), 3)
                .await,
            Err(StateError::NotFound(_))
        ));
        assert!(storage
            .read_small(&RemoteKey::new("catalog/dir"), 64)
            .await
            .is_err());
        assert!(matches!(
            storage
                .read_small(&RemoteKey::new("catalog/literal[?];key"), 2)
                .await,
            Err(StateError::Integrity(_))
        ));
        assert_eq!(storage.operation_metrics().remote_stat_calls, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires rclone on PATH; fresh local fixture only, no cloud credentials"]
    async fn real_rclone_healthy_catalog_retains_publication_recovery_and_authentication_checks() {
        let root =
            std::env::temp_dir().join(format!("noland-catalog-real-{}", uuid::Uuid::new_v4()));
        let remote = root.join("cloud");
        std::fs::create_dir_all(&remote).unwrap();
        let config = root.join("local.conf");
        std::fs::write(&config, "[fixture]\ntype = local\n").unwrap();
        let mut storage = RcloneStorage::new("fixture", remote.to_string_lossy());
        storage.extra_args = vec!["--config".into(), config.to_string_lossy().into()];
        let master = noland_crypto::MasterKey::generate();
        assert!(crate::load_catalog(&storage, &master)
            .await
            .unwrap()
            .apps
            .is_empty());
        let catalog = noland_state_core::CatalogDocument::empty();
        crate::commit::write_catalog(&storage, &master, &catalog)
            .await
            .unwrap();
        let before = storage.operation_metrics();
        let read_before = storage.metadata_read_metrics();
        let loaded = crate::load_catalog(&storage, &master).await.unwrap();
        assert_eq!(loaded.catalog_commit_id, catalog.catalog_commit_id);
        let operations = storage.operation_metrics().saturating_sub(before);
        assert_eq!(operations.rclone_invocations, 4);
        assert_eq!(operations.remote_stat_calls, 1);
        assert_eq!(operations.remote_list_calls, 1);
        assert_eq!(operations.remote_download_calls, 2);
        let reads = storage.metadata_read_metrics().saturating_sub(read_before);
        assert_eq!((reads.read_calls, reads.attempts), (2, 2));
        assert_eq!(reads.bytes_received, operations.bytes_downloaded);
        assert!(reads.bytes_received > 36);
        assert!(
            crate::load_catalog(&storage, &noland_crypto::MasterKey::generate())
                .await
                .is_err()
        );
        // A broken pointer recovers committed history but never rewrites it.
        let latest = remote.join(noland_state_core::catalog_latest_key());
        std::fs::write(&latest, b"../not-a-commit").unwrap();
        assert_eq!(
            crate::load_catalog(&storage, &master)
                .await
                .unwrap()
                .catalog_commit_id,
            catalog.catalog_commit_id
        );
        assert_eq!(std::fs::read(&latest).unwrap(), b"../not-a-commit");
        std::fs::remove_file(&latest).unwrap();
        assert_eq!(
            crate::load_catalog(&storage, &master)
                .await
                .unwrap()
                .catalog_commit_id,
            catalog.catalog_commit_id
        );
        assert!(!latest.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
