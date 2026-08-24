import re

with open("src-tauri/src/microphone/pipeline.rs", "r") as f:
    code = f.read()

old_sig = """pub fn spawn_gstreamer_pipeline(
    sample_rate: u32,
    channels: u16,
    remote_host: &str,
    remote_port: u16,
    ssrc: Option<u32>,
    sequence_offset: Option<u16>,
    timestamp_offset: Option<u32>,
) -> Result<Child, MicrophoneError> {"""

new_sig = """pub fn spawn_gstreamer_pipeline(
    sample_rate: u32,
    channels: u16,
    remote_host: &str,
    remote_port: u16,
    ssrc: Option<u32>,
    sequence_offset: Option<u16>,
    timestamp_offset: Option<u32>,
    remote_rtcp_port: Option<u16>,
    local_rtcp_port: Option<u16>,
) -> Result<Child, MicrophoneError> {"""

code = code.replace(old_sig, new_sig)

old_args = """    let mut args = vec![
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

new_args = """    let mut args = vec!["rtpbin".to_string(), "name=rtpbin".to_string()];
    args.extend(vec![
        "fdsrc".to_string(), "fd=0".to_string(),
        "!".to_string(), format!("audio/x-raw,format=F32LE,rate={},channels={},layout=interleaved", sample_rate, channels),
        "!".to_string(), "audioconvert".to_string(),
        "!".to_string(), "audioresample".to_string(),
        "!".to_string(), "audio/x-raw,format=S16LE,rate=48000,channels=1".to_string(),
        "!".to_string(), "opusenc".to_string(), "bitrate=96000".to_string(), "frame-size=10".to_string(),
        "!".to_string(), "rtpopuspay".to_string(), "pt=111".to_string(),
    ]);

    if let Some(s) = ssrc {
        args.push(format!("ssrc={}", s));
    }
    if let Some(s) = sequence_offset {
        args.push(format!("seqnum-offset={}", s));
    }
    if let Some(t) = timestamp_offset {
        args.push(format!("timestamp-offset={}", t));
    }

    if let (Some(remote_rtcp), Some(local_rtcp)) = (remote_rtcp_port, local_rtcp_port) {
        // Link to rtpbin
        args.push("!".to_string());
        args.push("rtpbin.send_rtp_sink_0".to_string());
        
        args.push("rtpbin.send_rtp_src_0".to_string());
        args.push("!".to_string());
        args.push("udpsink".to_string());
        args.push(format!("host={}", remote_host));
        args.push(format!("port={}", remote_port));
        args.push("sync=false".to_string());
        args.push("async=false".to_string());

        args.push("rtpbin.send_rtcp_src_0".to_string());
        args.push("!".to_string());
        args.push("udpsink".to_string());
        args.push(format!("host={}", remote_host));
        args.push(format!("port={}", remote_rtcp));
        args.push("sync=false".to_string());
        args.push("async=false".to_string());

        args.push("udpsrc".to_string());
        args.push(format!("port={}", local_rtcp));
        args.push("!".to_string());
        args.push("rtpbin.recv_rtcp_sink_0".to_string());
    } else {
        // Fallback without RTCP (e.g. for testing or simple streaming)
        args.push("!".to_string());
        args.push("udpsink".to_string());
        args.push(format!("host={}", remote_host));
        args.push(format!("port={}", remote_port));
        args.push("sync=false".to_string());
        args.push("async=false".to_string());
    }

    command.args(args);"""

code = code.replace(old_args, new_args)

with open("src-tauri/src/microphone/pipeline.rs", "w") as f:
    f.write(code)

