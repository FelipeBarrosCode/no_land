use std::collections::HashSet;
use std::process;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use gst::prelude::*;
use gstreamer as gst;
use serde::{Deserialize, Serialize};

const SAMPLE_RATE: i32 = 48_000;
const CHANNELS: i32 = 1;
const COMPLEXITY: i32 = 5;
const RTP_PAYLOAD_TYPE: i32 = 96;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MicrophoneDevice {
    id: String,
    name: String,
    is_default: bool,
    sample_rates: Vec<u32>,
    channels: u8,
}

#[derive(Debug, Clone)]
struct StreamArgs {
    host: String,
    port: u16,
    device_id: Option<String>,
    bitrate_kbps: u32,
    frame_ms: u32,
}

fn main() {
    match run() {
        Ok(()) => process::exit(0),
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().map(String::as_str) else {
        return Err("Missing microphone sender sidecar command".to_string());
    };

    match command {
        "list-devices" => {
            let devices = list_devices()?;
            if args.iter().any(|arg| arg == "--json") {
                let payload = serde_json::to_string(&devices)
                    .map_err(|error| format!("Failed serializing device list: {error}"))?;
                println!("{payload}");
            } else {
                for device in devices {
                    println!("{}\t{}", device.id, device.name);
                }
            }
            Ok(())
        }
        "stream" => run_stream(parse_stream_args(&args[1..])?),
        other => Err(format!(
            "Unsupported microphone sender sidecar command '{other}'"
        )),
    }
}

fn parse_stream_args(args: &[String]) -> Result<StreamArgs, String> {
    let host = required_arg(args, "--host")?;
    let port = required_arg(args, "--port")?
        .parse::<u16>()
        .map_err(|error| format!("Invalid --port value: {error}"))?;
    let bitrate_kbps = required_arg(args, "--bitrate-kbps")?
        .parse::<u32>()
        .map_err(|error| format!("Invalid --bitrate-kbps value: {error}"))?;
    let frame_ms = required_arg(args, "--frame-ms")?
        .parse::<u32>()
        .map_err(|error| format!("Invalid --frame-ms value: {error}"))?;
    let device_id = optional_arg(args, "--device-id");

    Ok(StreamArgs {
        host,
        port,
        device_id,
        bitrate_kbps,
        frame_ms,
    })
}

fn required_arg(args: &[String], flag: &str) -> Result<String, String> {
    optional_arg(args, flag).ok_or_else(|| format!("Missing required argument {flag}"))
}

fn optional_arg(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn list_devices() -> Result<Vec<MicrophoneDevice>, String> {
    gst::init().map_err(|error| format!("Failed initializing GStreamer: {error}"))?;

    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::builder("audio/x-raw").build();
    monitor.add_filter(Some("Audio/Source"), Some(&caps));
    monitor
        .start()
        .map_err(|error| format!("Failed starting device monitor: {error}"))?;

    let mut devices = vec![MicrophoneDevice {
        id: "default".to_string(),
        name: "System Default".to_string(),
        is_default: true,
        sample_rates: vec![SAMPLE_RATE as u32],
        channels: CHANNELS as u8,
    }];

    let mut seen_names = HashSet::new();
    for device in monitor.devices() {
        let display_name = device.display_name().to_string();
        if display_name.trim().is_empty() || !seen_names.insert(display_name.clone()) {
            continue;
        }

        devices.push(MicrophoneDevice {
            id: display_name.clone(),
            name: display_name,
            is_default: false,
            sample_rates: vec![SAMPLE_RATE as u32],
            channels: CHANNELS as u8,
        });
    }

    monitor.stop();
    Ok(devices)
}

fn run_stream(args: StreamArgs) -> Result<(), String> {
    gst::init().map_err(|error| format!("Failed initializing GStreamer: {error}"))?;

    let pipeline = gst::Pipeline::new();
    let source = build_source(args.device_id.as_deref())?;
    let queue = make_element("queue")?;
    let convert = make_element("audioconvert")?;
    let resample = make_element("audioresample")?;
    let capsfilter = make_element("capsfilter")?;
    let opusenc = make_element("opusenc")?;
    let pay = make_element("rtpopuspay")?;
    let sink = make_element("udpsink")?;

    queue.set_property("max-size-buffers", 2u32);
    queue.set_property_from_str("leaky", "downstream");

    if cfg!(target_os = "macos") {
        convert.set_property_from_str("input-channels-reorder-mode", "unpositioned");
        convert.set_property_from_str("input-channels-reorder", "mono");
    }

    let raw_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("rate", SAMPLE_RATE)
        .field("channels", CHANNELS)
        .build();
    capsfilter.set_property("caps", raw_caps);

    opusenc.set_property_from_str("audio-type", "voice");
    opusenc.set_property("bitrate", (args.bitrate_kbps * 1000) as i32);
    opusenc.set_property_from_str("bitrate-type", "constrained-vbr");
    opusenc.set_property_from_str("frame-size", &args.frame_ms.to_string());
    opusenc.set_property("complexity", COMPLEXITY);
    opusenc.set_property("dtx", false);
    opusenc.set_property("inband-fec", false);

    pay.set_property("pt", RTP_PAYLOAD_TYPE as u32);

    sink.set_property("host", args.host.as_str());
    sink.set_property("port", args.port as i32);
    sink.set_property("sync", false);
    sink.set_property("async", false);

    pipeline
        .add_many([
            &source,
            &queue,
            &convert,
            &resample,
            &capsfilter,
            &opusenc,
            &pay,
            &sink,
        ])
        .map_err(|error| format!("Failed assembling microphone sender pipeline: {error}"))?;
    gst::Element::link_many([
        &source,
        &queue,
        &convert,
        &resample,
        &capsfilter,
        &opusenc,
        &pay,
        &sink,
    ])
    .map_err(|error| format!("Failed linking microphone sender pipeline: {error}"))?;

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|error| format!("Failed starting microphone sender pipeline: {error:?}"))?;

    let running = Arc::new(AtomicBool::new(true));
    let signal_flag = running.clone();
    ctrlc::set_handler(move || {
        signal_flag.store(false, Ordering::SeqCst);
    })
    .map_err(|error| format!("Failed installing microphone sender signal handler: {error}"))?;

    let bus = pipeline
        .bus()
        .ok_or_else(|| "Microphone sender pipeline bus is unavailable".to_string())?;

    while running.load(Ordering::SeqCst) {
        if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(250)) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    let debug = error.debug().unwrap_or_default();
                    let _ = pipeline.set_state(gst::State::Null);
                    return Err(format!(
                        "Microphone sender pipeline error from {}: {} ({debug})",
                        error
                            .src()
                            .map(|src| src.path_string())
                            .unwrap_or_else(|| "unknown".to_string().into()),
                        error.error()
                    ));
                }
                gst::MessageView::Eos(..) => break,
                _ => {}
            }
        }
    }

    let _ = pipeline.send_event(gst::event::Eos::new());
    let _ = pipeline.set_state(gst::State::Null);
    Ok(())
}

fn build_source(device_id: Option<&str>) -> Result<gst::Element, String> {
    let source = if let Some(requested) = device_id.map(str::trim).filter(|value| !value.is_empty())
    {
        create_monitored_device_source(requested)?
    } else {
        create_default_source()?
    };

    if source.find_property("do-timestamp").is_some() {
        source.set_property("do-timestamp", true);
    }
    if source.find_property("low-latency").is_some() {
        source.set_property("low-latency", true);
    }

    Ok(source)
}

fn create_monitored_device_source(requested: &str) -> Result<gst::Element, String> {
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::builder("audio/x-raw").build();
    monitor.add_filter(Some("Audio/Source"), Some(&caps));
    monitor
        .start()
        .map_err(|error| format!("Failed starting device monitor: {error}"))?;

    let maybe_device = monitor.devices().into_iter().find(|device| {
        let display_name = device.display_name().to_string();
        display_name == requested
    });

    let device = maybe_device.ok_or_else(|| {
        format!(
            "Requested microphone device '{}' is no longer available via GStreamer",
            requested
        )
    })?;

    let element = device.create_element(None).map_err(|error| {
        format!(
            "Failed creating source element for '{}': {error}",
            requested
        )
    })?;
    monitor.stop();
    Ok(element)
}

fn create_default_source() -> Result<gst::Element, String> {
    let factory_name = if cfg!(target_os = "macos") {
        "osxaudiosrc"
    } else if cfg!(target_os = "windows") {
        "wasapisrc"
    } else {
        "pipewiresrc"
    };

    make_element(factory_name)
}

fn make_element(factory_name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory_name)
        .build()
        .map_err(|_| {
            format!(
                "Required GStreamer element '{}' is not available",
                factory_name
            )
        })
}
