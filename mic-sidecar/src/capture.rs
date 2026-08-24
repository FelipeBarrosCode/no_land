use crate::metrics::{CaptureBackend, Metrics};
use crate::ring::AudioRing;
use arc_swap::ArcSwap;
#[cfg(not(target_os = "macos"))]
use cpal::traits::StreamTrait;
use cpal::traits::{DeviceTrait, HostTrait};
#[cfg(not(target_os = "macos"))]
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig, SupportedStreamConfig};
#[cfg(target_os = "macos")]
use gst::prelude::*;
#[cfg(target_os = "macos")]
use gstreamer as gst;
#[cfg(target_os = "macos")]
use gstreamer_app as gst_app;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const TARGET_SAMPLE_RATE: u32 = 48_000;
pub const RING_MILLIS: usize = 120;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    #[default]
    Microphone,
    Sine,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub sample_rates: Vec<u32>,
    pub channels: u16,
    pub id_stability: IdStability,
    pub id_note: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum IdStability {
    Fallback,
}

pub struct AudioInput {
    pub ring: Arc<AudioRing>,
    pub sample_rate: u32,
    pub generation: u64,
}

impl AudioInput {
    pub fn silent() -> Arc<Self> {
        Arc::new(Self {
            ring: Arc::new(AudioRing::new(ring_capacity(TARGET_SAMPLE_RATE))),
            sample_rate: TARGET_SAMPLE_RATE,
            generation: 0,
        })
    }
}

pub type SharedInput = Arc<ArcSwap<AudioInput>>;

#[derive(Debug)]
pub enum CaptureSignal {
    Error(String),
}

#[derive(Debug, Clone)]
pub struct CaptureStarted {
    pub active_device_id: String,
    pub active_device_name: String,
    pub sample_rate: u32,
    pub capture_backend: CaptureBackend,
    pub used_fallback: bool,
}

enum CaptureHandle {
    #[cfg(not(target_os = "macos"))]
    Native { _stream: cpal::Stream },
    #[cfg(target_os = "macos")]
    GStreamer {
        pipeline: gst::Pipeline,
        stop: Arc<AtomicBool>,
        monitor: Option<JoinHandle<()>>,
    },
    Synthetic {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    },
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        match self {
            #[cfg(target_os = "macos")]
            Self::GStreamer {
                pipeline,
                stop,
                monitor,
            } => {
                stop.store(true, Ordering::Release);
                let _ = pipeline.set_state(gst::State::Null);
                if let Some(monitor) = monitor.take() {
                    let _ = monitor.join();
                }
            }
            Self::Synthetic { stop, thread } => {
                stop.store(true, Ordering::Release);
                if let Some(thread) = thread.take() {
                    let _ = thread.join();
                }
            }
            #[cfg(not(target_os = "macos"))]
            Self::Native { .. } => {}
        }
    }
}

pub struct CaptureController {
    handle: Option<CaptureHandle>,
    generation: u64,
    active_device_id: Option<String>,
}

impl Default for CaptureController {
    fn default() -> Self {
        Self {
            handle: None,
            generation: 0,
            active_device_id: None,
        }
    }
}

impl CaptureController {
    pub fn active_device_id(&self) -> Option<&str> {
        self.active_device_id.as_deref()
    }

    pub fn stop(&mut self) {
        self.handle.take();
        self.active_device_id = None;
    }

    pub fn start(
        &mut self,
        source: SourceKind,
        requested_device_id: Option<&str>,
        shared: &SharedInput,
        metrics: Arc<Metrics>,
        signals: Sender<CaptureSignal>,
    ) -> Result<CaptureStarted, String> {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (handle, input, started) = match source {
            SourceKind::Sine => start_sine(generation, metrics.clone()),
            SourceKind::Microphone => {
                #[cfg(target_os = "macos")]
                {
                    start_gstreamer_macos(
                        requested_device_id,
                        generation,
                        metrics.clone(),
                        signals,
                    )?
                }
                #[cfg(not(target_os = "macos"))]
                {
                    start_native(requested_device_id, generation, metrics.clone(), signals)?
                }
            }
        };

        metrics.set_capture_backend(started.capture_backend);
        shared.store(input);
        self.handle = Some(handle);
        self.active_device_id = Some(started.active_device_id.clone());
        Ok(started)
    }
}

pub fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    Ok(enumerate_devices()?
        .into_iter()
        .map(|(_, info)| info)
        .collect())
}

pub fn device_available(device_id: &str) -> bool {
    enumerate_devices()
        .map(|devices| {
            devices
                .iter()
                .any(|(_, info)| info.id == device_id || info.name == device_id)
        })
        .unwrap_or(false)
}

pub fn current_default_device_id() -> Option<String> {
    enumerate_devices().ok().and_then(|devices| {
        devices
            .into_iter()
            .find_map(|(_, info)| info.is_default.then_some(info.id))
    })
}

fn enumerate_devices() -> Result<Vec<(cpal::Device, DeviceInfo)>, String> {
    let host = cpal::default_host();
    let host_name = format!("{:?}", host.id()).to_lowercase();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let devices = host
        .input_devices()
        .map_err(|error| format!("failed enumerating CPAL input devices: {error}"))?;
    let mut occurrences = HashMap::<String, usize>::new();
    let mut result = Vec::new();

    for device in devices {
        let name = device
            .name()
            .unwrap_or_else(|_| "Unnamed input device".to_string());
        let occurrence = occurrences.entry(name.clone()).or_default();
        let id = fallback_device_id(&host_name, &name, *occurrence);
        *occurrence += 1;
        let (sample_rates, channels) = device_capabilities(&device);
        result.push((
            device,
            DeviceInfo {
                id,
                name: name.clone(),
                is_default: default_name.as_deref() == Some(name.as_str()),
                sample_rates,
                channels,
                id_stability: IdStability::Fallback,
                id_note: "CPAL's stable DeviceTrait does not expose a cross-platform device ID; this deterministic host/name/occurrence ID may change if duplicate-name enumeration order changes.".to_string(),
            },
        ));
    }
    Ok(result)
}

fn fallback_device_id(host: &str, name: &str, occurrence: usize) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in host
        .bytes()
        .chain([0])
        .chain(name.bytes())
        .chain([0])
        .chain(occurrence.to_string().bytes())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("cpal-fallback-{hash:016x}")
}

fn device_capabilities(device: &cpal::Device) -> (Vec<u32>, u16) {
    let Ok(configs) = device.supported_input_configs() else {
        return (Vec::new(), 0);
    };
    let mut rates = BTreeSet::new();
    let mut channels = 0;
    for config in configs {
        channels = channels.max(config.channels());
        rates.insert(config.min_sample_rate().0);
        rates.insert(config.max_sample_rate().0);
        if config.min_sample_rate().0 <= TARGET_SAMPLE_RATE
            && TARGET_SAMPLE_RATE <= config.max_sample_rate().0
        {
            rates.insert(TARGET_SAMPLE_RATE);
        }
    }
    (rates.into_iter().collect(), channels)
}

#[cfg(target_os = "macos")]
fn ensure_mic_permission() -> Result<(), String> {
    // Trigger macOS TCC microphone permission prompt. The sidecar is a
    // separate process from the app and needs its own TCC authorization.
    // Without this, macOS silently provides zero-filled audio buffers.
    extern "C" {
        fn noland_macos_ensure_microphone_access() -> i32;
    }
    let result = unsafe { noland_macos_ensure_microphone_access() };
    match result {
        0 => Ok(()),
        1 => Err("macOS microphone access denied. Grant permission in System Settings > Privacy & Security > Microphone".to_string()),
        2 => Err("macOS microphone permission prompt timed out (30s)".to_string()),
        _ => Err(format!("macOS microphone permission check failed (code {result})")),
    }
}

#[cfg(target_os = "macos")]
fn start_gstreamer_macos(
    requested_device_id: Option<&str>,
    generation: u64,
    metrics: Arc<Metrics>,
    signals: Sender<CaptureSignal>,
) -> Result<(CaptureHandle, Arc<AudioInput>, CaptureStarted), String> {
    ensure_mic_permission()?;
    gst::init().map_err(|error| format!("failed initializing GStreamer capture: {error}"))?;
    let devices = enumerate_devices()?;
    if devices.is_empty() {
        return Err("no microphone input devices are available".to_string());
    }

    let requested = requested_device_id.filter(|id| !id.trim().is_empty() && *id != "default");
    let selected_index = requested.and_then(|id| {
        devices
            .iter()
            .position(|(_, info)| info.id == id || info.name == id)
    });
    let used_fallback = requested.is_some() && selected_index.is_none();
    let index = selected_index
        .or_else(|| devices.iter().position(|(_, info)| info.is_default))
        .unwrap_or(0);
    let info = &devices[index].1;

    let source = create_macos_gstreamer_source(&info.name)?;
    set_i64_property_if_present(&source, "buffer-time", 40_000);
    set_i64_property_if_present(&source, "latency-time", 10_000);
    set_bool_property_if_present(&source, "do-timestamp", true);

    let convert = gst::ElementFactory::make("audioconvert")
        .build()
        .map_err(|_| "required GStreamer element 'audioconvert' is unavailable".to_string())?;
    let resample = gst::ElementFactory::make("audioresample")
        .build()
        .map_err(|_| "required GStreamer element 'audioresample' is unavailable".to_string())?;
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .build()
        .map_err(|_| "required GStreamer element 'capsfilter' is unavailable".to_string())?;
    let appsink_element = gst::ElementFactory::make("appsink")
        .name("macos_capture_sink")
        .build()
        .map_err(|_| "required GStreamer element 'appsink' is unavailable".to_string())?;
    let appsink = appsink_element
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "GStreamer appsink has an unexpected type".to_string())?;

    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("rate", TARGET_SAMPLE_RATE as i32)
        .field("channels", 1i32)
        .build();
    capsfilter.set_property("caps", &caps);
    appsink.set_sync(false);
    appsink.set_max_buffers(2);
    appsink.set_drop(true);
    appsink.set_wait_on_eos(false);

    let input = Arc::new(AudioInput {
        ring: Arc::new(AudioRing::new(ring_capacity(TARGET_SAMPLE_RATE))),
        sample_rate: TARGET_SAMPLE_RATE,
        generation,
    });
    let callback_input = input.clone();
    let callback_metrics = metrics.clone();
    let callback_signals = signals.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let bytes = map.as_slice();
                if bytes.len() % 2 != 0 {
                    let _ = callback_signals.send(CaptureSignal::Error(
                        "macOS GStreamer capture produced a misaligned S16LE buffer".to_string(),
                    ));
                    return Err(gst::FlowError::Error);
                }
                let sample_count = bytes.len() / 2;
                record_capture_samples(
                    callback_input.as_ref(),
                    callback_metrics.as_ref(),
                    sample_count,
                    bytes
                        .chunks_exact(2)
                        .map(|sample| i16::from_le_bytes([sample[0], sample[1]])),
                );
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([&source, &convert, &resample, &capsfilter, &appsink_element])
        .map_err(|error| format!("failed assembling macOS capture pipeline: {error}"))?;
    gst::Element::link_many([&source, &convert, &resample, &capsfilter, &appsink_element])
        .map_err(|error| format!("failed linking macOS capture pipeline: {error}"))?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|error| format!("failed starting macOS capture pipeline: {error:?}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let monitor_stop = stop.clone();
    let bus = pipeline
        .bus()
        .ok_or_else(|| "macOS capture pipeline has no GStreamer bus".to_string())?;
    let monitor_signals = signals;
    let monitor = thread::Builder::new()
        .name("noland-macos-capture-bus".to_string())
        .spawn(move || {
            while !monitor_stop.load(Ordering::Acquire) {
                let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
                    continue;
                };
                match message.view() {
                    gst::MessageView::Error(error) => {
                        let _ = monitor_signals.send(CaptureSignal::Error(format!(
                            "macOS GStreamer capture failed: {} ({})",
                            error.error(),
                            error.debug().unwrap_or_default()
                        )));
                        break;
                    }
                    gst::MessageView::Eos(..) => {
                        let _ = monitor_signals.send(CaptureSignal::Error(
                            "macOS GStreamer capture reached unexpected EOS".to_string(),
                        ));
                        break;
                    }
                    _ => {}
                }
            }
        })
        .map_err(|error| {
            let _ = pipeline.set_state(gst::State::Null);
            format!("failed starting macOS capture monitor: {error}")
        })?;

    Ok((
        CaptureHandle::GStreamer {
            pipeline,
            stop,
            monitor: Some(monitor),
        },
        input,
        CaptureStarted {
            active_device_id: info.id.clone(),
            active_device_name: info.name.clone(),
            sample_rate: TARGET_SAMPLE_RATE,
            capture_backend: CaptureBackend::GstreamerOsx,
            used_fallback,
        },
    ))
}

#[cfg(target_os = "macos")]
fn create_macos_gstreamer_source(device_name: &str) -> Result<gst::Element, String> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Source"), None);
    monitor
        .start()
        .map_err(|error| format!("failed starting GStreamer device discovery: {error}"))?;

    let devices = monitor.devices();
    eprintln!("[mic-sidecar] GStreamer Audio/Source devices:");
    for d in &devices {
        eprintln!(
            "  display_name={:?}  class={:?}",
            d.display_name().as_str(),
            d.device_class()
        );
    }
    eprintln!("[mic-sidecar] requested device name: {:?}", device_name);

    // Try exact / prefix match first, then fall back to a contains-based match.
    let selected = devices
        .iter()
        .find(|device| macos_device_names_match(device.display_name().as_str(), device_name));
    let selected = selected.or_else(|| {
        devices.iter().find(|device| {
            macos_device_names_loose_match(device.display_name().as_str(), device_name)
        })
    });

    let result = if let Some(device) = selected {
        eprintln!(
            "[mic-sidecar] matched GStreamer device: {:?}",
            device.display_name().as_str()
        );
        // Try osxaudiosrc first with device-name property — it's more
        // reliable than macos_audio_source from the DeviceMonitor which
        // can fail to start IO on some macOS versions.
        let element = gst::ElementFactory::make("osxaudiosrc")
            .name("macos_audio_source")
            .build()
            .map_err(|e| format!("failed creating osxaudiosrc: {e}"))?;
        set_string_property_if_present(&element, "device-name", device_name);
        Ok(element)
    } else {
        eprintln!(
            "[mic-sidecar] no GStreamer device matched '{device_name}', falling back to osxaudiosrc with device-name property"
        );
        // Try setting the device-name property on osxaudiosrc so it resolves
        // to the requested device instead of the system default (which may be
        // an output-only device on some macOS configurations).
        let element = gst::ElementFactory::make("osxaudiosrc")
            .name("macos_audio_source")
            .build()
            .map_err(|_| {
                format!(
                    "GStreamer could not resolve microphone '{device_name}' and osxaudiosrc is unavailable"
                )
            })?;
        // osxaudiosrc may expose a `device-name` property on some GStreamer
        // versions; set it if available.
        set_string_property_if_present(&element, "device-name", device_name);
        Ok(element)
    };
    monitor.stop();
    result
}

/// Probe GStreamer's Audio/Source device monitor and print display names.
/// Used by the `probe-devices` CLI subcommand for debugging device matching.
#[cfg(target_os = "macos")]
pub fn probe_gstreamer_source_devices() -> Result<(), String> {
    gst::init().map_err(|e| format!("gst init: {e}"))?;
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Source"), None);
    monitor.start().map_err(|e| format!("monitor start: {e}"))?;
    println!("GStreamer Audio/Source devices:");
    for d in monitor.devices() {
        println!(
            "  display_name={:?}  class={:?}",
            d.display_name().as_str(),
            d.device_class()
        );
    }
    monitor.stop();
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_device_names_match(gstreamer_name: &str, cpal_name: &str) -> bool {
    let gstreamer = gstreamer_name.trim().to_lowercase();
    let cpal = cpal_name.trim().to_lowercase();
    gstreamer == cpal
        || (gstreamer.len().abs_diff(cpal.len()) <= 4
            && (gstreamer.starts_with(&cpal) || cpal.starts_with(&gstreamer)))
}

/// Looser match: checks whether both names contain a common significant
/// token (e.g., "microphone", "macbook", "iphone").  This handles cases
/// where GStreamer and CPAL use slightly different device naming conventions
/// on macOS (GStreamer may prefix/suffix the CoreAudio device name).
#[cfg(target_os = "macos")]
fn macos_device_names_loose_match(gstreamer_name: &str, cpal_name: &str) -> bool {
    let g = gstreamer_name.trim().to_lowercase();
    let c = cpal_name.trim().to_lowercase();
    // If one name contains the other entirely, that's good enough.
    if g.contains(&c) || c.contains(&g) {
        return true;
    }
    // Check for shared significant tokens (at least 4 chars).
    let g_tokens: Vec<&str> = g.split_whitespace().filter(|t| t.len() >= 4).collect();
    let c_tokens: Vec<&str> = c.split_whitespace().filter(|t| t.len() >= 4).collect();
    let shared = g_tokens
        .iter()
        .filter(|gt| c_tokens.iter().any(|ct| *ct == **gt))
        .count();
    // Require at least 2 shared tokens, or 1 if one of the names is short.
    shared >= 2 || (shared >= 1 && (g_tokens.len() <= 2 || c_tokens.len() <= 2))
}

#[cfg(target_os = "macos")]
fn set_i64_property_if_present(element: &gst::Element, name: &str, value: i64) {
    if element.find_property(name).is_some() {
        element.set_property(name, value);
    }
}

#[cfg(target_os = "macos")]
fn set_bool_property_if_present(element: &gst::Element, name: &str, value: bool) {
    if element.find_property(name).is_some() {
        element.set_property(name, value);
    }
}

#[cfg(target_os = "macos")]
fn set_string_property_if_present(element: &gst::Element, name: &str, value: &str) {
    if element.find_property(name).is_some() {
        element.set_property(name, value.to_string());
    }
}

#[cfg(not(target_os = "macos"))]
fn start_native(
    requested_device_id: Option<&str>,
    generation: u64,
    metrics: Arc<Metrics>,
    signals: Sender<CaptureSignal>,
) -> Result<(CaptureHandle, Arc<AudioInput>, CaptureStarted), String> {
    let devices = enumerate_devices()?;
    if devices.is_empty() {
        return Err("no CPAL input devices are available".to_string());
    }

    let requested = requested_device_id.filter(|id| !id.trim().is_empty() && *id != "default");
    let selected_index = requested.and_then(|id| {
        devices
            .iter()
            .position(|(_, info)| info.id == id || info.name == id)
    });
    let used_fallback = requested.is_some() && selected_index.is_none();
    let index = selected_index
        .or_else(|| devices.iter().position(|(_, info)| info.is_default))
        .unwrap_or(0);
    let (device, info) = &devices[index];
    let supported = choose_input_config(device)?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let sample_rate = config.sample_rate.0;
    let channels = config.channels as usize;
    let input = Arc::new(AudioInput {
        ring: Arc::new(AudioRing::new(ring_capacity(sample_rate))),
        sample_rate,
        generation,
    });
    let callback_input = input.clone();
    let callback_metrics = metrics.clone();
    let error_signals = signals.clone();

    macro_rules! build_stream {
        ($sample:ty) => {{
            build_typed_stream::<$sample>(
                device,
                &config,
                channels,
                callback_input,
                callback_metrics,
                error_signals,
            )
        }};
    }

    let stream = match sample_format {
        SampleFormat::I8 => build_stream!(i8),
        SampleFormat::I16 => build_stream!(i16),
        SampleFormat::I32 => build_stream!(i32),
        SampleFormat::I64 => build_stream!(i64),
        SampleFormat::U8 => build_stream!(u8),
        SampleFormat::U16 => build_stream!(u16),
        SampleFormat::U32 => build_stream!(u32),
        SampleFormat::U64 => build_stream!(u64),
        SampleFormat::F32 => build_stream!(f32),
        SampleFormat::F64 => build_stream!(f64),
        other => Err(format!("unsupported CPAL input sample format {other:?}")),
    }?;
    stream
        .play()
        .map_err(|error| format!("failed starting CPAL input stream: {error}"))?;

    Ok((
        CaptureHandle::Native { _stream: stream },
        input,
        CaptureStarted {
            active_device_id: info.id.clone(),
            active_device_name: info.name.clone(),
            sample_rate,
            capture_backend: CaptureBackend::Cpal,
            used_fallback,
        },
    ))
}

#[cfg(not(target_os = "macos"))]
fn choose_input_config(device: &cpal::Device) -> Result<SupportedStreamConfig, String> {
    let ranges = device
        .supported_input_configs()
        .map_err(|error| format!("failed querying CPAL input formats: {error}"))?;
    let mut best: Option<(u64, SupportedStreamConfig)> = None;
    for range in ranges {
        let min = range.min_sample_rate().0;
        let max = range.max_sample_rate().0;
        let rate = TARGET_SAMPLE_RATE.clamp(min, max);
        let rate_distance = rate.abs_diff(TARGET_SAMPLE_RATE) as u64;
        let channel_penalty = range.channels().saturating_sub(1) as u64 * 1_000_000;
        let score = channel_penalty + rate_distance;
        let config = range.with_sample_rate(cpal::SampleRate(rate));
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score < *best_score)
        {
            best = Some((score, config));
        }
    }
    best.map(|(_, config)| config)
        .or_else(|| device.default_input_config().ok())
        .ok_or_else(|| "input device has no supported CPAL stream configuration".to_string())
}

#[cfg(not(target_os = "macos"))]
fn build_typed_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    input: Arc<AudioInput>,
    metrics: Arc<Metrics>,
    signals: Sender<CaptureSignal>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    i16: FromSample<T>,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let frames = data.len() / channels;
                record_capture_samples(
                    input.as_ref(),
                    metrics.as_ref(),
                    frames,
                    data.chunks_exact(channels)
                        .map(|frame| frame[0].to_sample::<i16>()),
                );
            },
            move |error| {
                let _ = signals.send(CaptureSignal::Error(error.to_string()));
            },
            None,
        )
        .map_err(|error| format!("failed building CPAL input stream: {error}"))
}

fn start_sine(
    generation: u64,
    metrics: Arc<Metrics>,
) -> (CaptureHandle, Arc<AudioInput>, CaptureStarted) {
    let input = Arc::new(AudioInput {
        ring: Arc::new(AudioRing::new(ring_capacity(TARGET_SAMPLE_RATE))),
        sample_rate: TARGET_SAMPLE_RATE,
        generation,
    });
    let thread_input = input.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread = thread::spawn(move || {
        let mut phase = 0.0f32;
        let step = 2.0 * std::f32::consts::PI * 440.0 / TARGET_SAMPLE_RATE as f32;
        let mut samples = vec![0i16; frame_samples(TARGET_SAMPLE_RATE, 10)];
        let mut next = Instant::now();
        while !thread_stop.load(Ordering::Acquire) {
            for sample in &mut samples {
                *sample = (phase.sin() * 0.1 * i16::MAX as f32) as i16;
                phase = (phase + step) % (2.0 * std::f32::consts::PI);
            }
            record_capture_slice(thread_input.as_ref(), metrics.as_ref(), &samples);
            next += Duration::from_millis(10);
            if let Some(delay) = next.checked_duration_since(Instant::now()) {
                thread::sleep(delay);
            } else {
                next = Instant::now();
            }
        }
    });

    (
        CaptureHandle::Synthetic {
            stop,
            thread: Some(thread),
        },
        input,
        CaptureStarted {
            active_device_id: "synthetic-sine".to_string(),
            active_device_name: "Synthetic 440 Hz sine".to_string(),
            sample_rate: TARGET_SAMPLE_RATE,
            capture_backend: CaptureBackend::Synthetic,
            used_fallback: false,
        },
    )
}

fn record_capture_slice(input: &AudioInput, metrics: &Metrics, samples: &[i16]) {
    let nonzero_samples = samples.iter().filter(|sample| **sample != 0).count() as u64;
    let peak = samples
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap_or(0);
    let report = input.ring.push_slice(samples);
    metrics.record_capture(
        samples.len() as u64,
        nonzero_samples,
        peak,
        report.dropped_stale,
        report.overrun,
    );
    metrics.set_ring_depth(input.ring.len());
}

fn record_capture_samples<I>(input: &AudioInput, metrics: &Metrics, sample_count: usize, samples: I)
where
    I: IntoIterator<Item = i16>,
{
    let mut nonzero_samples = 0u64;
    let mut peak = 0u16;
    let samples = samples.into_iter().inspect(|sample| {
        if *sample != 0 {
            nonzero_samples += 1;
        }
        peak = peak.max(sample.unsigned_abs());
    });
    let report = input.ring.push_from_iter(sample_count, samples);
    metrics.record_capture(
        sample_count as u64,
        nonzero_samples,
        peak,
        report.dropped_stale,
        report.overrun,
    );
    metrics.set_ring_depth(input.ring.len());
}

pub fn frame_samples(sample_rate: u32, frame_ms: u32) -> usize {
    ((sample_rate as u64 * frame_ms as u64 + 500) / 1_000) as usize
}

fn ring_capacity(sample_rate: u32) -> usize {
    frame_samples(sample_rate, RING_MILLIS as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sizes_match_common_rates() {
        assert_eq!(frame_samples(48_000, 10), 480);
        assert_eq!(frame_samples(44_100, 10), 441);
        assert_eq!(ring_capacity(48_000), 1_920);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_device_name_matching_accepts_gstreamer_truncation() {
        assert!(macos_device_names_match(
            "MacBook Air Microphone",
            "MacBook Air Microphone"
        ));
        assert!(macos_device_names_match(
            "Felipe’s iPhone Micropho",
            "Felipe’s iPhone Microphone"
        ));
        assert!(!macos_device_names_match(
            "MacBook Air Microphone",
            "Studio Display Microphone"
        ));
    }

    #[test]
    fn capture_levels_count_nonzero_samples_and_peak() {
        let input = AudioInput {
            ring: Arc::new(AudioRing::new(4)),
            sample_rate: TARGET_SAMPLE_RATE,
            generation: 1,
        };
        let metrics = Metrics::default();
        record_capture_samples(&input, &metrics, 4, [0, -12, i16::MIN, 4]);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.captured_samples, 4);
        assert_eq!(snapshot.capture_nonzero_samples, 3);
        assert_eq!(snapshot.capture_peak, 32_768);
        assert_eq!(snapshot.capture_silent_callbacks, 0);
    }

    #[test]
    fn fallback_id_is_deterministic_and_disambiguates_duplicates() {
        assert_eq!(
            fallback_device_id("coreaudio", "Mic", 0),
            fallback_device_id("coreaudio", "Mic", 0)
        );
        assert_ne!(
            fallback_device_id("coreaudio", "Mic", 0),
            fallback_device_id("coreaudio", "Mic", 1)
        );
    }
}
