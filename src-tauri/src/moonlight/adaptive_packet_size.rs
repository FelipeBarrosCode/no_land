use std::{
    fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::moonlight::domain::RemoteStreamMode;
use crate::moonlight::infrastructure::persistence::atomic_file::write_atomically;

pub const PACKET_SIZE_LADDER: [u16; 6] = [1392, 1280, 1152, 1088, 1024, 960];
pub const MIN_PACKET_SIZE: u16 = 960;

const CACHE_SCHEMA_VERSION: u8 = 1;
const CACHE_MAX_ENTRIES: usize = 128;
const CACHE_MAX_BYTES: u64 = 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const UPSHIFT_PROBE_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const EVALUATION_INTERVAL: Duration = Duration::from_millis(500);
const VALIDATION_COOLDOWN: Duration = Duration::from_secs(5);
const STABLE_SESSION_DURATION: Duration = Duration::from_secs(30);
const MAX_WINDOW_COUNTER: u64 = 1_000_000_000;
const STATUS_MAX_AGE_SECS: u64 = 10;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PacketSizeObservation {
    pub generation: u64,
    pub video_packets: u64,
    pub fec_packets: u64,
    pub fec_recoveries: u64,
    pub fec_failures: u64,
    pub out_of_sequence: u64,
    pub invalid_packets: u64,
    pub invalid_fec_packets: u64,
    pub estimated_rtt_ms: Option<u32>,
    pub estimated_rtt_variance_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketSizeDecision {
    pub from: u16,
    pub to: u16,
    pub score: u8,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PacketSizeControllerSnapshot {
    pub state_label: String,
    pub path_label: String,
    pub selected_size: u16,
    pub last_good: Option<u16>,
    pub mtu_hint: Option<u32>,
    pub bad_window_count: u8,
    pub confidence: f32,
    pub fingerprint: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Tunnel,
    LocalDirect,
    PrivateUnverified,
    RemoteDirect,
    Unknown,
}

impl PathKind {
    fn label(self) -> &'static str {
        match self {
            Self::Tunnel => "tunnel",
            Self::LocalDirect => "local-direct",
            Self::PrivateUnverified => "private-unverified",
            Self::RemoteDirect => "remote-direct",
            Self::Unknown => "unknown",
        }
    }

    fn fingerprint_label(self) -> &'static str {
        self.label()
    }
}

#[derive(Debug, Clone)]
struct PathInfo {
    kind: PathKind,
    destination_family: &'static str,
    destination_address: String,
    mtu_hint: Option<u32>,
    wg_peer: Option<String>,
    wg_config_fingerprint: Option<String>,
    wg_endpoint: Option<String>,
    outer_source_ip: Option<IpAddr>,
    outer_interface: Option<String>,
}

impl PathInfo {
    fn unknown(host_address: &str) -> Self {
        let (destination_family, destination_address) = destination_identity(host_address);
        Self {
            kind: PathKind::Unknown,
            destination_family,
            destination_address,
            mtu_hint: None,
            wg_peer: None,
            wg_config_fingerprint: None,
            wg_endpoint: None,
            outer_source_ip: None,
            outer_interface: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControllerState {
    Disabled,
    Prepared,
    Validating,
    Monitoring,
    Stable,
    DownshiftPending,
}

impl ControllerState {
    fn label(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Prepared => "prepared",
            Self::Validating => "validating",
            Self::Monitoring => "monitoring",
            Self::Stable => "stable",
            Self::DownshiftPending => "downshift-pending",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ObservationAccumulator {
    video_packets: u64,
    fec_packets: u64,
    fec_recoveries: u64,
    fec_failures: u64,
    out_of_sequence: u64,
    invalid_packets: u64,
    invalid_fec_packets: u64,
    estimated_rtt_ms: Option<u32>,
    estimated_rtt_variance_ms: Option<u32>,
}

impl ObservationAccumulator {
    fn add(&mut self, observation: PacketSizeObservation) {
        self.video_packets = bounded_add(self.video_packets, observation.video_packets);
        self.fec_packets = bounded_add(self.fec_packets, observation.fec_packets);
        self.fec_recoveries = bounded_add(self.fec_recoveries, observation.fec_recoveries);
        self.fec_failures = bounded_add(self.fec_failures, observation.fec_failures);
        self.out_of_sequence = bounded_add(self.out_of_sequence, observation.out_of_sequence);
        self.invalid_packets = bounded_add(self.invalid_packets, observation.invalid_packets);
        self.invalid_fec_packets =
            bounded_add(self.invalid_fec_packets, observation.invalid_fec_packets);
        if observation.estimated_rtt_ms.is_some() {
            self.estimated_rtt_ms = observation.estimated_rtt_ms;
        }
        if observation.estimated_rtt_variance_ms.is_some() {
            self.estimated_rtt_variance_ms = observation.estimated_rtt_variance_ms;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PacketSizeCacheDocument {
    schema_version: u8,
    #[serde(default)]
    entries: Vec<PacketSizeCacheEntry>,
}

impl Default for PacketSizeCacheDocument {
    fn default() -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PacketSizeCacheEntry {
    fingerprint: String,
    selected_size: u16,
    last_good: Option<u16>,
    last_bad: Option<u16>,
    confidence: f32,
    successful_sessions: u32,
    updated_at_unix: u64,
    mtu_hint: Option<u32>,
    last_upshift_probe_at_unix: Option<u64>,
}

#[derive(Debug)]
pub struct AdaptivePacketSizeController {
    enabled: bool,
    resolved_mode: RemoteStreamMode,
    selected_size: u16,
    last_good: Option<u16>,
    last_bad: Option<u16>,
    mtu_hint: Option<u32>,
    path_kind: PathKind,
    fingerprint: String,
    confidence: f32,
    successful_sessions: u32,
    cache_path: PathBuf,
    cache: PacketSizeCacheDocument,
    generation: Option<u64>,
    state: ControllerState,
    window_started_at: Option<Instant>,
    last_observed_at: Option<Instant>,
    validation_until: Option<Instant>,
    stable_since: Option<Instant>,
    stable_recorded_generation: Option<u64>,
    accumulator: ObservationAccumulator,
    baseline_rtt_ms: Option<f64>,
    baseline_rtt_variance_ms: Option<f64>,
    recovery_window_streak: u8,
    bad_window_count: u8,
    pending_target: Option<u16>,
}

impl AdaptivePacketSizeController {
    pub fn prepare(
        app_data_dir: &Path,
        host_id: &str,
        host_address: &str,
        configured_mode: RemoteStreamMode,
        configured_packet_size: u16,
        enabled: bool,
    ) -> Self {
        let cache_path = app_data_dir
            .join("moonlight")
            .join("network-path-cache.json");

        if !enabled {
            let path = PathInfo::unknown(host_address);
            return Self {
                enabled: false,
                resolved_mode: configured_mode,
                selected_size: configured_packet_size,
                last_good: None,
                last_bad: None,
                mtu_hint: None,
                path_kind: PathKind::Unknown,
                fingerprint: path_fingerprint(host_id, &path),
                confidence: 0.0,
                successful_sessions: 0,
                cache_path,
                cache: PacketSizeCacheDocument::default(),
                generation: None,
                state: ControllerState::Disabled,
                window_started_at: None,
                last_observed_at: None,
                validation_until: None,
                stable_since: None,
                stable_recorded_generation: None,
                accumulator: ObservationAccumulator::default(),
                baseline_rtt_ms: None,
                baseline_rtt_variance_ms: None,
                recovery_window_streak: 0,
                bad_window_count: 0,
                pending_target: None,
            };
        }

        let path = detect_path(app_data_dir, host_address);
        Self::prepare_with_path(cache_path, host_id, configured_mode, path, unix_now())
    }

    pub fn selected_packet_size(&self) -> u16 {
        self.selected_size
    }

    pub fn resolved_remote_mode(&self) -> RemoteStreamMode {
        self.resolved_mode
    }

    pub fn on_connected(&mut self, generation: u64, now: Instant) {
        if !self.enabled {
            return;
        }

        self.generation = Some(generation);
        self.state = ControllerState::Validating;
        self.window_started_at = Some(now);
        self.last_observed_at = Some(now);
        self.validation_until = Some(now + VALIDATION_COOLDOWN);
        self.stable_since = Some(now);
        self.stable_recorded_generation = None;
        self.accumulator = ObservationAccumulator::default();
        self.baseline_rtt_ms = None;
        self.baseline_rtt_variance_ms = None;
        self.recovery_window_streak = 0;
        self.bad_window_count = 0;
        self.pending_target = None;
    }

    pub fn observe(
        &mut self,
        observation: PacketSizeObservation,
        now: Instant,
    ) -> Option<PacketSizeDecision> {
        if !self.enabled || self.generation != Some(observation.generation) {
            return None;
        }

        if self
            .last_observed_at
            .is_some_and(|last_observed| now < last_observed)
        {
            self.window_started_at = Some(now);
            self.last_observed_at = Some(now);
            self.accumulator = ObservationAccumulator::default();
            self.bad_window_count = 0;
            self.recovery_window_streak = 0;
            self.stable_since = Some(now);
            return None;
        }

        self.last_observed_at = Some(now);
        self.accumulator.add(observation);
        let window_started_at = self.window_started_at.get_or_insert(now);
        if now.duration_since(*window_started_at) < EVALUATION_INTERVAL {
            return None;
        }

        self.window_started_at = Some(now);
        let window = std::mem::take(&mut self.accumulator);
        self.evaluate_window(window, now)
    }

    pub fn commit_downshift(&mut self, target: u16) {
        if !self.enabled
            || !PACKET_SIZE_LADDER.contains(&target)
            || target < MIN_PACKET_SIZE
            || target >= self.selected_size
        {
            return;
        }

        self.last_bad = Some(self.selected_size);
        self.selected_size = target;
        self.confidence = (self.confidence - 0.2).clamp(0.0, 1.0);
        self.bad_window_count = 0;
        self.recovery_window_streak = 0;
        self.pending_target = None;
        self.accumulator = ObservationAccumulator::default();
        if let Some(now) = self.last_observed_at {
            self.validation_until = Some(now + VALIDATION_COOLDOWN);
            self.window_started_at = Some(now);
            self.stable_since = Some(now);
            self.state = ControllerState::Validating;
        } else {
            self.state = ControllerState::Prepared;
        }

        let now_unix = unix_now();
        if let Some(entry) = self
            .cache
            .entries
            .iter_mut()
            .find(|entry| entry.fingerprint == self.fingerprint)
        {
            entry.selected_size = self.selected_size;
            entry.last_good = self.last_good;
            entry.last_bad = self.last_bad;
            entry.confidence = self.confidence;
            entry.successful_sessions = self.successful_sessions;
            entry.updated_at_unix = now_unix;
            entry.mtu_hint = self.mtu_hint;
        } else {
            self.cache.entries.push(PacketSizeCacheEntry {
                fingerprint: self.fingerprint.clone(),
                selected_size: self.selected_size,
                last_good: self.last_good,
                last_bad: self.last_bad,
                confidence: self.confidence,
                successful_sessions: self.successful_sessions,
                updated_at_unix: now_unix,
                mtu_hint: self.mtu_hint,
                last_upshift_probe_at_unix: None,
            });
        }
        persist_cache(&self.cache_path, &mut self.cache, now_unix);
    }

    pub fn snapshot(&self) -> PacketSizeControllerSnapshot {
        PacketSizeControllerSnapshot {
            state_label: self.state.label().to_string(),
            path_label: self.path_kind.label().to_string(),
            selected_size: self.selected_size,
            last_good: self.last_good,
            mtu_hint: self.mtu_hint,
            bad_window_count: self.bad_window_count,
            confidence: self.confidence.clamp(0.0, 1.0),
            fingerprint: self.fingerprint.clone(),
            enabled: self.enabled,
        }
    }

    fn prepare_with_path(
        cache_path: PathBuf,
        host_id: &str,
        configured_mode: RemoteStreamMode,
        path: PathInfo,
        now_unix: u64,
    ) -> Self {
        let fingerprint = path_fingerprint(host_id, &path);
        let resolved_mode = resolve_remote_mode(configured_mode, path.kind);
        let candidate_path_kind = match resolved_mode {
            RemoteStreamMode::ForceRemote if path.kind != PathKind::Tunnel => {
                PathKind::RemoteDirect
            }
            RemoteStreamMode::ForceLocal => PathKind::LocalDirect,
            _ => path.kind,
        };
        let mtu_cap = initial_candidate(candidate_path_kind, path.mtu_hint);
        let mut cache = load_cache(&cache_path, now_unix);
        let mut selected_size = mtu_cap;
        let mut last_good = None;
        let mut last_bad = None;
        let mut confidence = 0.0;
        let mut successful_sessions = 0;
        let mut persist_probe_timestamp = false;

        if let Some(entry) = cache
            .entries
            .iter_mut()
            .find(|entry| entry.fingerprint == fingerprint)
        {
            selected_size = cap_candidate(normalize_cached_size(entry.selected_size), mtu_cap);
            last_good = entry
                .last_good
                .map(normalize_cached_size)
                .map(|size| cap_candidate(size, mtu_cap));
            last_bad = entry.last_bad.map(normalize_cached_size);
            confidence = entry.confidence.clamp(0.0, 1.0);
            successful_sessions = entry.successful_sessions;

            let (prepared_size, should_record_probe) =
                cached_session_selection(selected_size, mtu_cap, entry, now_unix);
            selected_size = prepared_size;
            if should_record_probe {
                entry.last_upshift_probe_at_unix = Some(now_unix);
                persist_probe_timestamp = true;
            }
        }

        if persist_probe_timestamp {
            persist_cache(&cache_path, &mut cache, now_unix);
        }

        Self {
            enabled: true,
            resolved_mode,
            selected_size,
            last_good,
            last_bad,
            mtu_hint: path.mtu_hint,
            path_kind: path.kind,
            fingerprint,
            confidence,
            successful_sessions,
            cache_path,
            cache,
            generation: None,
            state: ControllerState::Prepared,
            window_started_at: None,
            last_observed_at: None,
            validation_until: None,
            stable_since: None,
            stable_recorded_generation: None,
            accumulator: ObservationAccumulator::default(),
            baseline_rtt_ms: None,
            baseline_rtt_variance_ms: None,
            recovery_window_streak: 0,
            bad_window_count: 0,
            pending_target: None,
        }
    }

    fn evaluate_window(
        &mut self,
        window: ObservationAccumulator,
        now: Instant,
    ) -> Option<PacketSizeDecision> {
        let has_packet_evidence = has_packet_size_evidence(&window);
        if self.baseline_rtt_ms.is_none() {
            self.bad_window_count = 0;
            self.recovery_window_streak = 0;
            if !has_packet_evidence && window.estimated_rtt_ms.is_some() {
                self.update_rtt_baseline(&window);
                self.record_stable_if_ready(now);
            } else {
                self.stable_since = Some(now);
            }
            if self.state != ControllerState::Stable {
                self.state = self.active_state(now);
            }
            return None;
        }

        if self.is_congestion_window(&window) {
            self.bad_window_count = 0;
            self.recovery_window_streak = 0;
            self.stable_since = Some(now);
            self.state = self.active_state(now);
            return None;
        }

        let score = self.score_window(&window);
        if score == 0 {
            self.update_rtt_baseline(&window);
            self.bad_window_count = 0;
            self.record_stable_if_ready(now);
            if self.state != ControllerState::Stable {
                self.state = self.active_state(now);
            }
            return None;
        }

        self.stable_since = Some(now);
        if score < 3 {
            self.bad_window_count = 0;
            self.state = self.active_state(now);
            return None;
        }

        if self.in_validation_cooldown(now) || self.pending_target.is_some() {
            self.bad_window_count = 0;
            self.state = self.active_state(now);
            return None;
        }

        self.bad_window_count = self.bad_window_count.saturating_add(1).min(3);
        self.state = ControllerState::Monitoring;
        if self.bad_window_count < 3 {
            return None;
        }

        let target = next_lower_candidate(self.selected_size)?;
        self.pending_target = Some(target);
        self.state = ControllerState::DownshiftPending;
        Some(PacketSizeDecision {
            from: self.selected_size,
            to: target,
            score,
            reason: decision_reason(&window, self.recovery_window_streak),
        })
    }

    fn score_window(&mut self, window: &ObservationAccumulator) -> u8 {
        let mut score = 0u8;
        if window.fec_failures > 0 {
            score = score.saturating_add(3);
        }
        if window.invalid_fec_packets > 0 {
            score = score.saturating_add(3);
        }
        if window.invalid_packets > 0 {
            let significant = window.invalid_packets >= 3
                || window.invalid_packets.saturating_mul(1_000) >= window.video_packets.max(1);
            score = score.saturating_add(if significant { 3 } else { 1 });
        }

        let high_recovery_ratio = window.video_packets > 0
            && window.fec_recoveries.saturating_mul(100) >= window.video_packets;
        if high_recovery_ratio {
            self.recovery_window_streak = self.recovery_window_streak.saturating_add(1).min(2);
            if self.recovery_window_streak >= 2 {
                score = score.saturating_add(3);
            }
        } else {
            self.recovery_window_streak = 0;
        }

        score.min(10)
    }

    fn is_congestion_window(&self, window: &ObservationAccumulator) -> bool {
        let Some(rtt_ms) = window.estimated_rtt_ms.map(f64::from) else {
            return false;
        };
        let variance_ms = window
            .estimated_rtt_variance_ms
            .map(f64::from)
            .unwrap_or(0.0);

        let Some(baseline_rtt_ms) = self.baseline_rtt_ms else {
            return false;
        };
        let baseline_variance_ms = self.baseline_rtt_variance_ms.unwrap_or(0.0);
        let rtt_growth_limit = baseline_rtt_ms + (4.0 * baseline_variance_ms).max(20.0);
        let variance_growth_limit = baseline_variance_ms + (3.0 * baseline_variance_ms).max(10.0);

        rtt_ms > rtt_growth_limit || variance_ms > variance_growth_limit
    }

    fn update_rtt_baseline(&mut self, window: &ObservationAccumulator) {
        if let Some(rtt_ms) = window.estimated_rtt_ms.map(f64::from) {
            self.baseline_rtt_ms = Some(match self.baseline_rtt_ms {
                Some(baseline) => baseline * 0.9 + rtt_ms * 0.1,
                None => rtt_ms,
            });
        }
        if let Some(variance_ms) = window.estimated_rtt_variance_ms.map(f64::from) {
            self.baseline_rtt_variance_ms = Some(match self.baseline_rtt_variance_ms {
                Some(baseline) => baseline * 0.9 + variance_ms * 0.1,
                None => variance_ms,
            });
        }
    }

    fn in_validation_cooldown(&self, now: Instant) -> bool {
        self.validation_until.is_some_and(|until| now < until)
    }

    fn active_state(&self, now: Instant) -> ControllerState {
        if self.in_validation_cooldown(now) {
            ControllerState::Validating
        } else {
            ControllerState::Monitoring
        }
    }

    fn record_stable_if_ready(&mut self, now: Instant) {
        let Some(generation) = self.generation else {
            return;
        };
        if self.stable_recorded_generation == Some(generation)
            || self.stable_since.is_none_or(|stable_since| {
                now.duration_since(stable_since) < STABLE_SESSION_DURATION
            })
        {
            return;
        }

        self.last_good = Some(self.selected_size);
        self.successful_sessions = self.successful_sessions.saturating_add(1);
        self.confidence = (self.confidence + 0.2).clamp(0.0, 1.0);
        self.stable_recorded_generation = Some(generation);
        self.state = ControllerState::Stable;

        let now_unix = unix_now();
        if let Some(entry) = self
            .cache
            .entries
            .iter_mut()
            .find(|entry| entry.fingerprint == self.fingerprint)
        {
            entry.selected_size = self.selected_size;
            entry.last_good = self.last_good;
            entry.last_bad = self.last_bad;
            entry.confidence = self.confidence;
            entry.successful_sessions = self.successful_sessions;
            entry.updated_at_unix = now_unix;
            entry.mtu_hint = self.mtu_hint;
        } else {
            self.cache.entries.push(PacketSizeCacheEntry {
                fingerprint: self.fingerprint.clone(),
                selected_size: self.selected_size,
                last_good: self.last_good,
                last_bad: self.last_bad,
                confidence: self.confidence,
                successful_sessions: self.successful_sessions,
                updated_at_unix: now_unix,
                mtu_hint: self.mtu_hint,
                last_upshift_probe_at_unix: None,
            });
        }
        persist_cache(&self.cache_path, &mut self.cache, now_unix);
    }
}

fn bounded_add(left: u64, right: u64) -> u64 {
    left.saturating_add(right).min(MAX_WINDOW_COUNTER)
}

fn resolve_remote_mode(configured: RemoteStreamMode, path_kind: PathKind) -> RemoteStreamMode {
    if configured != RemoteStreamMode::Auto {
        return configured;
    }

    match path_kind {
        PathKind::Tunnel | PathKind::PrivateUnverified | PathKind::RemoteDirect => {
            RemoteStreamMode::ForceRemote
        }
        PathKind::LocalDirect => RemoteStreamMode::ForceLocal,
        PathKind::Unknown => RemoteStreamMode::Auto,
    }
}

fn initial_candidate(path_kind: PathKind, mtu_hint: Option<u32>) -> u16 {
    match path_kind {
        PathKind::LocalDirect => match mtu_hint {
            Some(mtu) if mtu >= 1500 => 1392,
            Some(mtu) if mtu >= 1400 => 1280,
            Some(mtu) if mtu >= 1300 => 1152,
            Some(mtu) if mtu >= 1280 => 1024,
            Some(_) => 960,
            None => 1024,
        },
        PathKind::Tunnel => match mtu_hint {
            Some(mtu) if mtu >= 1400 => 1280,
            Some(mtu) if mtu >= 1300 => 1152,
            Some(mtu) if mtu >= 1280 => 1024,
            Some(_) => 960,
            None => 1024,
        },
        PathKind::PrivateUnverified | PathKind::RemoteDirect => {
            if mtu_hint.is_some_and(|mtu| mtu < 1280) {
                960
            } else {
                1024
            }
        }
        PathKind::Unknown => 1024,
    }
}

fn normalize_cached_size(size: u16) -> u16 {
    PACKET_SIZE_LADDER
        .iter()
        .copied()
        .find(|candidate| *candidate <= size)
        .unwrap_or(MIN_PACKET_SIZE)
}

fn cap_candidate(candidate: u16, cap: u16) -> u16 {
    PACKET_SIZE_LADDER
        .iter()
        .copied()
        .find(|size| *size <= candidate && *size <= cap)
        .unwrap_or(MIN_PACKET_SIZE)
}

fn next_lower_candidate(current: u16) -> Option<u16> {
    let index = PACKET_SIZE_LADDER
        .iter()
        .position(|candidate| *candidate == current)?;
    PACKET_SIZE_LADDER.get(index + 1).copied()
}

fn next_higher_candidate(current: u16) -> Option<u16> {
    let index = PACKET_SIZE_LADDER
        .iter()
        .position(|candidate| *candidate == current)?;
    index
        .checked_sub(1)
        .and_then(|higher| PACKET_SIZE_LADDER.get(higher))
        .copied()
}

fn cached_session_selection(
    selected_size: u16,
    mtu_cap: u16,
    entry: &PacketSizeCacheEntry,
    now_unix: u64,
) -> (u16, bool) {
    if entry.confidence < 0.8 || entry.successful_sessions < 4 {
        return (selected_size, false);
    }

    let reference_time = entry
        .last_upshift_probe_at_unix
        .unwrap_or(entry.updated_at_unix);
    if now_unix.saturating_sub(reference_time) < UPSHIFT_PROBE_INTERVAL.as_secs() {
        return (selected_size, false);
    }

    let Some(higher) = next_higher_candidate(selected_size) else {
        return (selected_size, false);
    };
    if higher > mtu_cap {
        return (selected_size, false);
    }

    (higher, true)
}

fn has_packet_size_evidence(window: &ObservationAccumulator) -> bool {
    window.fec_failures > 0
        || window.invalid_fec_packets > 0
        || window.invalid_packets >= 3
        || (window.video_packets > 0
            && (window.invalid_packets.saturating_mul(1_000) >= window.video_packets
                || window.fec_recoveries.saturating_mul(100) >= window.video_packets))
}

fn decision_reason(window: &ObservationAccumulator, recovery_streak: u8) -> String {
    let mut reasons = Vec::with_capacity(4);
    if window.fec_failures > 0 {
        reasons.push("FEC failures");
    }
    if window.invalid_fec_packets > 0 {
        reasons.push("invalid FEC packets");
    }
    if window.invalid_packets > 0 {
        reasons.push("invalid packets");
    }
    if recovery_streak >= 2 {
        reasons.push("sustained FEC recovery ratio");
    }
    reasons.join(", ")
}

fn cache_is_fresh(entry: &PacketSizeCacheEntry, now_unix: u64) -> bool {
    entry.updated_at_unix > now_unix
        || now_unix.saturating_sub(entry.updated_at_unix) <= CACHE_TTL.as_secs()
}

fn normalize_cache(cache: &mut PacketSizeCacheDocument, now_unix: u64) {
    cache.schema_version = CACHE_SCHEMA_VERSION;
    cache
        .entries
        .retain(|entry| cache_is_fresh(entry, now_unix));
    cache.entries.sort_by(|left, right| {
        right
            .updated_at_unix
            .cmp(&left.updated_at_unix)
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    cache.entries.truncate(CACHE_MAX_ENTRIES);
    for entry in &mut cache.entries {
        entry.selected_size = normalize_cached_size(entry.selected_size);
        entry.last_good = entry.last_good.map(normalize_cached_size);
        entry.last_bad = entry.last_bad.map(normalize_cached_size);
        entry.confidence = entry.confidence.clamp(0.0, 1.0);
    }
}

fn load_cache(path: &Path, now_unix: u64) -> PacketSizeCacheDocument {
    if fs::metadata(path)
        .ok()
        .is_some_and(|metadata| metadata.len() > CACHE_MAX_BYTES)
    {
        return PacketSizeCacheDocument::default();
    }
    let Ok(contents) = fs::read(path) else {
        return PacketSizeCacheDocument::default();
    };
    let Ok(mut cache) = serde_json::from_slice::<PacketSizeCacheDocument>(&contents) else {
        return PacketSizeCacheDocument::default();
    };
    if cache.schema_version != CACHE_SCHEMA_VERSION {
        return PacketSizeCacheDocument::default();
    }
    normalize_cache(&mut cache, now_unix);
    cache
}

fn persist_cache(path: &Path, cache: &mut PacketSizeCacheDocument, now_unix: u64) {
    normalize_cache(cache, now_unix);
    if let Ok(contents) = serde_json::to_vec_pretty(cache) {
        if let Err(error) = write_atomically(path, &contents) {
            tracing::warn!(path = %path.display(), %error, "failed to persist adaptive packet-size cache");
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn detect_path(app_data_dir: &Path, host_address: &str) -> PathInfo {
    let (destination_family, destination_address) = destination_identity(host_address);
    if let Some(destination_ip) = parse_destination_ip(host_address) {
        if let Some(tunnel) = detect_managed_tunnel(app_data_dir, destination_ip) {
            let route = derive_outer_route(&tunnel.endpoint);
            return PathInfo {
                kind: PathKind::Tunnel,
                destination_family,
                destination_address,
                mtu_hint: tunnel.mtu,
                wg_peer: Some(tunnel.peer),
                wg_config_fingerprint: Some(tunnel.config_fingerprint),
                wg_endpoint: Some(tunnel.endpoint),
                outer_source_ip: route.as_ref().map(|route| route.source_ip),
                outer_interface: route.and_then(|route| route.interface),
            };
        }

        let kind = classify_direct_ip(destination_ip);
        let route = derive_outer_route(host_address);
        let mtu_hint = route
            .as_ref()
            .and_then(|route| route.interface.as_deref())
            .and_then(interface_mtu);
        return PathInfo {
            kind,
            destination_family,
            destination_address,
            mtu_hint,
            wg_peer: None,
            wg_config_fingerprint: None,
            wg_endpoint: None,
            outer_source_ip: route.as_ref().map(|route| route.source_ip),
            outer_interface: route.and_then(|route| route.interface),
        };
    }

    let route = derive_outer_route(host_address);
    PathInfo {
        kind: PathKind::Unknown,
        destination_family,
        destination_address,
        mtu_hint: None,
        wg_peer: None,
        wg_config_fingerprint: None,
        wg_endpoint: None,
        outer_source_ip: route.as_ref().map(|route| route.source_ip),
        outer_interface: route.and_then(|route| route.interface),
    }
}

fn classify_direct_ip(ip: IpAddr) -> PathKind {
    match ip {
        IpAddr::V4(ip) => {
            if ip.is_loopback() || ip.is_link_local() {
                PathKind::LocalDirect
            } else if ip.is_private() {
                PathKind::PrivateUnverified
            } else if ip.is_unspecified() || ip.is_multicast() || ip == Ipv4Addr::BROADCAST {
                PathKind::Unknown
            } else {
                PathKind::RemoteDirect
            }
        }
        IpAddr::V6(ip) => {
            if ip.is_loopback() || ip.is_unicast_link_local() {
                PathKind::LocalDirect
            } else if ip.is_unique_local() {
                PathKind::PrivateUnverified
            } else if ip.is_unspecified() || ip.is_multicast() {
                PathKind::Unknown
            } else {
                PathKind::RemoteDirect
            }
        }
    }
}

fn parse_destination_ip(host_address: &str) -> Option<IpAddr> {
    let host_address = host_address.trim();
    host_address
        .parse::<IpAddr>()
        .ok()
        .or_else(|| {
            host_address
                .parse::<SocketAddr>()
                .ok()
                .map(|addr| addr.ip())
        })
        .or_else(|| {
            host_address
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .and_then(|value| value.parse().ok())
        })
}

fn destination_identity(host_address: &str) -> (&'static str, String) {
    match parse_destination_ip(host_address) {
        Some(IpAddr::V4(ip)) => ("ipv4", ip.to_string()),
        Some(IpAddr::V6(ip)) => ("ipv6", ip.to_string()),
        None => ("hostname", host_address.trim().to_ascii_lowercase()),
    }
}

fn path_fingerprint(host_id: &str, path: &PathInfo) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, host_id);
    hash_field(&mut hasher, path.destination_family);
    hash_field(&mut hasher, &path.destination_address);
    hash_field(&mut hasher, path.kind.fingerprint_label());
    if let Some(peer) = path.wg_peer.as_deref() {
        hash_field(&mut hasher, peer);
    }
    if let Some(config_fingerprint) = path.wg_config_fingerprint.as_deref() {
        hash_field(&mut hasher, config_fingerprint);
    }
    if let Some(endpoint) = path.wg_endpoint.as_deref() {
        hash_field(&mut hasher, endpoint);
    }
    if let Some(source_ip) = path.outer_source_ip {
        hash_field(&mut hasher, &source_ip.to_string());
    }
    if let Some(interface) = path.outer_interface.as_deref() {
        hash_field(&mut hasher, interface);
    }
    format!("{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_le_bytes());
    hasher.update(value.as_bytes());
}

#[derive(Debug)]
struct ManagedTunnelInfo {
    endpoint: String,
    peer: String,
    config_fingerprint: String,
    mtu: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedTunnelStatus {
    #[serde(default)]
    engine: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    config_path: String,
    #[serde(default)]
    peer_public_key: String,
    #[serde(default)]
    allowed_ips: Vec<String>,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    config_fingerprint: String,
    #[serde(default)]
    updated_at_unix: u64,
    #[serde(default)]
    error: Option<String>,
}

fn detect_managed_tunnel(app_data_dir: &Path, destination_ip: IpAddr) -> Option<ManagedTunnelInfo> {
    detect_managed_tunnel_in_root(
        &platform_wireguard_root(app_data_dir),
        destination_ip,
        unix_now(),
    )
}

fn detect_managed_tunnel_in_root(
    root: &Path,
    destination_ip: IpAddr,
    now_unix: u64,
) -> Option<ManagedTunnelInfo> {
    let mut status_paths = vec![root.join("gotatun-runtime").join("status.json")];
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten().take(256) {
            if entry.path().is_dir() {
                status_paths.push(entry.path().join("gotatun-runtime").join("status.json"));
            }
        }
    }

    let canonical_root = root.canonicalize().ok();
    let mut matching = Vec::new();
    for status_path in status_paths {
        let Ok(contents) = fs::read(&status_path) else {
            continue;
        };
        let Ok(status) = serde_json::from_slice::<ManagedTunnelStatus>(&contents) else {
            continue;
        };
        if !status.active
            || !status.engine.starts_with("gotatun-embedded-")
            || status.error.is_some()
            || now_unix.saturating_sub(status.updated_at_unix) > STATUS_MAX_AGE_SECS
            || status.endpoint.is_empty()
            || status.peer_public_key.is_empty()
            || status.config_fingerprint.is_empty()
            || !status
                .allowed_ips
                .iter()
                .flat_map(|allowed| allowed.split(','))
                .any(|allowed| cidr_contains(allowed.trim(), destination_ip))
        {
            continue;
        }

        let config_path = PathBuf::from(&status.config_path);
        let Ok(canonical_config_path) = config_path.canonicalize() else {
            continue;
        };
        if canonical_root
            .as_ref()
            .is_some_and(|root| !canonical_config_path.starts_with(root))
        {
            continue;
        }
        let Ok(config_contents) = fs::read(&canonical_config_path) else {
            continue;
        };
        let actual_fingerprint = format!("{:x}", Sha256::digest(&config_contents));
        if actual_fingerprint != status.config_fingerprint {
            continue;
        }
        let mtu = std::str::from_utf8(&config_contents)
            .ok()
            .and_then(parse_interface_mtu);
        matching.push((
            status.updated_at_unix,
            ManagedTunnelInfo {
                endpoint: status.endpoint,
                peer: status.peer_public_key,
                config_fingerprint: status.config_fingerprint,
                mtu,
            },
        ));
    }

    matching.sort_by_key(|(updated_at, _)| *updated_at);
    matching.pop().map(|(_, tunnel)| tunnel)
}

#[cfg(target_os = "macos")]
fn platform_wireguard_root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("wireguard-local")
}

#[cfg(not(target_os = "macos"))]
fn platform_wireguard_root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("wireguard")
}

fn parse_interface_mtu(config: &str) -> Option<u32> {
    let mut in_interface = false;
    for line in config.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_interface = line[1..line.len() - 1].eq_ignore_ascii_case("interface");
            continue;
        }
        if !in_interface {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("mtu") {
            return value.trim().parse::<u32>().ok().filter(|mtu| *mtu > 0);
        }
    }
    None
}

fn cidr_contains(cidr: &str, destination: IpAddr) -> bool {
    let (network, prefix) = match cidr.split_once('/') {
        Some((network, prefix)) => {
            let Ok(prefix) = prefix.trim().parse::<u8>() else {
                return false;
            };
            (network.trim(), Some(prefix))
        }
        None => (cidr.trim(), None),
    };
    let Ok(network) = network.parse::<IpAddr>() else {
        return false;
    };

    match (network, destination) {
        (IpAddr::V4(network), IpAddr::V4(destination)) => {
            let prefix = prefix.unwrap_or(32);
            if prefix > 32 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(network) & mask) == (u32::from(destination) & mask)
        }
        (IpAddr::V6(network), IpAddr::V6(destination)) => {
            let prefix = prefix.unwrap_or(128);
            if prefix > 128 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(network) & mask) == (u128::from(destination) & mask)
        }
        _ => false,
    }
}

#[derive(Debug)]
struct OuterRoute {
    source_ip: IpAddr,
    interface: Option<String>,
}

fn derive_outer_route(destination: &str) -> Option<OuterRoute> {
    let target = resolve_udp_target(destination)?;
    let bind_address = match target {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = UdpSocket::bind(bind_address).ok()?;
    socket.connect(target).ok()?;
    let source_ip = socket.local_addr().ok()?.ip();
    Some(OuterRoute {
        source_ip,
        interface: interface_for_ip(source_ip),
    })
}

fn resolve_udp_target(destination: &str) -> Option<SocketAddr> {
    let destination = destination.trim();
    if let Ok(address) = destination.parse::<SocketAddr>() {
        return Some(address);
    }
    if let Ok(ip) = destination.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, 9));
    }
    if let Ok(mut addresses) = destination.to_socket_addrs() {
        if let Some(address) = addresses.next() {
            return Some(address);
        }
    }
    let host = destination
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(destination);
    (host, 9).to_socket_addrs().ok()?.next()
}

#[cfg(unix)]
fn interface_for_ip(source_ip: IpAddr) -> Option<String> {
    use std::{ffi::CStr, ptr};

    let mut interfaces: *mut libc::ifaddrs = ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut interfaces) } != 0 {
        return None;
    }

    let mut current = interfaces;
    let mut result = None;
    while !current.is_null() {
        let interface = unsafe { &*current };
        if !interface.ifa_addr.is_null() && !interface.ifa_name.is_null() {
            let family = unsafe { (*interface.ifa_addr).sa_family as i32 };
            let address = match family {
                libc::AF_INET => {
                    let address = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_in) };
                    Some(IpAddr::V4(Ipv4Addr::from(
                        address.sin_addr.s_addr.to_ne_bytes(),
                    )))
                }
                libc::AF_INET6 => {
                    let address = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_in6) };
                    Some(IpAddr::V6(Ipv6Addr::from(address.sin6_addr.s6_addr)))
                }
                _ => None,
            };
            if address == Some(source_ip) {
                result = unsafe { CStr::from_ptr(interface.ifa_name) }
                    .to_str()
                    .ok()
                    .map(str::to_string);
                break;
            }
        }
        current = unsafe { (*current).ifa_next };
    }

    unsafe { libc::freeifaddrs(interfaces) };
    result
}

#[cfg(not(unix))]
fn interface_for_ip(_source_ip: IpAddr) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn interface_mtu(interface: &str) -> Option<u32> {
    fs::read_to_string(Path::new("/sys/class/net").join(interface).join("mtu"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(target_os = "macos")]
fn interface_mtu(interface: &str) -> Option<u32> {
    use std::process::{Command, Stdio};

    let output = Command::new("/sbin/ifconfig")
        .arg(interface)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let output = String::from_utf8_lossy(&output.stdout);
    let mut words = output.split_whitespace();
    while let Some(word) = words.next() {
        if word == "mtu" {
            return words.next()?.parse().ok();
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn interface_mtu(_interface: &str) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn test_path(kind: PathKind, mtu_hint: Option<u32>) -> PathInfo {
        PathInfo {
            kind,
            destination_family: "ipv4",
            destination_address: "203.0.113.10".to_string(),
            mtu_hint,
            wg_peer: None,
            wg_config_fingerprint: None,
            wg_endpoint: None,
            outer_source_ip: None,
            outer_interface: None,
        }
    }

    fn controller_at(
        root: &Path,
        kind: PathKind,
        mtu: Option<u32>,
    ) -> AdaptivePacketSizeController {
        AdaptivePacketSizeController::prepare_with_path(
            root.join("moonlight/network-path-cache.json"),
            "host-1",
            RemoteStreamMode::Auto,
            test_path(kind, mtu),
            unix_now(),
        )
    }

    fn healthy_observation(generation: u64) -> PacketSizeObservation {
        PacketSizeObservation {
            generation,
            video_packets: 1_000,
            estimated_rtt_ms: Some(40),
            estimated_rtt_variance_ms: Some(3),
            ..PacketSizeObservation::default()
        }
    }

    fn failing_observation(generation: u64) -> PacketSizeObservation {
        PacketSizeObservation {
            generation,
            video_packets: 1_000,
            fec_failures: 1,
            estimated_rtt_ms: Some(40),
            estimated_rtt_variance_ms: Some(3),
            ..PacketSizeObservation::default()
        }
    }

    #[test]
    fn maps_mtu_conservatively() {
        assert_eq!(initial_candidate(PathKind::LocalDirect, Some(1500)), 1392);
        assert_eq!(initial_candidate(PathKind::RemoteDirect, Some(1500)), 1024);
        assert_eq!(initial_candidate(PathKind::Tunnel, Some(1450)), 1280);
        assert_eq!(initial_candidate(PathKind::Tunnel, Some(1400)), 1280);
        assert_eq!(initial_candidate(PathKind::Tunnel, Some(1350)), 1152);
        assert_eq!(initial_candidate(PathKind::Tunnel, Some(1300)), 1152);
        assert_eq!(initial_candidate(PathKind::Tunnel, Some(1279)), 960);
        assert_eq!(initial_candidate(PathKind::RemoteDirect, None), 1024);
    }

    #[test]
    fn explicit_remote_mode_stays_conservative_on_private_direct_address() {
        let root = temp_dir("adaptive-packet-explicit-remote");
        let controller = AdaptivePacketSizeController::prepare_with_path(
            root.join("moonlight/network-path-cache.json"),
            "host-remote",
            RemoteStreamMode::ForceRemote,
            test_path(PathKind::LocalDirect, Some(1500)),
            unix_now(),
        );
        assert_eq!(
            controller.resolved_remote_mode(),
            RemoteStreamMode::ForceRemote
        );
        assert_eq!(controller.selected_packet_size(), 1024);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ladder_is_aligned_and_normalization_never_invents_sizes() {
        assert!(PACKET_SIZE_LADDER.windows(2).all(|pair| pair[0] > pair[1]));
        assert!(PACKET_SIZE_LADDER.iter().all(|size| size % 16 == 0));
        assert_eq!(normalize_cached_size(1344), 1280);
        assert_eq!(normalize_cached_size(959), MIN_PACKET_SIZE);
    }

    #[test]
    fn tunnel_mtu_1280_maps_to_1024() {
        assert_eq!(initial_candidate(PathKind::Tunnel, Some(1280)), 1024);
    }

    #[test]
    fn cache_is_ttl_pruned_and_bounded() {
        let now = 5_000_000;
        let mut entries = (0..140)
            .map(|index| PacketSizeCacheEntry {
                fingerprint: format!("entry-{index}"),
                selected_size: 1344,
                last_good: None,
                last_bad: None,
                confidence: 2.0,
                successful_sessions: 0,
                updated_at_unix: now - index,
                mtu_hint: None,
                last_upshift_probe_at_unix: None,
            })
            .collect::<Vec<_>>();
        entries.push(PacketSizeCacheEntry {
            fingerprint: "expired".to_string(),
            selected_size: 1024,
            last_good: None,
            last_bad: None,
            confidence: 0.5,
            successful_sessions: 1,
            updated_at_unix: now - CACHE_TTL.as_secs() - 1,
            mtu_hint: None,
            last_upshift_probe_at_unix: None,
        });
        let mut cache = PacketSizeCacheDocument {
            schema_version: CACHE_SCHEMA_VERSION,
            entries,
        };

        normalize_cache(&mut cache, now);

        assert_eq!(cache.entries.len(), CACHE_MAX_ENTRIES);
        assert!(!cache
            .entries
            .iter()
            .any(|entry| entry.fingerprint == "expired"));
        assert!(cache
            .entries
            .iter()
            .all(|entry| entry.selected_size == 1280 && entry.confidence <= 1.0));
    }

    #[test]
    fn out_of_sequence_alone_never_triggers() {
        let root = temp_dir("adaptive-packet-oos");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        let start = Instant::now();
        controller.on_connected(7, start);

        for index in 1..=12 {
            let decision = controller.observe(
                PacketSizeObservation {
                    generation: 7,
                    video_packets: 1_000,
                    out_of_sequence: 500,
                    estimated_rtt_ms: Some(40),
                    estimated_rtt_variance_ms: Some(3),
                    ..PacketSizeObservation::default()
                },
                start + Duration::from_millis(index * 500),
            );
            assert!(decision.is_none());
        }
        assert_eq!(controller.snapshot().bad_window_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn congestion_suppresses_bad_windows() {
        let root = temp_dir("adaptive-packet-congestion");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        let start = Instant::now();
        controller.on_connected(1, start);
        controller.observe(healthy_observation(1), start + Duration::from_millis(500));

        for index in 12..=15 {
            let decision = controller.observe(
                PacketSizeObservation {
                    generation: 1,
                    video_packets: 1_000,
                    fec_failures: 2,
                    estimated_rtt_ms: Some(100),
                    estimated_rtt_variance_ms: Some(30),
                    ..PacketSizeObservation::default()
                },
                start + Duration::from_millis(index * 500),
            );
            assert!(decision.is_none());
        }
        assert_eq!(controller.snapshot().bad_window_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_loss_cannot_become_the_rtt_baseline() {
        let root = temp_dir("adaptive-packet-startup-congestion");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        let start = Instant::now();
        controller.on_connected(32, start);

        for index in 1..=12 {
            let decision = controller.observe(
                PacketSizeObservation {
                    generation: 32,
                    video_packets: 1_000,
                    fec_failures: 2,
                    estimated_rtt_ms: Some(120),
                    estimated_rtt_variance_ms: Some(35),
                    ..PacketSizeObservation::default()
                },
                start + Duration::from_millis(index * 500),
            );
            assert!(decision.is_none());
        }
        assert_eq!(controller.snapshot().bad_window_count, 0);
        assert!(controller.baseline_rtt_ms.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn significant_invalid_packets_can_trigger_downshift() {
        let root = temp_dir("adaptive-packet-invalid");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        let start = Instant::now();
        controller.on_connected(33, start);
        controller.observe(healthy_observation(33), start + Duration::from_secs(5));
        let invalid = PacketSizeObservation {
            generation: 33,
            video_packets: 1_000,
            invalid_packets: 3,
            estimated_rtt_ms: Some(40),
            estimated_rtt_variance_ms: Some(3),
            ..PacketSizeObservation::default()
        };
        assert!(controller
            .observe(invalid.clone(), start + Duration::from_millis(5_500))
            .is_none());
        assert!(controller
            .observe(invalid.clone(), start + Duration::from_secs(6))
            .is_none());
        let decision = controller
            .observe(invalid, start + Duration::from_millis(6_500))
            .unwrap();
        assert_eq!(decision.to, 960);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn three_strong_windows_request_one_step_downshift() {
        let root = temp_dir("adaptive-packet-downshift");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        let start = Instant::now();
        controller.on_connected(3, start);
        controller.observe(healthy_observation(3), start + Duration::from_secs(5));

        assert!(controller
            .observe(failing_observation(3), start + Duration::from_millis(5_500))
            .is_none());
        assert!(controller
            .observe(failing_observation(3), start + Duration::from_secs(6))
            .is_none());
        let decision = controller
            .observe(failing_observation(3), start + Duration::from_millis(6_500))
            .unwrap();

        assert_eq!(decision.from, 1024);
        assert_eq!(decision.to, 960);
        assert!(decision.score >= 3);
        controller.commit_downshift(decision.to);
        assert_eq!(controller.selected_packet_size(), 960);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sustained_recovery_ratio_can_trigger_without_oos() {
        let root = temp_dir("adaptive-packet-recovery");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        let start = Instant::now();
        controller.on_connected(31, start);
        controller.observe(healthy_observation(31), start + Duration::from_secs(5));

        let recovery = PacketSizeObservation {
            generation: 31,
            video_packets: 1_000,
            fec_recoveries: 20,
            estimated_rtt_ms: Some(40),
            estimated_rtt_variance_ms: Some(3),
            ..PacketSizeObservation::default()
        };
        assert!(controller
            .observe(recovery.clone(), start + Duration::from_millis(5_500))
            .is_none());
        assert!(controller
            .observe(recovery.clone(), start + Duration::from_secs(6))
            .is_none());
        assert!(controller
            .observe(recovery.clone(), start + Duration::from_millis(6_500))
            .is_none());
        let decision = controller
            .observe(recovery, start + Duration::from_secs(7))
            .unwrap();
        assert_eq!(decision.from, 1024);
        assert_eq!(decision.to, 960);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_downshift_is_persisted_before_stability() {
        let root = temp_dir("adaptive-packet-persist-downshift");
        let cache_path = root.join("moonlight/network-path-cache.json");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        assert_eq!(controller.selected_packet_size(), 1024);

        controller.commit_downshift(960);

        let cache = load_cache(&cache_path, unix_now());
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].selected_size, 960);
        assert_eq!(cache.entries[0].last_bad, Some(1024));
        assert_eq!(cache.entries[0].successful_sessions, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn never_downshifts_below_normal_minimum() {
        let root = temp_dir("adaptive-packet-min");
        let mut controller = controller_at(&root, PathKind::Tunnel, Some(1200));
        assert_eq!(controller.selected_packet_size(), MIN_PACKET_SIZE);
        let start = Instant::now();
        controller.on_connected(4, start);
        controller.observe(healthy_observation(4), start + Duration::from_secs(5));
        for index in 11..=16 {
            assert!(controller
                .observe(
                    failing_observation(4),
                    start + Duration::from_millis(index * 500)
                )
                .is_none());
        }
        controller.commit_downshift(0);
        assert_eq!(controller.selected_packet_size(), MIN_PACKET_SIZE);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generation_change_resets_window_state() {
        let root = temp_dir("adaptive-packet-generation");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        let start = Instant::now();
        controller.on_connected(10, start);
        controller.observe(healthy_observation(10), start + Duration::from_secs(5));
        controller.observe(
            failing_observation(10),
            start + Duration::from_millis(5_500),
        );
        controller.observe(failing_observation(10), start + Duration::from_secs(6));
        assert_eq!(controller.snapshot().bad_window_count, 2);

        controller.on_connected(11, start + Duration::from_millis(6_500));
        assert_eq!(controller.snapshot().bad_window_count, 0);
        assert!(controller
            .observe(failing_observation(10), start + Duration::from_secs(12))
            .is_none());
        assert_eq!(controller.snapshot().bad_window_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stable_session_learns_and_writes_cache_once() {
        let root = temp_dir("adaptive-packet-stable");
        let cache_path = root.join("moonlight/network-path-cache.json");
        let mut controller = controller_at(&root, PathKind::RemoteDirect, Some(1500));
        let start = Instant::now();
        controller.on_connected(20, start);

        controller.observe(healthy_observation(20), start + Duration::from_secs(30));
        let snapshot = controller.snapshot();
        assert_eq!(snapshot.state_label, "stable");
        assert_eq!(snapshot.last_good, Some(1024));
        assert!((snapshot.confidence - 0.2).abs() < f32::EPSILON);

        let cache = load_cache(&cache_path, unix_now());
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].successful_sessions, 1);
        controller.observe(healthy_observation(20), start + Duration::from_secs(31));
        let cache = load_cache(&cache_path, unix_now());
        assert_eq!(cache.entries[0].successful_sessions, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upshift_probe_is_future_session_only_and_requires_all_conditions() {
        let now = 10_000_000;
        let mut entry = PacketSizeCacheEntry {
            fingerprint: "safe-hash".to_string(),
            selected_size: 1024,
            last_good: Some(1024),
            last_bad: None,
            confidence: 0.8,
            successful_sessions: 4,
            updated_at_unix: now - UPSHIFT_PROBE_INTERVAL.as_secs(),
            mtu_hint: Some(1400),
            last_upshift_probe_at_unix: None,
        };

        assert_eq!(
            cached_session_selection(1024, 1152, &entry, now),
            (1088, true)
        );
        entry.last_upshift_probe_at_unix = Some(now - 60);
        assert_eq!(
            cached_session_selection(1024, 1152, &entry, now),
            (1024, false)
        );
        entry.last_upshift_probe_at_unix = None;
        entry.successful_sessions = 3;
        assert_eq!(
            cached_session_selection(1024, 1152, &entry, now),
            (1024, false)
        );
        entry.successful_sessions = 4;
        assert_eq!(
            cached_session_selection(1024, 1024, &entry, now),
            (1024, false)
        );
    }

    #[test]
    fn disabled_controller_preserves_baseline_and_never_writes() {
        let root = temp_dir("adaptive-packet-disabled");
        let mut controller = AdaptivePacketSizeController::prepare(
            &root,
            "host-disabled",
            "example.invalid",
            RemoteStreamMode::Auto,
            1337,
            false,
        );
        let start = Instant::now();
        controller.on_connected(1, start);
        assert!(controller
            .observe(failing_observation(1), start + Duration::from_secs(10))
            .is_none());
        controller.commit_downshift(1024);

        assert_eq!(controller.selected_packet_size(), 1337);
        assert_eq!(controller.resolved_remote_mode(), RemoteStreamMode::Auto);
        assert_eq!(controller.snapshot().state_label, "disabled");
        assert!(!root.join("moonlight/network-path-cache.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_fresh_managed_tunnel_and_parses_interface_mtu() {
        let root = temp_dir("adaptive-packet-tunnel-detect");
        let config_dir = root.join("42");
        let runtime_dir = root.join("gotatun-runtime");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&runtime_dir).unwrap();
        let config_path = config_dir.join("nolandwg0.conf");
        let config = "[Interface]\nPrivateKey = secret-not-persisted\nMTU = 1280\n\n[Peer]\nEndpoint = 198.51.100.10:51820\n";
        fs::write(&config_path, config).unwrap();
        let config_fingerprint = format!("{:x}", Sha256::digest(config.as_bytes()));
        let status = serde_json::json!({
            "engine": "gotatun-embedded-0.7.1",
            "active": true,
            "configPath": config_path,
            "peerPublicKey": "public-peer",
            "allowedIps": ["10.77.0.0/24"],
            "endpoint": "198.51.100.10:51820",
            "configFingerprint": config_fingerprint,
            "updatedAtUnix": 12345,
            "error": null
        });
        fs::write(
            runtime_dir.join("status.json"),
            serde_json::to_vec(&status).unwrap(),
        )
        .unwrap();

        let tunnel =
            detect_managed_tunnel_in_root(&root, "10.77.0.5".parse().unwrap(), 12345).unwrap();
        assert_eq!(tunnel.mtu, Some(1280));
        assert_eq!(initial_candidate(PathKind::Tunnel, tunnel.mtu), 1024);
        assert!(
            detect_managed_tunnel_in_root(&root, "10.78.0.5".parse().unwrap(), 12345).is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_classification_and_explicit_modes_are_conservative() {
        assert_eq!(
            classify_direct_ip("127.0.0.1".parse().unwrap()),
            PathKind::LocalDirect
        );
        assert_eq!(
            classify_direct_ip("10.77.0.1".parse().unwrap()),
            PathKind::PrivateUnverified
        );
        assert_eq!(
            resolve_remote_mode(RemoteStreamMode::Auto, PathKind::PrivateUnverified),
            RemoteStreamMode::ForceRemote
        );
        assert_eq!(
            classify_direct_ip("8.8.8.8".parse().unwrap()),
            PathKind::RemoteDirect
        );
        assert_eq!(
            resolve_remote_mode(RemoteStreamMode::Auto, PathKind::Unknown),
            RemoteStreamMode::Auto
        );
        assert_eq!(
            resolve_remote_mode(RemoteStreamMode::ForceLocal, PathKind::Tunnel),
            RemoteStreamMode::ForceLocal
        );
    }
}
