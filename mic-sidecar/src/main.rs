#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
use std::collections::HashSet;
use std::process;
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
use gst::prelude::*;
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
use gstreamer as gst;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
use std::{
    ffi::{c_void, CStr},
    mem::{size_of, MaybeUninit},
    os::raw::c_char,
    ptr, slice,
};

#[cfg(target_os = "macos")]
use core_foundation_sys::string::{
    kCFStringEncodingUTF8, CFStringGetCString, CFStringGetLength,
    CFStringGetMaximumSizeForEncoding, CFStringRef,
};
#[cfg(target_os = "macos")]
use coreaudio_sys::{
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyStreamConfiguration,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectSystemObject, AudioBufferList, AudioDeviceID,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress,
};

const SAMPLE_RATE: i32 = 48_000;
const CHANNELS: i32 = 1;
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
const COMPLEXITY: i32 = 5;
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
const RTP_PAYLOAD_TYPE: i32 = 96;

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
const WINDOWS_ARM64_UNSUPPORTED_MESSAGE: &str = "Microphone passthrough is not yet supported on Windows ARM64 because the required GStreamer SDK is not published for that target.";

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

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct MacosAudioInputDevice {
    name: String,
    uid: String,
    is_default: bool,
    channels: u32,
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

#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
fn list_devices() -> Result<Vec<MicrophoneDevice>, String> {
    gst::init().map_err(|error| format!("Failed initializing GStreamer: {error}"))?;

    #[cfg(target_os = "macos")]
    {
        let devices_in_coreaudio = list_macos_audio_input_devices()?;
        let mut devices = vec![MicrophoneDevice {
            id: "default".to_string(),
            name: "System Default".to_string(),
            is_default: true,
            sample_rates: vec![SAMPLE_RATE as u32],
            channels: CHANNELS as u8,
        }];

        let mut seen_names = HashSet::new();
        for device in devices_in_coreaudio {
            if device.name.trim().is_empty() || !seen_names.insert(device.name.clone()) {
                continue;
            }

            devices.push(MicrophoneDevice {
                id: device.name.clone(),
                name: device.name,
                is_default: device.is_default,
                sample_rates: vec![SAMPLE_RATE as u32],
                channels: device.channels.clamp(1, u8::MAX as u32) as u8,
            });
        }

        return Ok(devices);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let devices_in_monitor = list_audio_source_devices()?;
        let mut devices = vec![MicrophoneDevice {
            id: "default".to_string(),
            name: "System Default".to_string(),
            is_default: true,
            sample_rates: vec![SAMPLE_RATE as u32],
            channels: CHANNELS as u8,
        }];

        let mut seen_names = HashSet::new();
        for device in devices_in_monitor {
            let display_name = device.display_name().to_string();
            if display_name.trim().is_empty() || !seen_names.insert(display_name.clone()) {
                continue;
            }

            devices.push(MicrophoneDevice {
                id: display_name.clone(),
                name: display_name,
                is_default: device_is_default(&device),
                sample_rates: vec![SAMPLE_RATE as u32],
                channels: CHANNELS as u8,
            });
        }

        Ok(devices)
    }
}

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
fn list_devices() -> Result<Vec<MicrophoneDevice>, String> {
    Ok(Vec::new())
}

#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
fn run_stream(args: StreamArgs) -> Result<(), String> {
    gst::init().map_err(|error| format!("Failed initializing GStreamer: {error}"))?;

    let pipeline = build_pipeline(&args)?;

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

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
fn run_stream(_args: StreamArgs) -> Result<(), String> {
    Err(WINDOWS_ARM64_UNSUPPORTED_MESSAGE.to_string())
}

#[cfg(all(
    target_os = "macos",
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
fn build_pipeline(args: &StreamArgs) -> Result<gst::Pipeline, String> {
    let resolved_device = resolve_macos_stream_device(args.device_id.as_deref())?;
    eprintln!(
        "Using macOS microphone '{}' with CoreAudio UID '{}'",
        resolved_device.name, resolved_device.uid
    );

    let description = format!(
        concat!(
            "osxaudiosrc unique-id=\"{}\" ",
            "! queue max-size-buffers=2 leaky=downstream ",
            "! audioconvert input-channels-reorder-mode=unpositioned input-channels-reorder=mono ",
            "! audioresample ",
            "! audio/x-raw,format=S16LE,rate={},channels={} ",
            "! opusenc audio-type=voice bitrate={} bitrate-type=constrained-vbr frame-size={} complexity={} dtx=false inband-fec=false ",
            "! rtpopuspay pt={} ",
            "! udpsink host=\"{}\" port={} sync=false async=false"
        ),
        gst_string_literal(&resolved_device.uid),
        SAMPLE_RATE,
        CHANNELS,
        (args.bitrate_kbps * 1000),
        args.frame_ms,
        COMPLEXITY,
        RTP_PAYLOAD_TYPE,
        gst_string_literal(&args.host),
        args.port
    );

    let element = gst::parse::launch(&description)
        .map_err(|error| format!("Failed building macOS microphone pipeline: {error}"))?;

    element.downcast::<gst::Pipeline>().map_err(|element| {
        format!(
            "macOS microphone pipeline description did not produce a pipeline (got '{}')",
            element.type_().name()
        )
    })
}

#[cfg(all(
    not(target_os = "macos"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
fn build_pipeline(args: &StreamArgs) -> Result<gst::Pipeline, String> {
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

    Ok(pipeline)
}

#[cfg(all(
    not(target_os = "macos"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
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

#[cfg(all(
    not(target_os = "macos"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
fn create_monitored_device_source(requested: &str) -> Result<gst::Element, String> {
    let device = list_audio_source_devices()?
        .into_iter()
        .find(|device| device.display_name().as_str() == requested)
        .ok_or_else(|| {
            format!(
                "Requested microphone device '{}' is no longer available via GStreamer",
                requested
            )
        })?;

    create_source_for_device(&device).map_err(|error| {
        format!(
            "Failed creating source element for '{}': {error}",
            requested
        )
    })
}

#[cfg(all(
    not(target_os = "macos"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
fn create_default_source() -> Result<gst::Element, String> {
    let factory_name = if cfg!(target_os = "windows") {
        "wasapisrc"
    } else {
        "pipewiresrc"
    };

    make_element(factory_name)
}

#[cfg(all(
    not(target_os = "macos"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
fn list_audio_source_devices() -> Result<Vec<gst::Device>, String> {
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::builder("audio/x-raw").build();
    monitor.add_filter(Some("Audio/Source"), Some(&caps));
    monitor
        .start()
        .map_err(|error| format!("Failed starting device monitor: {error}"))?;
    let devices = monitor.devices().into_iter().collect::<Vec<_>>();
    monitor.stop();
    Ok(devices)
}

#[cfg(all(
    not(target_os = "macos"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
fn create_source_for_device(device: &gst::Device) -> Result<gst::Element, String> {
    device.create_element(None).map_err(|error| {
        format!(
            "Failed to create source element for '{}': {error}",
            device.display_name()
        )
    })
}

#[cfg(all(
    not(target_os = "macos"),
    not(all(target_os = "windows", target_arch = "aarch64"))
))]
fn device_is_default(device: &gst::Device) -> bool {
    device
        .properties()
        .and_then(|properties| properties.get_optional::<bool>("is-default").ok().flatten())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
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

#[cfg(target_os = "macos")]
fn gst_string_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
fn resolve_macos_stream_device(requested: Option<&str>) -> Result<MacosAudioInputDevice, String> {
    let devices = list_macos_audio_input_devices()?;
    if devices.is_empty() {
        return Err("No macOS audio input devices are available".to_string());
    }

    let requested = requested
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "default");

    match requested {
        Some(requested) => devices
            .iter()
            .find(|device| device.name == requested || device.uid == requested)
            .cloned()
            .ok_or_else(|| {
                let available = devices
                    .iter()
                    .map(|device| format!("{} [{}]", device.name, device.uid))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "Requested macOS microphone '{}' was not found. Available devices: {}",
                    requested, available
                )
            }),
        None => devices
            .iter()
            .find(|device| device.is_default)
            .cloned()
            .or_else(|| devices.first().cloned())
            .ok_or_else(|| "No macOS default audio input device is available".to_string()),
    }
}

#[cfg(target_os = "macos")]
fn list_macos_audio_input_devices() -> Result<Vec<MacosAudioInputDevice>, String> {
    let device_ids = list_macos_device_ids()?;
    let default_device_id = get_macos_default_input_device_id()?;
    let mut devices = Vec::new();

    for device_id in device_ids {
        let channels = get_macos_input_channel_count(device_id)?;
        if channels == 0 {
            continue;
        }

        let name = get_macos_device_name(device_id)?;
        let uid = get_macos_device_uid(device_id)?;
        if name.trim().is_empty() || uid.trim().is_empty() {
            continue;
        }

        devices.push(MacosAudioInputDevice {
            name,
            uid,
            is_default: device_id == default_device_id,
            channels,
        });
    }

    Ok(devices)
}

#[cfg(target_os = "macos")]
fn list_macos_device_ids() -> Result<Vec<AudioDeviceID>, String> {
    let address = macos_property_address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    );
    let size = get_macos_property_data_size(kAudioObjectSystemObject, &address)? as usize;
    let count = size / size_of::<AudioDeviceID>();
    let mut devices = vec![0 as AudioDeviceID; count];
    get_macos_property_data_into(
        kAudioObjectSystemObject,
        &address,
        (devices.len() * size_of::<AudioDeviceID>()) as u32,
        devices.as_mut_ptr().cast::<c_void>(),
    )?;
    Ok(devices)
}

#[cfg(target_os = "macos")]
fn get_macos_default_input_device_id() -> Result<AudioDeviceID, String> {
    let address = macos_property_address(
        kAudioHardwarePropertyDefaultInputDevice,
        kAudioObjectPropertyScopeGlobal,
    );
    get_macos_scalar_property(kAudioObjectSystemObject, &address)
}

#[cfg(target_os = "macos")]
fn get_macos_input_channel_count(device_id: AudioDeviceID) -> Result<u32, String> {
    let address = macos_property_address(
        kAudioDevicePropertyStreamConfiguration,
        kAudioObjectPropertyScopeInput,
    );
    let size = get_macos_property_data_size(device_id, &address)? as usize;
    if size < size_of::<AudioBufferList>() {
        return Ok(0);
    }

    let mut storage = vec![0u8; size];
    get_macos_property_data_into(
        device_id,
        &address,
        size as u32,
        storage.as_mut_ptr().cast(),
    )?;

    let buffer_list = unsafe { &*(storage.as_ptr().cast::<AudioBufferList>()) };
    let buffers = unsafe {
        slice::from_raw_parts(
            buffer_list.mBuffers.as_ptr(),
            buffer_list.mNumberBuffers as usize,
        )
    };

    Ok(buffers.iter().map(|buffer| buffer.mNumberChannels).sum())
}

#[cfg(target_os = "macos")]
fn get_macos_device_name(device_id: AudioDeviceID) -> Result<String, String> {
    let address = macos_property_address(kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal);
    get_macos_cfstring_property(device_id, &address)
}

#[cfg(target_os = "macos")]
fn get_macos_device_uid(device_id: AudioDeviceID) -> Result<String, String> {
    let address = macos_property_address(
        kAudioDevicePropertyDeviceUID,
        kAudioObjectPropertyScopeGlobal,
    );
    get_macos_cfstring_property(device_id, &address)
}

#[cfg(target_os = "macos")]
fn macos_property_address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

#[cfg(target_os = "macos")]
fn get_macos_scalar_property<T: Copy>(
    object_id: AudioObjectID,
    address: &AudioObjectPropertyAddress,
) -> Result<T, String> {
    let mut value = MaybeUninit::<T>::uninit();
    get_macos_property_data_into(
        object_id,
        address,
        size_of::<T>() as u32,
        value.as_mut_ptr().cast::<c_void>(),
    )?;
    Ok(unsafe { value.assume_init() })
}

#[cfg(target_os = "macos")]
fn get_macos_cfstring_property(
    object_id: AudioObjectID,
    address: &AudioObjectPropertyAddress,
) -> Result<String, String> {
    let cf_string = get_macos_scalar_property::<CFStringRef>(object_id, address)?;
    if cf_string.is_null() {
        return Err(format!(
            "CoreAudio returned a null CFString for selector '{}' on object {}",
            fourcc(address.mSelector),
            object_id
        ));
    }

    let length = unsafe { CFStringGetLength(cf_string) };
    let capacity = unsafe { CFStringGetMaximumSizeForEncoding(length, kCFStringEncodingUTF8) } + 1;
    let mut buffer = vec![0 as c_char; capacity.max(1) as usize];
    let success = unsafe {
        CFStringGetCString(
            cf_string,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            kCFStringEncodingUTF8,
        )
    };
    if success == 0 {
        return Err(format!(
            "Failed converting CoreAudio CFString property '{}' on object {} to UTF-8",
            fourcc(address.mSelector),
            object_id
        ));
    }

    let value = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Ok(value)
}

#[cfg(target_os = "macos")]
fn get_macos_property_data_size(
    object_id: AudioObjectID,
    address: &AudioObjectPropertyAddress,
) -> Result<u32, String> {
    let mut data_size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(object_id, address, 0, ptr::null(), &mut data_size)
    };
    if status != 0 {
        return Err(format!(
            "CoreAudio failed getting property size '{}' for object {}: {}",
            fourcc(address.mSelector),
            object_id,
            format_os_status(status)
        ));
    }
    Ok(data_size)
}

#[cfg(target_os = "macos")]
fn get_macos_property_data_into(
    object_id: AudioObjectID,
    address: &AudioObjectPropertyAddress,
    mut data_size: u32,
    data: *mut c_void,
) -> Result<(), String> {
    let status = unsafe {
        AudioObjectGetPropertyData(object_id, address, 0, ptr::null(), &mut data_size, data)
    };
    if status != 0 {
        return Err(format!(
            "CoreAudio failed getting property '{}' for object {}: {}",
            fourcc(address.mSelector),
            object_id,
            format_os_status(status)
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn fourcc(value: u32) -> String {
    let bytes = value.to_be_bytes();
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("0x{value:08x}")
    }
}

#[cfg(target_os = "macos")]
fn format_os_status(status: i32) -> String {
    let bytes = (status as u32).to_be_bytes();
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        format!("{} ('{}')", status, String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}
