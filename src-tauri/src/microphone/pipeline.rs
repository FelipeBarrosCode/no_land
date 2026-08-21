use crate::microphone::types::MicrophoneError;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

fn resolve_gstreamer_binary() -> Result<PathBuf, MicrophoneError> {
    let Ok(current_exe) = std::env::current_exe() else {
        return Err(MicrophoneError::Internal("No current exe".to_string()));
    };

    if cfg!(target_os = "macos") {
        // Resolve GStreamer framework using existing candidate logic
        // We can just use the environment to find it, or simply use `gst-launch-1.0`
        // if we assume it's in PATH. To be safe, let's try to locate it near current_exe
        let candidates = vec![
            std::env::current_dir()
                .unwrap_or_default()
                .join("src-tauri")
                .join("bundled")
                .join("macos")
                .join("GStreamer.framework"),
            current_exe
                .parent()
                .unwrap_or(Path::new(""))
                .join("..")
                .join("Resources")
                .join("gstreamer")
                .join("macos")
                .join("GStreamer.framework"),
            current_exe
                .parent()
                .unwrap_or(Path::new(""))
                .join("..")
                .join("Frameworks")
                .join("GStreamer.framework"),
        ];

        for candidate in candidates {
            let bin = candidate.join("bin").join("gst-launch-1.0");
            if bin.is_file() {
                return Ok(bin);
            }
            let bin2 = candidate
                .join("Versions")
                .join("Current")
                .join("bin")
                .join("gst-launch-1.0");
            if bin2.is_file() {
                return Ok(bin2);
            }
        }

        // Fallback
        return Ok(PathBuf::from("gst-launch-1.0"));
    }

    if cfg!(target_os = "windows") {
        // In windows, configure_windows_gstreamer_command prepends root/bin to PATH
        // so it should be found
        return Ok(PathBuf::from("gst-launch-1.0.exe"));
    }

    if cfg!(target_os = "linux") {
        // Assume it's in PATH or handled by configure_linux_gstreamer_command
        return Ok(PathBuf::from("gst-launch-1.0"));
    }

    Ok(PathBuf::from("gst-launch-1.0"))
}

pub fn spawn_gstreamer_pipeline(
    sample_rate: u32,
    channels: u16,
    remote_host: &str,
    remote_port: u16,
    ssrc: Option<u32>,
    sequence_offset: Option<u16>,
    timestamp_offset: Option<u32>,
    remote_rtcp_port: Option<u16>,
    local_rtcp_port: Option<u16>,
) -> Result<Child, MicrophoneError> {
    let mut command = Command::new(resolve_gstreamer_binary()?);

    if let Ok(current_exe) = std::env::current_exe() {
        crate::mic_client::runtime::configure_gstreamer_command(&mut command, &current_exe);
    }

    let mut args = vec!["rtpbin".to_string(), "name=rtpbin".to_string()];
    args.extend(vec![
        "fdsrc".to_string(),
        "fd=0".to_string(),
        "do-timestamp=true".to_string(),
        "!".to_string(),
        format!(
            "audio/x-raw,format=F32LE,rate={},channels={},layout=interleaved",
            sample_rate, channels
        ),
        "!".to_string(),
        "audioconvert".to_string(),
        "!".to_string(),
        "audioresample".to_string(),
        "!".to_string(),
        "audio/x-raw,format=S16LE,rate=48000,channels=1".to_string(),
        "!".to_string(),
        "opusenc".to_string(),
        "bitrate=96000".to_string(),
        "frame-size=10".to_string(),
        "!".to_string(),
        "rtpopuspay".to_string(),
        "pt=111".to_string(),
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
        args.push("application/x-rtcp".to_string());
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

    command.args(args);

    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    command
        .spawn()
        .map_err(|e| MicrophoneError::GStreamerSpawnFailed(e.to_string()))
}
