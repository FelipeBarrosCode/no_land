use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
#[cfg(target_os = "linux")]
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
#[cfg(target_os = "linux")]
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::clock::Clock;
use crate::config::Config;
use crate::db::Database;
use crate::{AgentError, Result};

pub const MAX_ACTIVITY_LINE_BYTES: usize = 4 * 1024;
const SOURCE_DEBOUNCE_MS: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivitySource {
    Keyboard,
    Mouse,
    Controller,
    Touch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub source: ActivitySource,
    pub timestamp_ms: i64,
    #[serde(default)]
    pub synthetic: bool,
    #[serde(default)]
    pub magnitude: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityOutcome {
    Accepted,
    IgnoredSynthetic,
    IgnoredControllerDrift,
    IgnoredDebounced,
}

#[async_trait]
pub trait ActivityRecorder: Send + Sync {
    async fn record_activity(&self, event: ActivityEvent) -> Result<ActivityOutcome>;
}

pub struct ActivityProcessor {
    database: Arc<Database>,
    config: Arc<RwLock<Config>>,
    clock: Arc<dyn Clock>,
    last_by_source: Mutex<HashMap<ActivitySource, u64>>,
    policy_lock: Mutex<()>,
    last_activity_monotonic_ms: AtomicU64,
    timeout_latched: AtomicBool,
}

impl ActivityProcessor {
    pub fn new(
        database: Arc<Database>,
        config: Arc<RwLock<Config>>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            database,
            config,
            last_activity_monotonic_ms: AtomicU64::new(clock.monotonic_ms()),
            clock,
            last_by_source: Mutex::new(HashMap::new()),
            policy_lock: Mutex::new(()),
            timeout_latched: AtomicBool::new(false),
        }
    }

    pub fn last_activity_monotonic_ms(&self) -> u64 {
        self.last_activity_monotonic_ms.load(Ordering::Acquire)
    }

    pub fn latch_timeout(&self) {
        let _policy = self
            .policy_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.timeout_latched.store(true, Ordering::Release);
    }

    pub fn latch_timeout_if_elapsed(&self, timeout_ms: u64) -> Result<bool> {
        let _policy = self
            .policy_lock
            .lock()
            .map_err(|_| AgentError::new("activity policy lock poisoned"))?;
        if self.timeout_latched.load(Ordering::Acquire) {
            return Ok(true);
        }
        let elapsed = self
            .clock
            .monotonic_ms()
            .saturating_sub(self.last_activity_monotonic_ms());
        if elapsed < timeout_ms {
            return Ok(false);
        }
        self.timeout_latched.store(true, Ordering::Release);
        Ok(true)
    }

    pub fn reset_for_monitoring(&self) -> Result<()> {
        let _policy = self
            .policy_lock
            .lock()
            .map_err(|_| AgentError::new("activity policy lock poisoned"))?;
        let now_mono = self.clock.monotonic_ms();
        self.last_activity_monotonic_ms
            .store(now_mono, Ordering::Release);
        self.timeout_latched.store(false, Ordering::Release);
        self.last_by_source
            .lock()
            .map_err(|_| AgentError::new("activity debounce lock poisoned"))?
            .clear();
        self.database.record_accepted_activity(self.clock.now_utc())
    }

    pub fn idle_duration_ms(&self) -> u64 {
        self.clock
            .monotonic_ms()
            .saturating_sub(self.last_activity_monotonic_ms())
    }

    pub fn timeout_is_latched(&self) -> bool {
        self.timeout_latched.load(Ordering::Acquire)
    }
}

#[async_trait]
impl ActivityRecorder for ActivityProcessor {
    async fn record_activity(&self, event: ActivityEvent) -> Result<ActivityOutcome> {
        if event.event_type != "user_activity" {
            return Err(AgentError::new("unsupported activity event type"));
        }
        if event.timestamp_ms < 0 {
            return Err(AgentError::new("activity timestampMs must be non-negative"));
        }
        if event.synthetic {
            return Ok(ActivityOutcome::IgnoredSynthetic);
        }
        if event.source == ActivitySource::Controller {
            let magnitude = event
                .magnitude
                .filter(|value| value.is_finite())
                .ok_or_else(|| AgentError::new("controller activity requires finite magnitude"))?;
            let dead_zone = self
                .config
                .read()
                .map_err(|_| AgentError::new("configuration lock poisoned"))?
                .controller_dead_zone;
            if magnitude.abs() <= dead_zone {
                return Ok(ActivityOutcome::IgnoredControllerDrift);
            }
        }

        let _policy = self
            .policy_lock
            .lock()
            .map_err(|_| AgentError::new("activity policy lock poisoned"))?;
        let now_mono = self.clock.monotonic_ms();
        {
            let mut last = self
                .last_by_source
                .lock()
                .map_err(|_| AgentError::new("activity debounce lock poisoned"))?;
            if last
                .get(&event.source)
                .is_some_and(|previous| now_mono.saturating_sub(*previous) < SOURCE_DEBOUNCE_MS)
            {
                return Ok(ActivityOutcome::IgnoredDebounced);
            }
            last.insert(event.source, now_mono);
        }

        self.database
            .record_accepted_activity(self.clock.now_utc())?;
        self.last_activity_monotonic_ms
            .store(now_mono, Ordering::Release);
        Ok(ActivityOutcome::Accepted)
    }
}

pub async fn serve_activity_socket(path: &Path, recorder: Arc<dyn ActivityRecorder>) -> Result<()> {
    let listener = bind_private_socket(path).await?;
    info!(path = %path.display(), "activity socket listening");
    loop {
        let (stream, _) = listener.accept().await?;
        let recorder = Arc::clone(&recorder);
        tokio::spawn(async move {
            if let Err(error) = read_activity_stream(stream, recorder).await {
                warn!(error = %error, "activity producer disconnected with an error");
            }
        });
    }
}

/// Monitor Sunshine/Moonlight-created Linux input devices as a fallback until
/// Sunshine emits normalized events directly to the activity socket.
#[cfg(target_os = "linux")]
pub async fn monitor_stream_input_devices(recorder: Arc<dyn ActivityRecorder>) -> Result<()> {
    loop {
        let devices = discover_stream_input_devices()?;
        if devices.is_empty() {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        let mut readers = JoinSet::new();
        for device in devices {
            let recorder = Arc::clone(&recorder);
            readers.spawn(async move { read_linux_input_device(device, recorder).await });
        }

        tokio::select! {
            result = readers.join_next() => {
                if let Some(Ok(Err(error))) = result {
                    warn!(error = %error, "stream input device reader stopped");
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
        readers.abort_all();
        while readers.join_next().await.is_some() {}
    }
}

#[cfg(not(target_os = "linux"))]
pub async fn monitor_stream_input_devices(_recorder: Arc<dyn ActivityRecorder>) -> Result<()> {
    std::future::pending().await
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone)]
struct StreamInputDevice {
    path: PathBuf,
    source: ActivitySource,
}

#[cfg(target_os = "linux")]
fn discover_stream_input_devices() -> Result<Vec<StreamInputDevice>> {
    let devices = match std::fs::read_to_string("/proc/bus/input/devices") {
        Ok(devices) => devices,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(parse_stream_input_devices(&devices))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stream_input_devices(devices: &str) -> Vec<StreamInputDevice> {
    let mut output = Vec::new();
    for block in devices.split("\n\n") {
        let lower = block.to_ascii_lowercase();
        let is_stream_device = lower.contains("sunshine")
            || lower.contains("moonlight")
            || lower.contains("passthrough")
            || (lower.contains("vendor=beef") && lower.contains("product=dead"));
        if !is_stream_device {
            continue;
        }
        let source = if lower.contains("controller")
            || lower.contains("gamepad")
            || lower.contains("xbox")
        {
            ActivitySource::Controller
        } else if lower.contains("touch") || lower.contains("pen") {
            ActivitySource::Touch
        } else if lower.contains("mouse") {
            ActivitySource::Mouse
        } else if lower.contains("keyboard") {
            ActivitySource::Keyboard
        } else {
            continue;
        };
        let Some(handler_line) = block.lines().find(|line| line.starts_with("H: Handlers=")) else {
            continue;
        };
        for handler in handler_line.split_whitespace() {
            if handler.starts_with("event")
                && handler[5..]
                    .chars()
                    .all(|character| character.is_ascii_digit())
            {
                output.push(StreamInputDevice {
                    path: PathBuf::from("/dev/input").join(handler),
                    source,
                });
            }
        }
    }
    output
}

#[cfg(target_os = "linux")]
async fn read_linux_input_device(
    device: StreamInputDevice,
    recorder: Arc<dyn ActivityRecorder>,
) -> Result<()> {
    const INPUT_EVENT_BYTES_64_BIT: usize = 24;
    const EV_KEY: u16 = 1;
    const EV_REL: u16 = 2;
    const EV_ABS: u16 = 3;

    let mut file = tokio::fs::File::open(&device.path).await?;
    let mut bytes = [0_u8; INPUT_EVENT_BYTES_64_BIT];
    loop {
        file.read_exact(&mut bytes).await?;
        let event_type = u16::from_ne_bytes([bytes[16], bytes[17]]);
        let value = i32::from_ne_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        let accepted = match device.source {
            ActivitySource::Keyboard => event_type == EV_KEY && value != 0,
            ActivitySource::Mouse => matches!(event_type, EV_KEY | EV_REL) && value != 0,
            ActivitySource::Touch => matches!(event_type, EV_KEY | EV_ABS) && value != 0,
            ActivitySource::Controller => matches!(event_type, EV_KEY | EV_ABS) && value != 0,
        };
        if !accepted {
            continue;
        }
        let magnitude = if device.source == ActivitySource::Controller {
            Some(if event_type == EV_ABS {
                ((value as f32).abs() / 32_767.0).min(1.0)
            } else {
                1.0
            })
        } else {
            None
        };
        let event = ActivityEvent {
            event_type: "user_activity".to_string(),
            source: device.source,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            synthetic: false,
            magnitude,
        };
        if let Err(error) = recorder.record_activity(event).await {
            warn!(path = %device.path.display(), error = %error, "stream input event was rejected");
        }
    }
}

async fn bind_private_socket(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    }
    Ok(listener)
}

async fn read_activity_stream(
    mut stream: UnixStream,
    recorder: Arc<dyn ActivityRecorder>,
) -> Result<()> {
    let mut read_buffer = [0_u8; 1024];
    let mut line = Vec::with_capacity(512);
    let mut discarding_oversize = false;
    loop {
        let count = stream.read(&mut read_buffer).await?;
        if count == 0 {
            return Ok(());
        }
        for byte in &read_buffer[..count] {
            if *byte == b'\n' {
                if !discarding_oversize && !line.is_empty() {
                    match serde_json::from_slice::<ActivityEvent>(&line) {
                        Ok(event) => {
                            if let Err(error) = recorder.record_activity(event).await {
                                warn!(error = %error, "rejected activity event");
                            }
                        }
                        Err(error) => warn!(error = %error, "rejected malformed activity event"),
                    }
                }
                line.clear();
                discarding_oversize = false;
            } else if !discarding_oversize {
                if line.len() == MAX_ACTIVITY_LINE_BYTES {
                    line.clear();
                    discarding_oversize = true;
                    warn!("rejected oversized activity event");
                } else {
                    line.push(*byte);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::clock::ManualClock;

    fn event(source: ActivitySource) -> ActivityEvent {
        ActivityEvent {
            event_type: "user_activity".into(),
            source,
            timestamp_ms: 1,
            synthetic: false,
            magnitude: (source == ActivitySource::Controller).then_some(0.8),
        }
    }

    fn processor() -> (Arc<ActivityProcessor>, ManualClock, Arc<Database>) {
        let database = Arc::new(Database::open_in_memory().unwrap());
        let clock = ManualClock::new(Utc.timestamp_opt(1_000, 0).unwrap());
        let processor = Arc::new(ActivityProcessor::new(
            Arc::clone(&database),
            Arc::new(RwLock::new(Config::default())),
            Arc::new(clock.clone()),
        ));
        (processor, clock, database)
    }

    #[test]
    fn discovers_passthrough_stream_input_devices() {
        let devices = r#"I: Bus=0011 Vendor=0001 Product=0001 Version=ab41
N: Name="AT Translated Set 2 keyboard"
H: Handlers=sysrq kbd event1 leds

I: Bus=0003 Vendor=beef Product=dead Version=0111
N: Name="Mouse passthrough"
H: Handlers=mouse1 event7

I: Bus=0003 Vendor=beef Product=dead Version=0111
N: Name="Keyboard passthrough"
H: Handlers=sysrq kbd event9

I: Bus=0003 Vendor=beef Product=dead Version=0111
N: Name="Touch passthrough"
H: Handlers=mouse3 event10

I: Bus=0003 Vendor=beef Product=dead Version=0111
N: Name="Pen passthrough"
H: Handlers=mouse4 event11
"#;

        let discovered = parse_stream_input_devices(devices);
        assert_eq!(discovered.len(), 4);
        assert_eq!(discovered[0].path, PathBuf::from("/dev/input/event7"));
        assert_eq!(discovered[0].source, ActivitySource::Mouse);
        assert_eq!(discovered[1].source, ActivitySource::Keyboard);
        assert_eq!(discovered[2].source, ActivitySource::Touch);
        assert_eq!(discovered[3].source, ActivitySource::Touch);
    }

    #[test]
    fn still_discovers_named_sunshine_controller() {
        let devices = r#"I: Bus=0003 Vendor=045e Product=028e Version=0114
N: Name="Sunshine Xbox Controller"
H: Handlers=js1 event12
"#;

        let discovered = parse_stream_input_devices(devices);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].path, PathBuf::from("/dev/input/event12"));
        assert_eq!(discovered[0].source, ActivitySource::Controller);
    }

    #[tokio::test]
    async fn all_real_activity_sources_reset_the_anchor() {
        let (processor, clock, _) = processor();
        for source in [
            ActivitySource::Keyboard,
            ActivitySource::Mouse,
            ActivitySource::Controller,
            ActivitySource::Touch,
        ] {
            clock.advance(Duration::from_millis(100));
            assert_eq!(
                processor.record_activity(event(source)).await.unwrap(),
                ActivityOutcome::Accepted
            );
            assert_eq!(processor.idle_duration_ms(), 0);
        }
    }

    #[tokio::test]
    async fn controller_drift_and_synthetic_events_are_ignored() {
        let (processor, clock, _) = processor();
        clock.advance(Duration::from_secs(1));
        let mut drift = event(ActivitySource::Controller);
        drift.magnitude = Some(0.1);
        assert_eq!(
            processor.record_activity(drift).await.unwrap(),
            ActivityOutcome::IgnoredControllerDrift
        );
        let mut synthetic = event(ActivitySource::Keyboard);
        synthetic.synthetic = true;
        assert_eq!(
            processor.record_activity(synthetic).await.unwrap(),
            ActivityOutcome::IgnoredSynthetic
        );
        assert_eq!(processor.idle_duration_ms(), 1_000);
    }

    #[tokio::test]
    async fn activity_reset_preserves_usage() {
        let (processor, clock, database) = processor();
        database
            .record_active_session("session", "app", 2_000, clock.now_utc())
            .unwrap();
        clock.advance(Duration::from_secs(1));
        processor
            .record_activity(event(ActivitySource::Keyboard))
            .await
            .unwrap();
        let app = database.ranked_apps(1).unwrap().remove(0);
        assert_eq!(app.process_runtime_ms, 2_000);
        assert_eq!(app.launch_count, 1);
    }
}
