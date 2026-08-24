use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use gst::glib::{self, ControlFlow};
use gst::prelude::*;
use gstreamer as gst;
use serde::Serialize;
use tracing::{error, info, warn};

use crate::config::{ReceiverConfig, RTP_CLOCK_RATE, RTP_PAYLOAD_TYPE};

const ACTIVE_PACKET_WINDOW_MS: u64 = 750;
const RECENT_SEQUENCE_WINDOW: u64 = 4_096;
const ABSOLUTE_MINIMUM_JITTER_MS: u32 = 10;
const ABSOLUTE_MAXIMUM_JITTER_MS: u32 = 60;
const JITTER_INCREASE_STEP_MS: u32 = 4;
const JITTER_DECREASE_STEP_MS: u32 = 1;
const INCREASE_COOLDOWN_SAMPLES: u8 = 4;
const STABLE_SAMPLES_BEFORE_DECREASE: u16 = 20;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverRuntimeStatus {
    pub session_id: String,
    pub receiving_audio: bool,
    pub received_packets: u64,
    pub lost_packets: u64,
    pub late_packets: u64,
    pub out_of_order_packets: u64,
    pub duplicate_packets: u64,
    pub rejected_packets: u64,
    pub oversized_packets: u64,
    pub packet_loss_percent: f64,
    pub jitter_ms: f64,
    pub buffer_depth_ms: f64,
    pub decoded_buffers: u64,
    pub plc_estimate: u64,
    pub pipewire_errors: u64,
    pub rtt_ms: Option<f64>,
    pub last_packet_ms_ago: Option<u64>,
    pub uptime_seconds: u64,
    pub health: HealthFlags,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthFlags {
    pub healthy: bool,
    pub network_receiving: bool,
    pub pipewire_ok: bool,
    pub source_authorized: bool,
    pub rtcp_configured: bool,
}

#[derive(Debug)]
struct SharedStatus {
    started_at: Instant,
    last_packet_at: Option<Instant>,
    tracker: RtpTracker,
    decoded_buffers: u64,
    pipewire_errors: u64,
    pipeline_error: Option<String>,
}

impl SharedStatus {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            last_packet_at: None,
            tracker: RtpTracker::default(),
            decoded_buffers: 0,
            pipewire_errors: 0,
            pipeline_error: None,
        }
    }
}

#[derive(Debug, Default)]
struct RtpTracker {
    base_arrival: Option<Instant>,
    max_sequence: Option<u16>,
    max_extended_sequence: u64,
    seen: HashSet<u64>,
    received: u64,
    lost: u64,
    out_of_order: u64,
    duplicates: u64,
    rejected: u64,
    oversized: u64,
    jitter_timestamp_units: f64,
    previous_transit: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PacketDisposition {
    Accept,
    Drop,
}

pub struct Receiver {
    pipeline: gst::Pipeline,
    main_loop: glib::MainLoop,
    shared: Arc<Mutex<SharedStatus>>,
    _bus_watch: gst::bus::BusWatchGuard,
}

impl Receiver {
    pub fn new(
        config: &ReceiverConfig,
        status_path: &str,
        running: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        gst::init().map_err(|error| format!("failed initializing GStreamer: {error}"))?;

        let pipeline = build_pipeline(config)?;
        let main_loop = glib::MainLoop::new(None, false);
        let shared = Arc::new(Mutex::new(SharedStatus::new()));
        let jitterbuffer = Arc::new(Mutex::new(None));

        attach_jitterbuffer_observer(&pipeline, jitterbuffer.clone())?;
        attach_packet_probe(&pipeline, config, shared.clone())?;
        attach_decode_probe(&pipeline, shared.clone())?;
        let bus_watch = attach_bus_watch(&pipeline, main_loop.clone(), shared.clone())?;
        attach_status_writer(
            config,
            jitterbuffer,
            shared.clone(),
            status_path.to_string(),
            running.clone(),
            main_loop.clone(),
        );
        attach_shutdown_watch(running, main_loop.clone());

        Ok(Self {
            pipeline,
            main_loop,
            shared,
            _bus_watch: bus_watch,
        })
    }

    pub fn run(&self) -> Result<(), String> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| format!("failed starting receiver pipeline: {error:?}"))?;
        info!("receiver pipeline started");

        self.main_loop.run();

        let _ = self.pipeline.send_event(gst::event::Eos::new());
        let _ = self.pipeline.set_state(gst::State::Null);
        info!("receiver pipeline stopped");

        let guard = lock_status(&self.shared);
        if let Some(error) = &guard.pipeline_error {
            Err(error.clone())
        } else {
            Ok(())
        }
    }
}

fn build_pipeline(config: &ReceiverConfig) -> Result<gst::Pipeline, String> {
    let mut pipeline_description = format!(
        concat!(
            "rtpbin name=rtp_session latency={latency} drop-on-latency=true do-lost=true ",
            "udpsrc name=rtp_source address={bind_address} port={rtp_port} buffer-size={recv_buffer} ",
            "caps=\"application/x-rtp,media=(string)audio,clock-rate=(int)48000,encoding-name=(string)OPUS,payload=(int){payload}\" ",
            "! rtp_session.recv_rtp_sink_0 ",
            "rtp_session. ! rtpopusdepay ! opusdec plc=true use-inband-fec=true ",
            "! queue max-size-time=120000000 max-size-buffers=0 max-size-bytes=0 ",
            "! audiorate skip-to-first=true tolerance=40000000 ",
            "! audioconvert ! audioresample ",
            "! audio/x-raw,format=(string)S16LE,layout=(string)interleaved,rate=(int)48000,channels=(int)1 ",
            "! queue max-size-time=120000000 max-size-buffers=0 max-size-bytes=0 ",
            "! identity name=decoded_probe ",
            "udpsrc name=rtcp_source address={bind_address} port={rtcp_port} buffer-size={recv_buffer} ",
            "caps=\"application/x-rtcp\" ! rtp_session.recv_rtcp_sink_0 "
        ),
        latency = config.jitter.initial_ms,
        bind_address = config.network.bind_address,
        rtp_port = config.network.rtp_port,
        rtcp_port = config.network.rtcp_port,
        recv_buffer = config.network.recv_buffer_bytes,
        payload = RTP_PAYLOAD_TYPE,
    );

    if let Some(peer_ip) = &config.session.expected_peer_ip {
        pipeline_description.push_str(&format!(
            "rtp_session.send_rtcp_src_0 ! udpsink name=rtcp_report_sink host={peer_ip} port={} sync=false async=false ",
            config.session.client_rtcp_port
        ));
    }

    let element = gst::parse::launch(&pipeline_description)
        .map_err(|error| format!("failed creating RTP/RTCP pipeline: {error}"))?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| "GStreamer receiver description did not produce a Pipeline".to_string())?;

    let sink = gst::ElementFactory::make("pipewiresink")
        .name("pipewire_sink")
        .build()
        .map_err(|error| {
            format!("pipewiresink is unavailable; install gstreamer1.0-pipewire: {error}")
        })?;
    let target_properties = pipewire_target_properties(
        sink.find_property("target-object").is_some(),
        sink.find_property("path").is_some(),
    )?;
    for property in target_properties {
        sink.set_property(*property, &config.audio.pipewire_sink_name);
    }
    sink.set_property("sync", true);
    sink.set_property("async", false);
    info!(
        properties = ?target_properties,
        target = %config.audio.pipewire_sink_name,
        "configured explicit PipeWire sink target"
    );

    pipeline
        .add(&sink)
        .map_err(|error| format!("failed adding pipewiresink to pipeline: {error}"))?;
    let decoded = pipeline
        .by_name("decoded_probe")
        .ok_or_else(|| "receiver pipeline missing decoded_probe".to_string())?;
    decoded
        .link(&sink)
        .map_err(|error| format!("failed linking decoded audio to pipewiresink: {error}"))?;

    Ok(pipeline)
}

fn pipewire_target_properties(
    has_target_object: bool,
    has_path: bool,
) -> Result<&'static [&'static str], String> {
    if !has_target_object && !has_path {
        return Err(
            "pipewiresink exposes neither target-object nor path; refusing to use the desktop default output"
                .to_string(),
        );
    }
    // Assign every target property the installed pipewiresink exposes. Different
    // gst-pipewire builds bind by `target-object` (node name/serial) or by the
    // deprecated `path` (object.path); setting both maximizes the chance the
    // sink binds to noland_mic_sink instead of falling back to the desktop
    // default output, which would publish silence to noland_mic_source.
    Ok(match (has_target_object, has_path) {
        (true, true) => &["target-object", "path"],
        (true, false) => &["target-object"],
        (false, true) => &["path"],
        _ => unreachable!(),
    })
}

fn attach_jitterbuffer_observer(
    pipeline: &gst::Pipeline,
    jitterbuffer: Arc<Mutex<Option<gst::Element>>>,
) -> Result<(), String> {
    let rtpbin = pipeline
        .by_name("rtp_session")
        .ok_or_else(|| "receiver pipeline missing rtp_session".to_string())?;
    rtpbin.connect("new-jitterbuffer", false, move |values| {
        if let Ok(element) = values[1].get::<gst::Element>() {
            let mut guard = match jitterbuffer.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            *guard = Some(element);
        }
        None
    });
    Ok(())
}

fn attach_packet_probe(
    pipeline: &gst::Pipeline,
    config: &ReceiverConfig,
    shared: Arc<Mutex<SharedStatus>>,
) -> Result<(), String> {
    let source = pipeline
        .by_name("rtp_source")
        .ok_or_else(|| "receiver pipeline missing rtp_source".to_string())?;
    let pad = source
        .static_pad("src")
        .ok_or_else(|| "receiver rtp_source is missing its src pad".to_string())?;
    let maximum_packet_size = config.network.maximum_packet_size;
    let expected_ssrc = config.session.expected_ssrc;

    pad.add_probe(gst::PadProbeType::BUFFER, move |_, probe_info| {
        let Some(buffer) = probe_info.buffer() else {
            return gst::PadProbeReturn::Ok;
        };
        let now = Instant::now();
        let mut status = lock_status(&shared);
        match inspect_rtp_packet(
            buffer,
            now,
            maximum_packet_size,
            expected_ssrc,
            &mut status.tracker,
        ) {
            PacketDisposition::Accept => {
                status.last_packet_at = Some(now);
                gst::PadProbeReturn::Ok
            }
            PacketDisposition::Drop => gst::PadProbeReturn::Drop,
        }
    });
    Ok(())
}

fn inspect_rtp_packet(
    buffer: &gst::BufferRef,
    now: Instant,
    maximum_packet_size: usize,
    expected_ssrc: Option<u32>,
    tracker: &mut RtpTracker,
) -> PacketDisposition {
    if buffer.size() > maximum_packet_size {
        tracker.oversized = tracker.oversized.saturating_add(1);
        return PacketDisposition::Drop;
    }
    let Ok(map) = buffer.map_readable() else {
        tracker.rejected = tracker.rejected.saturating_add(1);
        return PacketDisposition::Drop;
    };
    let packet = map.as_slice();
    if packet.len() < 12 || packet[0] >> 6 != 2 || packet[1] & 0x7f != RTP_PAYLOAD_TYPE {
        tracker.rejected = tracker.rejected.saturating_add(1);
        return PacketDisposition::Drop;
    }

    let sequence = u16::from_be_bytes([packet[2], packet[3]]);
    let timestamp = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
    if expected_ssrc.is_some_and(|expected| expected != ssrc) {
        tracker.rejected = tracker.rejected.saturating_add(1);
        return PacketDisposition::Drop;
    }

    if tracker.observe(sequence, timestamp, now) {
        PacketDisposition::Accept
    } else {
        PacketDisposition::Drop
    }
}

fn attach_decode_probe(
    pipeline: &gst::Pipeline,
    shared: Arc<Mutex<SharedStatus>>,
) -> Result<(), String> {
    let decoded = pipeline
        .by_name("decoded_probe")
        .ok_or_else(|| "receiver pipeline missing decoded_probe".to_string())?;
    let pad = decoded
        .static_pad("src")
        .ok_or_else(|| "decoded_probe is missing its src pad".to_string())?;
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        let mut status = lock_status(&shared);
        status.decoded_buffers = status.decoded_buffers.saturating_add(1);
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

fn attach_bus_watch(
    pipeline: &gst::Pipeline,
    main_loop: glib::MainLoop,
    shared: Arc<Mutex<SharedStatus>>,
) -> Result<gst::bus::BusWatchGuard, String> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| "receiver pipeline bus is unavailable".to_string())?;

    bus.add_watch_local(move |_, message| match message.view() {
        gst::MessageView::Error(error_message) => {
            let debug_info = error_message.debug().unwrap_or_default().to_string();
            let source = error_message
                .src()
                .map(|src| src.path_string().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let detail = format!(
                "receiver pipeline error from {source}: {} ({debug_info})",
                error_message.error()
            );
            {
                let mut status = lock_status(&shared);
                if source.contains("pipewire_sink") {
                    status.pipewire_errors = status.pipewire_errors.saturating_add(1);
                }
                status.pipeline_error = Some(detail.clone());
            }
            error!(source = %source, error = %error_message.error(), debug = %debug_info, "receiver pipeline error");
            let error_loop = main_loop.clone();
            glib::timeout_add_local_once(Duration::from_millis(300), move || error_loop.quit());
            ControlFlow::Break
        }
        gst::MessageView::Warning(warning_message) => {
            warn!(
                source = warning_message.src().map(|src| src.path_string().to_string()),
                warning = %warning_message.error(),
                debug = %warning_message.debug().unwrap_or_default(),
                "receiver pipeline warning"
            );
            ControlFlow::Continue
        }
        gst::MessageView::Eos(..) => {
            info!("receiver pipeline received EOS");
            main_loop.quit();
            ControlFlow::Break
        }
        _ => ControlFlow::Continue,
    })
    .map_err(|error| format!("failed attaching receiver bus watch: {error}"))
}

fn attach_status_writer(
    config: &ReceiverConfig,
    jitterbuffer: Arc<Mutex<Option<gst::Element>>>,
    shared: Arc<Mutex<SharedStatus>>,
    status_path: String,
    running: Arc<AtomicBool>,
    main_loop: glib::MainLoop,
) {
    let session_id = config.session.session_id.clone();
    let rtcp_configured = config.session.expected_peer_ip.is_some();
    let mut latency_controller = AdaptiveJitterBufferController::new(
        config.jitter.minimum_ms,
        config.jitter.initial_ms,
        config.jitter.maximum_ms,
    );

    glib::timeout_add_local(Duration::from_millis(250), move || {
        let mut jitter_stats = collect_jitterbuffer_stats(
            &jitterbuffer,
            f64::from(latency_controller.current_latency_ms()),
        );
        let (received_packets, tracker_jitter_ms) = {
            let status = lock_status(&shared);
            (status.tracker.received, status.tracker.jitter_ms())
        };
        let observed_jitter_ms = jitter_stats.jitter_ms.unwrap_or(tracker_jitter_ms);
        latency_controller.observe(observed_jitter_ms, jitter_stats.late, received_packets);
        if apply_jitterbuffer_latency(&jitterbuffer, latency_controller.current_latency_ms()) {
            jitter_stats.buffer_depth_ms = f64::from(latency_controller.current_latency_ms());
        }

        let runtime_status =
            collect_runtime_status(&session_id, &shared, jitter_stats, rtcp_configured);
        if let Err(error) = write_runtime_status(&status_path, &runtime_status) {
            warn!(path = %status_path, error = %error, "failed writing receiver status");
        }

        if running.load(Ordering::SeqCst) {
            ControlFlow::Continue
        } else {
            main_loop.quit();
            ControlFlow::Break
        }
    });
}

fn attach_shutdown_watch(running: Arc<AtomicBool>, main_loop: glib::MainLoop) {
    glib::timeout_add_local(Duration::from_millis(100), move || {
        if running.load(Ordering::SeqCst) {
            ControlFlow::Continue
        } else {
            main_loop.quit();
            ControlFlow::Break
        }
    });
}

#[derive(Debug)]
struct AdaptiveJitterBufferController {
    minimum_ms: u32,
    maximum_ms: u32,
    current_ms: u32,
    stable_samples: u16,
    increase_cooldown: u8,
    previous_late_packets: u64,
    previous_received_packets: u64,
}

impl AdaptiveJitterBufferController {
    fn new(minimum_ms: u32, initial_ms: u32, maximum_ms: u32) -> Self {
        let minimum_ms = minimum_ms.clamp(ABSOLUTE_MINIMUM_JITTER_MS, ABSOLUTE_MAXIMUM_JITTER_MS);
        let maximum_ms = maximum_ms.clamp(ABSOLUTE_MINIMUM_JITTER_MS, ABSOLUTE_MAXIMUM_JITTER_MS);
        let (minimum_ms, maximum_ms) = if minimum_ms <= maximum_ms {
            (minimum_ms, maximum_ms)
        } else {
            let fallback_ms =
                initial_ms.clamp(ABSOLUTE_MINIMUM_JITTER_MS, ABSOLUTE_MAXIMUM_JITTER_MS);
            (fallback_ms, fallback_ms)
        };

        Self {
            minimum_ms,
            maximum_ms,
            current_ms: initial_ms.clamp(minimum_ms, maximum_ms),
            stable_samples: 0,
            increase_cooldown: 0,
            previous_late_packets: 0,
            previous_received_packets: 0,
        }
    }

    fn current_latency_ms(&self) -> u32 {
        self.current_ms
    }

    fn observe(&mut self, jitter_ms: f64, late_packets: u64, received_packets: u64) {
        let late_delta = late_packets.saturating_sub(self.previous_late_packets);
        let receiving = received_packets > self.previous_received_packets;
        self.previous_late_packets = late_packets;
        self.previous_received_packets = received_packets;
        self.increase_cooldown = self.increase_cooldown.saturating_sub(1);

        let unstable_jitter = receiving && jitter_ms >= f64::from(self.current_ms) * 0.5;
        if late_delta > 0 || unstable_jitter {
            self.stable_samples = 0;
            if self.increase_cooldown == 0 {
                self.current_ms = self
                    .current_ms
                    .saturating_add(JITTER_INCREASE_STEP_MS)
                    .min(self.maximum_ms);
                self.increase_cooldown = INCREASE_COOLDOWN_SAMPLES;
            }
            return;
        }

        if receiving {
            self.stable_samples = self.stable_samples.saturating_add(1);
            if self.stable_samples >= STABLE_SAMPLES_BEFORE_DECREASE {
                self.current_ms = self
                    .current_ms
                    .saturating_sub(JITTER_DECREASE_STEP_MS)
                    .max(self.minimum_ms);
                self.stable_samples = 0;
            }
        } else {
            self.stable_samples = 0;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct JitterbufferStats {
    late: u64,
    duplicates: u64,
    jitter_ms: Option<f64>,
    buffer_depth_ms: f64,
}

fn apply_jitterbuffer_latency(
    jitterbuffer: &Arc<Mutex<Option<gst::Element>>>,
    latency_ms: u32,
) -> bool {
    let guard = match jitterbuffer.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let Some(jitter) = guard.as_ref() else {
        return false;
    };
    if jitter.find_property("latency").is_none() {
        return false;
    }

    if jitter.property::<u32>("latency") != latency_ms {
        jitter.set_property("latency", latency_ms);
        info!(latency_ms, "adjusted receiver jitterbuffer latency");
    }
    true
}

fn collect_jitterbuffer_stats(
    jitterbuffer: &Arc<Mutex<Option<gst::Element>>>,
    configured_latency_ms: f64,
) -> JitterbufferStats {
    let guard = match jitterbuffer.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let Some(jitter) = guard.as_ref() else {
        return JitterbufferStats {
            late: 0,
            duplicates: 0,
            jitter_ms: None,
            buffer_depth_ms: configured_latency_ms,
        };
    };

    let stats = jitter.property_value("stats").get::<gst::Structure>().ok();
    let late = stats
        .as_ref()
        .and_then(|value| get_structure_u64(value, &["num-late", "late"]))
        .unwrap_or(0);
    let duplicates = stats
        .as_ref()
        .and_then(|value| get_structure_u64(value, &["num-duplicates", "duplicates"]))
        .unwrap_or(0);
    let jitter_ms = stats
        .as_ref()
        .and_then(|value| get_structure_u64(value, &["avg-jitter", "jitter"]))
        .map(|nanoseconds| nanoseconds as f64 / 1_000_000.0);
    let buffer_depth_ms = if jitter.find_property("latency").is_some() {
        jitter.property::<u32>("latency") as f64
    } else {
        configured_latency_ms
    };

    JitterbufferStats {
        late,
        duplicates,
        jitter_ms,
        buffer_depth_ms,
    }
}

fn collect_runtime_status(
    session_id: &str,
    shared: &Arc<Mutex<SharedStatus>>,
    jitterbuffer: JitterbufferStats,
    rtcp_configured: bool,
) -> ReceiverRuntimeStatus {
    let status = lock_status(shared);
    let last_packet_ms_ago = status
        .last_packet_at
        .map(|instant| instant.elapsed().as_millis() as u64);
    let receiving_audio =
        last_packet_ms_ago.is_some_and(|elapsed| elapsed <= ACTIVE_PACKET_WINDOW_MS);
    let total_packets = status.tracker.received.saturating_add(status.tracker.lost);
    let packet_loss_percent = loss_percent(status.tracker.received, status.tracker.lost);
    let duplicate_packets = status.tracker.duplicates.max(jitterbuffer.duplicates);
    let pipewire_ok = status.pipewire_errors == 0 && status.pipeline_error.is_none();
    let source_authorized = status.tracker.rejected == 0;

    ReceiverRuntimeStatus {
        session_id: session_id.to_string(),
        receiving_audio,
        received_packets: status.tracker.received,
        lost_packets: status.tracker.lost,
        late_packets: jitterbuffer.late,
        out_of_order_packets: status.tracker.out_of_order,
        duplicate_packets,
        rejected_packets: status.tracker.rejected,
        oversized_packets: status.tracker.oversized,
        packet_loss_percent: if total_packets == 0 {
            0.0
        } else {
            packet_loss_percent
        },
        jitter_ms: jitterbuffer.jitter_ms.unwrap_or(status.tracker.jitter_ms()),
        buffer_depth_ms: jitterbuffer.buffer_depth_ms,
        decoded_buffers: status.decoded_buffers,
        plc_estimate: status.tracker.lost.min(status.decoded_buffers),
        pipewire_errors: status.pipewire_errors,
        // GStreamer's rtpbin does not expose a stable, aggregate RTT property.
        // Keep the field nullable until an SR/RR source-stat value is available.
        rtt_ms: None,
        last_packet_ms_ago,
        uptime_seconds: status.started_at.elapsed().as_secs(),
        health: HealthFlags {
            healthy: pipewire_ok && source_authorized,
            network_receiving: receiving_audio,
            pipewire_ok,
            source_authorized,
            rtcp_configured,
        },
    }
}

impl RtpTracker {
    fn observe(&mut self, sequence: u16, timestamp: u32, arrival: Instant) -> bool {
        let extended = match self.max_sequence {
            None => {
                self.max_sequence = Some(sequence);
                self.max_extended_sequence = sequence as u64;
                sequence as u64
            }
            Some(max_sequence) => {
                let delta = sequence.wrapping_sub(max_sequence) as i16;
                if delta > 0 {
                    let extended = self.max_extended_sequence.saturating_add(delta as u64);
                    self.lost = self.lost.saturating_add((delta as u64).saturating_sub(1));
                    self.max_sequence = Some(sequence);
                    self.max_extended_sequence = extended;
                    extended
                } else {
                    let distance = (-i32::from(delta)) as u64;
                    let extended = self.max_extended_sequence.saturating_sub(distance);
                    if self.seen.contains(&extended) {
                        self.duplicates = self.duplicates.saturating_add(1);
                        return false;
                    }
                    self.out_of_order = self.out_of_order.saturating_add(1);
                    self.lost = self.lost.saturating_sub(1);
                    extended
                }
            }
        };

        if !self.seen.insert(extended) {
            self.duplicates = self.duplicates.saturating_add(1);
            return false;
        }
        self.received = self.received.saturating_add(1);
        self.update_jitter(timestamp, arrival);

        let oldest = self
            .max_extended_sequence
            .saturating_sub(RECENT_SEQUENCE_WINDOW);
        self.seen.retain(|value| *value >= oldest);
        true
    }

    fn update_jitter(&mut self, timestamp: u32, arrival: Instant) {
        let base = *self.base_arrival.get_or_insert(arrival);
        let arrival_timestamp_units =
            arrival.duration_since(base).as_secs_f64() * RTP_CLOCK_RATE as f64;
        let transit = arrival_timestamp_units - timestamp as f64;
        if let Some(previous) = self.previous_transit {
            let difference = (transit - previous).abs();
            self.jitter_timestamp_units += (difference - self.jitter_timestamp_units) / 16.0;
        }
        self.previous_transit = Some(transit);
    }

    fn jitter_ms(&self) -> f64 {
        self.jitter_timestamp_units * 1_000.0 / RTP_CLOCK_RATE as f64
    }
}

fn loss_percent(received: u64, lost: u64) -> f64 {
    let total = received.saturating_add(lost);
    if total == 0 {
        0.0
    } else {
        lost as f64 * 100.0 / total as f64
    }
}

fn get_structure_u64(structure: &gst::Structure, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Ok(value) = structure.get::<u64>(*key) {
            return Some(value);
        }
        if let Ok(value) = structure.get::<u32>(*key) {
            return Some(value as u64);
        }
        if let Ok(value) = structure.get::<i64>(*key) {
            if value >= 0 {
                return Some(value as u64);
            }
        }
        if let Ok(value) = structure.get::<i32>(*key) {
            if value >= 0 {
                return Some(value as u64);
            }
        }
    }
    None
}

fn write_runtime_status(path: &str, status: &ReceiverRuntimeStatus) -> std::io::Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = format!("{path}.tmp");
    let payload =
        serde_json::to_vec(status).map_err(|error| std::io::Error::other(error.to_string()))?;
    fs::write(&tmp_path, payload)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

fn lock_status(shared: &Arc<Mutex<SharedStatus>>) -> std::sync::MutexGuard<'_, SharedStatus> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipewire_target_properties_sets_every_available_binding() {
        let both = pipewire_target_properties(true, true).unwrap();
        assert!(both.contains(&"target-object"));
        assert!(both.contains(&"path"));
        assert_eq!(
            pipewire_target_properties(true, false).unwrap().to_vec(),
            vec!["target-object"]
        );
        assert_eq!(
            pipewire_target_properties(false, true).unwrap().to_vec(),
            vec!["path"]
        );
        assert!(pipewire_target_properties(false, false).is_err());
    }

    #[test]
    fn sequence_tracker_counts_loss_reorder_and_duplicates() {
        let start = Instant::now();
        let mut tracker = RtpTracker::default();
        assert!(tracker.observe(100, 0, start));
        assert!(tracker.observe(102, 960, start + Duration::from_millis(20)));
        assert_eq!(tracker.lost, 1);

        assert!(tracker.observe(101, 480, start + Duration::from_millis(21)));
        assert_eq!(tracker.lost, 0);
        assert_eq!(tracker.out_of_order, 1);

        assert!(!tracker.observe(101, 480, start + Duration::from_millis(22)));
        assert_eq!(tracker.duplicates, 1);
        assert_eq!(tracker.received, 3);
    }

    #[test]
    fn sequence_tracker_handles_wraparound() {
        let start = Instant::now();
        let mut tracker = RtpTracker::default();
        assert!(tracker.observe(u16::MAX, 0, start));
        assert!(tracker.observe(0, 480, start + Duration::from_millis(10)));
        assert_eq!(tracker.max_extended_sequence, 65_536);
        assert_eq!(tracker.lost, 0);
    }

    #[test]
    fn status_loss_calculation_is_bounded_and_zero_safe() {
        assert_eq!(loss_percent(0, 0), 0.0);
        assert!((loss_percent(90, 10) - 10.0).abs() < f64::EPSILON);
        assert!((loss_percent(0, 10) - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn adaptive_jitter_starts_at_initial_and_respects_configured_bounds() {
        let controller = AdaptiveJitterBufferController::new(14, 22, 38);
        assert_eq!(controller.current_latency_ms(), 22);

        let clamped = AdaptiveJitterBufferController::new(5, 80, 100);
        assert_eq!(clamped.minimum_ms, ABSOLUTE_MINIMUM_JITTER_MS);
        assert_eq!(clamped.maximum_ms, ABSOLUTE_MAXIMUM_JITTER_MS);
        assert_eq!(clamped.current_latency_ms(), ABSOLUTE_MAXIMUM_JITTER_MS);
    }

    #[test]
    fn adaptive_jitter_increases_modestly_for_jitter_and_late_packets() {
        let mut controller = AdaptiveJitterBufferController::new(10, 20, 40);

        controller.observe(11.0, 0, 1);
        assert_eq!(controller.current_latency_ms(), 24);

        for received in 2..=4 {
            controller.observe(20.0, 0, received);
        }
        assert_eq!(controller.current_latency_ms(), 24);

        controller.observe(0.0, 1, 5);
        assert_eq!(controller.current_latency_ms(), 28);

        for received in 6..=40 {
            controller.observe(20.0, received - 4, received);
        }
        assert_eq!(controller.current_latency_ms(), 40);
    }

    #[test]
    fn adaptive_jitter_decreases_after_sustained_stability_and_does_not_ratchet() {
        let mut controller = AdaptiveJitterBufferController::new(10, 20, 40);
        controller.observe(11.0, 0, 1);
        assert_eq!(controller.current_latency_ms(), 24);

        let stable_samples = usize::from(STABLE_SAMPLES_BEFORE_DECREASE) * (24usize - 10usize);
        for sample in 0..stable_samples {
            controller.observe(0.0, 0, sample as u64 + 2);
        }

        assert_eq!(controller.current_latency_ms(), 10);
        for sample in 0..usize::from(STABLE_SAMPLES_BEFORE_DECREASE) {
            controller.observe(0.0, 0, stable_samples as u64 + sample as u64 + 2);
        }
        assert_eq!(controller.current_latency_ms(), 10);
    }
}
