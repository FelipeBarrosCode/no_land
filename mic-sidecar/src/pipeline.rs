use crate::capture::{frame_samples, SharedInput, TARGET_SAMPLE_RATE};
use crate::metrics::Metrics;
use crate::protocol::SessionConfig;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use rand::Rng;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const RTP_PAYLOAD_TYPE: u32 = 111;
pub const MAX_RTP_PAYLOAD_BYTES: u32 = 1_200;
const OPUS_COMPLEXITY: i32 = 5;
const APP_QUEUE_BUFFERS: u64 = 6;
const APP_QUEUE_TIME_NS: u64 = 60_000_000;
const STARTUP_PREBUFFER_MS: u32 = 30;
const UNDERRUN_GRACE_MS: u64 = 8;
const WAIT_SLICE_MS: u64 = 1;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RtpOffsets {
    pub ssrc: u32,
    pub sequence: u16,
    pub timestamp: u32,
}

impl RtpOffsets {
    pub fn from_config(config: &SessionConfig) -> Self {
        let mut rng = rand::thread_rng();
        Self {
            ssrc: config.ssrc.unwrap_or_else(|| rng.gen()),
            sequence: config.sequence_offset.unwrap_or_else(|| rng.gen()),
            timestamp: config.timestamp_offset.unwrap_or_else(|| rng.gen()),
        }
    }

    #[cfg(test)]
    pub fn advanced(self, packets: u16, samples_48k: u32) -> Self {
        Self {
            ssrc: self.ssrc,
            sequence: self.sequence.wrapping_add(packets),
            timestamp: self.timestamp.wrapping_add(samples_48k),
        }
    }
}

#[derive(Debug, Default)]
pub struct TimestampClock {
    next_pts_ns: u64,
    remainder: u64,
}

impl TimestampClock {
    pub fn next(&mut self, samples: usize, sample_rate: u32) -> (u64, u64) {
        let pts = self.next_pts_ns;
        let numerator = samples as u64 * 1_000_000_000 + self.remainder;
        let duration = numerator / sample_rate as u64;
        self.remainder = numerator % sample_rate as u64;
        self.next_pts_ns = self.next_pts_ns.saturating_add(duration);
        (pts, duration)
    }
}

pub struct PipelineSession {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    opusenc: gst::Element,
    feeder_stop: Arc<AtomicBool>,
    feeder: Option<JoinHandle<()>>,
    pub rtp_offsets: RtpOffsets,
    pub webrtc_dsp_enabled: bool,
}

impl PipelineSession {
    pub fn start(
        config: &SessionConfig,
        input: SharedInput,
        muted: Arc<AtomicBool>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, String> {
        config.validate()?;
        gst::init().map_err(|error| format!("failed initializing GStreamer: {error}"))?;
        let offsets = RtpOffsets::from_config(config);
        let (pipeline, appsrc, opusenc, webrtc_dsp_enabled) =
            build_pipeline(config, offsets, metrics.clone())?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| format!("failed starting GStreamer pipeline: {error:?}"))?;

        let feeder_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = feeder_stop.clone();
        let thread_appsrc = appsrc.clone();
        let frame_ms = config.frame_ms;
        let feeder = thread::Builder::new()
            .name("noland-mic-appsrc".to_string())
            .spawn(move || feed_appsrc(thread_appsrc, input, muted, metrics, thread_stop, frame_ms))
            .map_err(|error| format!("failed spawning appsrc feeder: {error}"))?;

        Ok(Self {
            pipeline,
            appsrc,
            opusenc,
            feeder_stop,
            feeder: Some(feeder),
            rtp_offsets: offsets,
            webrtc_dsp_enabled,
        })
    }

    pub fn set_bitrate(&self, bitrate: u32) -> Result<(), String> {
        if !(6_000..=128_000).contains(&bitrate) {
            return Err("bitrate must be between 6000 and 128000 bits/s".to_string());
        }
        self.opusenc.set_property("bitrate", bitrate as i32);
        Ok(())
    }

    pub fn poll_error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        while let Some(message) = bus.timed_pop(gst::ClockTime::ZERO) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    return Some(format!(
                        "GStreamer error from {}: {} ({})",
                        error
                            .src()
                            .map(|source| source.path_string().to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                        error.error(),
                        error.debug().unwrap_or_default()
                    ));
                }
                gst::MessageView::Eos(..) => {
                    return Some("GStreamer pipeline reached unexpected EOS".to_string())
                }
                _ => {}
            }
        }
        None
    }

    pub fn stop(&mut self) {
        self.feeder_stop.store(true, Ordering::Release);
        if let Some(feeder) = self.feeder.take() {
            let _ = feeder.join();
        }
        let _ = self.appsrc.end_of_stream();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for PipelineSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn build_pipeline(
    config: &SessionConfig,
    offsets: RtpOffsets,
    metrics: Arc<Metrics>,
) -> Result<(gst::Pipeline, gst_app::AppSrc, gst::Element, bool), String> {
    let pipeline = gst::Pipeline::new();
    let appsrc_element = make_element("appsrc", Some("audio_source"))?;
    let appsrc = appsrc_element
        .clone()
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| "GStreamer appsrc element has an unexpected type".to_string())?;
    let queue = make_element("queue", Some("ingress_queue"))?;
    let convert = make_element("audioconvert", None)?;
    let resample = make_element("audioresample", None)?;
    let normalized = make_element("capsfilter", Some("normalized_audio"))?;
    let opusenc = make_element("opusenc", Some("voice_encoder"))?;
    let pay = make_element("rtpopuspay", Some("opus_payloader"))?;
    let rtpbin = make_element("rtpbin", Some("rtp_session"))?;
    let rtp_sink = make_element("udpsink", Some("rtp_sink"))?;
    let rtcp_sink = make_element("udpsink", Some("rtcp_sink"))?;
    let rtcp_source = make_element("udpsrc", Some("rtcp_source"))?;

    require_property(&appsrc_element, "max-buffers")?;
    require_property(&appsrc_element, "max-time")?;
    require_property(&appsrc_element, "leaky-type")?;
    appsrc_element.set_property("is-live", true);
    appsrc_element.set_property_from_str("format", "time");
    appsrc_element.set_property("do-timestamp", false);
    appsrc_element.set_property("block", false);
    appsrc_element.set_property("max-buffers", APP_QUEUE_BUFFERS);
    appsrc_element.set_property("max-bytes", 0u64);
    appsrc_element.set_property("max-time", APP_QUEUE_TIME_NS);
    appsrc_element.set_property_from_str("leaky-type", "downstream");
    set_appsrc_caps(&appsrc, TARGET_SAMPLE_RATE);

    queue.set_property("max-size-buffers", APP_QUEUE_BUFFERS as u32);
    queue.set_property("max-size-bytes", 0u32);
    queue.set_property("max-size-time", APP_QUEUE_TIME_NS);
    queue.set_property_from_str("leaky", "downstream");

    let normalized_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("rate", TARGET_SAMPLE_RATE as i32)
        .field("channels", 1i32)
        .build();
    normalized.set_property("caps", normalized_caps);

    opusenc.set_property_from_str("audio-type", "voice");
    opusenc.set_property("bitrate", config.bitrate as i32);
    opusenc.set_property_from_str("bitrate-type", "constrained-vbr");
    opusenc.set_property_from_str("frame-size", &config.frame_ms.to_string());
    opusenc.set_property("complexity", OPUS_COMPLEXITY);
    opusenc.set_property("dtx", config.dtx);
    opusenc.set_property("inband-fec", config.fec);
    opusenc.set_property(
        "packet-loss-percentage",
        opus_packet_loss_percentage(config.packet_loss_percent)?,
    );

    pay.set_property("pt", RTP_PAYLOAD_TYPE);
    pay.set_property("mtu", MAX_RTP_PAYLOAD_BYTES);
    pay.set_property("ssrc", offsets.ssrc);
    pay.set_property("seqnum-offset", offsets.sequence as i32);
    pay.set_property("timestamp-offset", offsets.timestamp);

    rtp_sink.set_property("host", config.host.as_str());
    rtp_sink.set_property("port", config.rtp_port as i32);
    rtp_sink.set_property("sync", false);
    rtp_sink.set_property("async", false);

    rtcp_sink.set_property("host", config.host.as_str());
    rtcp_sink.set_property("port", config.resolved_rtcp_port()? as i32);
    rtcp_sink.set_property("sync", false);
    rtcp_sink.set_property("async", false);

    rtcp_source.set_property("port", config.resolved_rtcp_listen_port()? as i32);
    rtcp_source.set_property("caps", gst::Caps::builder("application/x-rtcp").build());

    pipeline
        .add_many([
            &appsrc_element,
            &queue,
            &convert,
            &resample,
            &normalized,
            &opusenc,
            &pay,
            &rtpbin,
            &rtp_sink,
            &rtcp_sink,
            &rtcp_source,
        ])
        .map_err(|error| format!("failed assembling GStreamer pipeline: {error}"))?;

    appsrc_element
        .link(&queue)
        .and_then(|_| queue.link(&convert))
        .and_then(|_| convert.link(&resample))
        .and_then(|_| resample.link(&normalized))
        .map_err(|error| format!("failed linking audio normalization chain: {error}"))?;

    let webrtc_dsp = build_optional_webrtc_dsp()?;
    if let Some(dsp) = &webrtc_dsp {
        pipeline
            .add(dsp)
            .map_err(|error| format!("failed adding webrtcdsp: {error}"))?;
        normalized
            .link(dsp)
            .and_then(|_| dsp.link(&opusenc))
            .map_err(|error| format!("failed linking webrtcdsp: {error}"))?;
    } else {
        normalized
            .link(&opusenc)
            .map_err(|error| format!("failed linking normalized audio to Opus: {error}"))?;
    }
    opusenc
        .link(&pay)
        .map_err(|error| format!("failed linking Opus payloader: {error}"))?;
    attach_rtp_metrics_probe(&pay, metrics)?;

    link_to_request_pad(&pay, "src", &rtpbin, "send_rtp_sink_0")?;
    link_from_named_pad(&rtpbin, "send_rtp_src_0", &rtp_sink, "sink")?;
    link_from_request_pad(&rtpbin, "send_rtcp_src_0", &rtcp_sink, "sink")?;
    link_to_request_pad(&rtcp_source, "src", &rtpbin, "recv_rtcp_sink_0")?;

    // RTCP intentionally uses separate UDP ports. RTP/RTCP mux is deferred:
    // rtpbin's separate-pad model is predictable across GStreamer platforms,
    // while mux/demux requires additional session negotiation and plumbing.
    Ok((pipeline, appsrc, opusenc, webrtc_dsp.is_some()))
}

fn attach_rtp_metrics_probe(payloader: &gst::Element, metrics: Arc<Metrics>) -> Result<(), String> {
    let pad = payloader
        .static_pad("src")
        .ok_or_else(|| "Opus RTP payloader is missing its src pad".to_string())?;
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(buffer) = info.buffer() {
            if let Ok(map) = buffer.map_readable() {
                let packet = map.as_slice();
                if packet.len() >= 4 {
                    let sequence = u16::from_be_bytes([packet[2], packet[3]]);
                    metrics.record_rtp_packet(packet.len(), sequence);
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

fn make_element(factory: &str, name: Option<&str>) -> Result<gst::Element, String> {
    let mut builder = gst::ElementFactory::make(factory);
    if let Some(name) = name {
        builder = builder.name(name);
    }
    builder
        .build()
        .map_err(|_| format!("required GStreamer element '{factory}' is unavailable"))
}

fn build_optional_webrtc_dsp() -> Result<Option<gst::Element>, String> {
    let enabled = std::env::var("NOLAND_ENABLE_WEBRTC_DSP")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }
    if gst::ElementFactory::find("webrtcdsp").is_none() {
        return Ok(None);
    }
    let dsp = make_element("webrtcdsp", Some("voice_dsp"))?;
    set_bool_property_if_present(&dsp, "noise-suppression", true);
    set_bool_property_if_present(&dsp, "gain-control", true);
    set_bool_property_if_present(&dsp, "high-pass-filter", true);
    set_bool_property_if_present(&dsp, "echo-cancel", false);
    Ok(Some(dsp))
}

fn require_property(element: &gst::Element, property: &str) -> Result<(), String> {
    if element.find_property(property).is_some() {
        Ok(())
    } else {
        Err(format!(
            "GStreamer appsrc property '{property}' is unavailable; GStreamer 1.20 or newer is required for bounded downstream-leaky operation"
        ))
    }
}

fn set_bool_property_if_present(element: &gst::Element, property: &str, value: bool) {
    if element.find_property(property).is_some() {
        element.set_property(property, value);
    }
}

fn opus_packet_loss_percentage(value: u32) -> Result<i32, String> {
    if value > 100 {
        return Err("packetLossPercent must be between 0 and 100".to_string());
    }
    i32::try_from(value).map_err(|_| "packetLossPercent exceeds GStreamer gint range".to_string())
}

fn link_to_request_pad(
    source: &gst::Element,
    source_pad_name: &str,
    destination: &gst::Element,
    destination_template: &str,
) -> Result<(), String> {
    let source_pad = source
        .static_pad(source_pad_name)
        .ok_or_else(|| format!("missing pad {source_pad_name} on {}", source.name()))?;
    let destination_pad = destination
        .request_pad_simple(destination_template)
        .ok_or_else(|| {
            format!(
                "failed requesting pad {destination_template} on {}",
                destination.name()
            )
        })?;
    source_pad
        .link(&destination_pad)
        .map(|_| ())
        .map_err(|error| format!("failed linking to {destination_template}: {error:?}"))
}

fn link_from_named_pad(
    source: &gst::Element,
    source_pad_name: &str,
    destination: &gst::Element,
    destination_pad_name: &str,
) -> Result<(), String> {
    let source_pad = source
        .static_pad(source_pad_name)
        .ok_or_else(|| format!("missing pad {source_pad_name} on {}", source.name()))?;
    let destination_pad = destination
        .static_pad(destination_pad_name)
        .ok_or_else(|| {
            format!(
                "missing pad {destination_pad_name} on {}",
                destination.name()
            )
        })?;
    source_pad
        .link(&destination_pad)
        .map(|_| ())
        .map_err(|error| format!("failed linking {source_pad_name}: {error:?}"))
}

fn link_from_request_pad(
    source: &gst::Element,
    source_template: &str,
    destination: &gst::Element,
    destination_pad_name: &str,
) -> Result<(), String> {
    let source_pad = source.request_pad_simple(source_template).ok_or_else(|| {
        format!(
            "failed requesting pad {source_template} on {}",
            source.name()
        )
    })?;
    let destination_pad = destination
        .static_pad(destination_pad_name)
        .ok_or_else(|| {
            format!(
                "missing pad {destination_pad_name} on {}",
                destination.name()
            )
        })?;
    source_pad
        .link(&destination_pad)
        .map(|_| ())
        .map_err(|error| format!("failed linking {source_template}: {error:?}"))
}

fn set_appsrc_caps(appsrc: &gst_app::AppSrc, sample_rate: u32) {
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("rate", sample_rate as i32)
        .field("channels", 1i32)
        .build();
    appsrc.set_caps(Some(&caps));
}

fn feed_appsrc(
    appsrc: gst_app::AppSrc,
    input: SharedInput,
    muted: Arc<AtomicBool>,
    metrics: Arc<Metrics>,
    stop: Arc<AtomicBool>,
    frame_ms: u32,
) {
    let mut generation = u64::MAX;
    let mut frame = Vec::<i16>::new();
    let mut clock = TimestampClock::default();
    let mut next_deadline = Instant::now();
    let mut primed = false;

    while !stop.load(Ordering::Acquire) {
        let current = input.load_full();
        if current.generation != generation {
            generation = current.generation;
            set_appsrc_caps(&appsrc, current.sample_rate);
            frame.resize(frame_samples(current.sample_rate, frame_ms), 0);
            next_deadline = Instant::now();
            primed = false;
        }
        frame.fill(0);

        let is_muted = muted.load(Ordering::Acquire);
        if !is_muted && !primed {
            let startup_target =
                frame_samples(current.sample_rate, STARTUP_PREBUFFER_MS).max(frame.len());
            if !wait_for_ring_samples(
                &current,
                startup_target,
                &stop,
                Duration::from_millis(u64::from(STARTUP_PREBUFFER_MS)),
            ) {
                thread::sleep(Duration::from_millis(WAIT_SLICE_MS));
                continue;
            }
            primed = true;
            next_deadline = Instant::now();
        }

        let consumed = if is_muted {
            current.ring.clear();
            primed = false;
            0
        } else {
            let _ = wait_for_ring_samples(
                &current,
                frame.len(),
                &stop,
                Duration::from_millis(UNDERRUN_GRACE_MS),
            );
            current.ring.pop_slice(&mut frame)
        };
        let silence = frame.len().saturating_sub(consumed);
        metrics.record_output(consumed as u64, silence as u64, !is_muted && silence > 0);
        metrics.set_ring_depth(current.ring.len());
        metrics.set_appsrc_queue_ns(appsrc.property::<u64>("current-level-time"));

        let (pts, duration) = clock.next(frame.len(), current.sample_rate);
        let mut buffer = match gst::Buffer::with_size(frame.len() * 2) {
            Ok(buffer) => buffer,
            Err(_) => break,
        };
        {
            let buffer_ref = buffer.get_mut().expect("new buffer must be writable");
            buffer_ref.set_pts(gst::ClockTime::from_nseconds(pts));
            buffer_ref.set_duration(gst::ClockTime::from_nseconds(duration));
            let Ok(mut mapped) = buffer_ref.map_writable() else {
                break;
            };
            for (bytes, sample) in mapped.as_mut_slice().chunks_exact_mut(2).zip(&frame) {
                bytes.copy_from_slice(&sample.to_le_bytes());
            }
        }
        if appsrc.push_buffer(buffer).is_err() {
            break;
        }

        next_deadline += Duration::from_millis(frame_ms as u64);
        if let Some(delay) = next_deadline.checked_duration_since(Instant::now()) {
            thread::sleep(delay);
        } else {
            next_deadline = Instant::now();
        }
    }
}

fn wait_for_ring_samples(
    input: &crate::capture::AudioInput,
    minimum_samples: usize,
    stop: &AtomicBool,
    timeout: Duration,
) -> bool {
    if input.ring.len() >= minimum_samples {
        return true;
    }
    let deadline = Instant::now() + timeout;
    while !stop.load(Ordering::Acquire) {
        if input.ring.len() >= minimum_samples {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline.saturating_duration_since(now);
        thread::sleep(remaining.min(Duration::from_millis(WAIT_SLICE_MS)));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_progression_is_monotonic_and_exact_for_ten_ms() {
        let mut clock = TimestampClock::default();
        let first = clock.next(480, 48_000);
        let second = clock.next(480, 48_000);
        let third = clock.next(441, 44_100);
        assert_eq!(first, (0, 10_000_000));
        assert_eq!(second, (10_000_000, 10_000_000));
        assert_eq!(third, (20_000_000, 10_000_000));
    }

    #[test]
    fn rtp_offsets_progress_with_wraparound() {
        let offsets = RtpOffsets {
            ssrc: 7,
            sequence: u16::MAX,
            timestamp: u32::MAX - 100,
        };
        let next = offsets.advanced(2, 480);
        assert_eq!(next.ssrc, 7);
        assert_eq!(next.sequence, 1);
        assert_eq!(next.timestamp, 379);
    }

    #[test]
    fn payload_constraints_are_production_defaults() {
        assert_eq!(RTP_PAYLOAD_TYPE, 111);
        assert!(MAX_RTP_PAYLOAD_BYTES <= 1_200);
    }

    #[test]
    fn opus_packet_loss_is_a_bounded_signed_gstreamer_value() {
        assert_eq!(opus_packet_loss_percentage(0), Ok(0i32));
        assert_eq!(opus_packet_loss_percentage(100), Ok(100i32));
        assert!(opus_packet_loss_percentage(101).is_err());
    }
}
