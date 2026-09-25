import re

with open("src-tauri/src/microphone/pipeline.rs", "r") as f:
    code = f.read()

old_signature = """pub fn spawn_gstreamer_pipeline(
    sample_rate: u32,
    channels: u16,
    remote_host: &str,
    remote_port: u16,
) -> Result<Child, MicrophoneError> {"""

new_signature = """pub fn spawn_gstreamer_pipeline(
    sample_rate: u32,
    channels: u16,
    remote_host: &str,
    remote_port: u16,
    ssrc: Option<u32>,
    sequence_offset: Option<u16>,
    timestamp_offset: Option<u32>,
) -> Result<Child, MicrophoneError> {"""

code = code.replace(old_signature, new_signature)

old_args = """    command.args([
        "fdsrc", "fd=0",
        "!", &format!("audio/x-raw,format=F32LE,rate={},channels={},layout=interleaved", sample_rate, channels),
        "!", "audioconvert",
        "!", "audioresample",
        "!", "audio/x-raw,format=S16LE,rate=48000,channels=1",
        "!", "opusenc", "bitrate=96000", "frame-size=10",
        "!", "rtpopuspay", "pt=96",
        "!", "udpsink", &format!("host={}", remote_host), &format!("port={}", remote_port), "sync=false", "async=false",
    ]);"""

new_args = """    let mut args = vec![
        "fdsrc".to_string(), "fd=0".to_string(),
        "!".to_string(), format!("audio/x-raw,format=F32LE,rate={},channels={},layout=interleaved", sample_rate, channels),
        "!".to_string(), "audioconvert".to_string(),
        "!".to_string(), "audioresample".to_string(),
        "!".to_string(), "audio/x-raw,format=S16LE,rate=48000,channels=1".to_string(),
        "!".to_string(), "opusenc".to_string(), "bitrate=96000".to_string(), "frame-size=10".to_string(),
        "!".to_string(), "rtpopuspay".to_string(), "pt=111".to_string(),
    ];
    if let Some(s) = ssrc {
        args.push(format!("ssrc={}", s));
    }
    if let Some(s) = sequence_offset {
        args.push(format!("seqnum-offset={}", s));
    }
    if let Some(t) = timestamp_offset {
        args.push(format!("timestamp-offset={}", t));
    }
    args.push("!".to_string());
    args.push("udpsink".to_string());
    args.push(format!("host={}", remote_host));
    args.push(format!("port={}", remote_port));
    args.push("sync=false".to_string());
    args.push("async=false".to_string());

    command.args(args);"""

code = code.replace(old_args, new_args)

with open("src-tauri/src/microphone/pipeline.rs", "w") as f:
    f.write(code)

