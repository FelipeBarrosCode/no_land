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

use crate::config::{ReceiverConfig, SecurityConfig};

const RTP_PAYLOAD_TYPE: u32 = 96;
const ACTIVE_PACKET_WINDOW_MS: u64 = 750;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverRuntimeStatus {
    pub receiving_audio: bool,
    pub packet_loss_percent: f64,
    pub jitter_ms: f64,
    pub buffer_depth_ms: f64,
    pub last_packet_ms_ago: Option<u64>,
}

impl Default for ReceiverRuntimeStatus {
    fn default() -> Self {
        Self {
            receiving_audio: false,
            packet_loss_percent: 0.0,
            jitter_ms: 0.0,
            buffer_depth_ms: 0.0,
            last_packet_ms_ago: None,
        }
    }
}

#[derive(Debug, Default)]
struct SharedStatus {
    last_packet_at: Option<Instant>,
    packets_received: u64,
    packets_lost: u64,
    jitter_ms: f64,
    buffer_depth_ms: f64,
}

pub struct Receiver {
    pipeline: gst::Pipeline,
    main_loop: glib::MainLoop,
    _bus_watch: gst::bus::BusWatchGuard,
}

impl Receiver {
    pub fn new(
        config: &ReceiverConfig,
        status_path: &str,
        running: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        gst::init().map_err(|error| format!("Failed initializing GStreamer: {error}"))?;

        let pipeline = build_pipeline(config)?;
        let main_loop = glib::MainLoop::new(None, false);
        let shared = Arc::new(Mutex::new(SharedStatus::default()));

        attach_packet_probe(&pipeline, shared.clone())?;
        let bus_watch = attach_bus_watch(&pipeline, main_loop.clone())?;
        attach_status_writer(
            &pipeline,
            shared,
            status_path.to_string(),
            running.clone(),
            main_loop.clone(),
        )?;
        attach_shutdown_watch(running, main_loop.clone());

        Ok(Self {
            pipeline,
            main_loop,
            _bus_watch: bus_watch,
        })
    }

    pub fn run(&self) -> Result<(), String> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| format!("Failed starting receiver pipeline: {error:?}"))?;
        info!("Receiver pipeline started");

        self.main_loop.run();

        let _ = self.pipeline.send_event(gst::event::Eos::new());
        let _ = self.pipeline.set_state(gst::State::Null);
        info!("Receiver pipeline stopped");
        Ok(())
    }
}

fn build_pipeline(config: &ReceiverConfig) -> Result<gst::Pipeline, String> {
    let pipeline_description = format!(
        concat!(
            "udpsrc name=udp_source address={bind_address} port={port} ",
            "caps=\"application/x-rtp,media=audio,clock-rate=48000,encoding-name=OPUS,payload={payload}\" ",
            "! rtpjitterbuffer name=jitter latency={latency} drop-on-latency=true do-lost=true ",
            "! rtpopusdepay ",
            "! opusdec ",
            "! audioconvert ",
            "! audioresample ",
            "! audio/x-raw,format=S16LE,rate={sample_rate},channels={channels} ",
            "! fdsink fd=1 sync=false"
        ),
        bind_address = config.network.bind_address,
        port = config.network.port,
        payload = RTP_PAYLOAD_TYPE,
        latency = config.jitter.initial_ms.round().max(1.0) as u32,
        sample_rate = config.audio.sample_rate,
        channels = config.audio.channels,
    );

    let element = gst::parse::launch(&pipeline_description)
        .map_err(|error| format!("Failed creating receiver pipeline: {error}"))?;
    element
        .downcast::<gst::Pipeline>()
        .map_err(|_| "GStreamer receiver pipeline did not produce a Pipeline".to_string())
}

fn attach_packet_probe(
    pipeline: &gst::Pipeline,
    shared: Arc<Mutex<SharedStatus>>,
) -> Result<(), String> {
    let source = pipeline
        .by_name("udp_source")
        .ok_or_else(|| "Receiver pipeline missing udp_source element".to_string())?;
    let pad = source
        .static_pad("src")
        .ok_or_else(|| "Receiver udp_source element missing src pad".to_string())?;

    pad.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
        move |_, _| {
            if let Ok(mut status) = shared.lock() {
                status.last_packet_at = Some(Instant::now());
                status.packets_received = status.packets_received.saturating_add(1);
            }
            gst::PadProbeReturn::Ok
        },
    );
    Ok(())
}

fn attach_bus_watch(
    pipeline: &gst::Pipeline,
    main_loop: glib::MainLoop,
) -> Result<gst::bus::BusWatchGuard, String> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| "Receiver pipeline bus is unavailable".to_string())?;

    let watch = bus
        .add_watch_local(move |_, message| match message.view() {
            gst::MessageView::Error(error_message) => {
                let debug_info = error_message.debug().unwrap_or_default().to_string();
                let src_path = error_message
                    .src()
                    .map(|src| src.path_string().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                error!(
                    src = %src_path,
                    error = %error_message.error(),
                    debug = %debug_info,
                    "Receiver pipeline error"
                );
                main_loop.quit();
                ControlFlow::Break
            }
            gst::MessageView::Eos(..) => {
                info!("Receiver pipeline received EOS");
                main_loop.quit();
                ControlFlow::Break
            }
            _ => ControlFlow::Continue,
        })
        .map_err(|error| format!("Failed attaching receiver bus watch: {error}"))?;

    Ok(watch)
}

fn attach_status_writer(
    pipeline: &gst::Pipeline,
    shared: Arc<Mutex<SharedStatus>>,
    status_path: String,
    running: Arc<AtomicBool>,
    main_loop: glib::MainLoop,
) -> Result<(), String> {
    let jitter = pipeline
        .by_name("jitter")
        .ok_or_else(|| "Receiver pipeline missing jitter buffer".to_string())?;

    glib::timeout_add_local(Duration::from_millis(250), move || {
        let runtime_status = collect_runtime_status(&jitter, &shared);
        if let Err(error) = write_runtime_status(&status_path, &runtime_status) {
            warn!(path = %status_path, "Failed writing receiver runtime status: {error}");
        }

        if running.load(Ordering::SeqCst) {
            ControlFlow::Continue
        } else {
            main_loop.quit();
            ControlFlow::Break
        }
    });

    Ok(())
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

fn collect_runtime_status(
    jitter: &gst::Element,
    shared: &Arc<Mutex<SharedStatus>>,
) -> ReceiverRuntimeStatus {
    let latency_ms = if jitter.find_property("latency").is_some() {
        jitter.property::<u32>("latency") as f64
    } else {
        0.0
    };

    let stats_value = jitter.property_value("stats");
    let stats = stats_value.get::<gst::Structure>().ok();
    let packets_lost = stats
        .as_ref()
        .and_then(|structure| get_structure_u64(structure, &["num-lost", "lost", "packets-lost"]))
        .unwrap_or(0);
    let jitter_ms = stats
        .as_ref()
        .and_then(|structure| {
            get_structure_u64(
                structure,
                &["avg-jitter", "jitter", "avg_jitter", "estimated-jitter"],
            )
        })
        .map(|nanos| nanos as f64 / 1_000_000.0)
        .unwrap_or_default();

    let mut guard = match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.packets_lost = packets_lost;
    guard.jitter_ms = jitter_ms;
    guard.buffer_depth_ms = latency_ms;

    let last_packet_ms_ago = guard
        .last_packet_at
        .map(|instant| instant.elapsed().as_millis() as u64);
    let receiving_audio = last_packet_ms_ago
        .map(|elapsed| elapsed <= ACTIVE_PACKET_WINDOW_MS)
        .unwrap_or(false);
    let total_packets = guard.packets_received.saturating_add(guard.packets_lost);
    let packet_loss_percent = if total_packets > 0 {
        (guard.packets_lost as f64 / total_packets as f64) * 100.0
    } else {
        0.0
    };

    ReceiverRuntimeStatus {
        receiving_audio,
        packet_loss_percent,
        jitter_ms,
        buffer_depth_ms: latency_ms,
        last_packet_ms_ago,
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
    let payload = serde_json::to_vec(status)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error.to_string()))?;
    fs::write(&tmp_path, payload)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

#[allow(dead_code)]
fn _security_mode(_security: &SecurityConfig) {}
