mod capture;
mod metrics;
mod pipeline;
mod protocol;
mod ring;
mod state;

use arc_swap::ArcSwap;
use capture::{
    current_default_device_id, device_available, list_devices, AudioInput, CaptureController,
    CaptureSignal, SharedInput, SourceKind,
};
use metrics::Metrics;
use pipeline::PipelineSession;
use protocol::{Command, Output, Request, ResponseResult, SessionConfig};
use serde_json::{json, Value};
use state::{SidecarState, StateMachine, Status};
use std::io::{self, BufRead, Write};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(2);
const CAPTURE_RETRY_INTERVAL: Duration = Duration::from_secs(1);

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        None | Some("daemon") => run_daemon(),
        Some("list-devices") => run_list_devices(&args[1..]),
        Some("stream") => run_compat_stream(&args[1..]),
        Some(other) => Err(format!("unsupported microphone sidecar command '{other}'")),
    }
}

fn run_list_devices(args: &[String]) -> Result<(), String> {
    let devices = list_devices()?;
    if args.iter().any(|arg| arg == "--json") {
        println!(
            "{}",
            serde_json::to_string(&devices)
                .map_err(|error| format!("failed serializing devices: {error}"))?
        );
    } else {
        for device in devices {
            println!("{}\t{}", device.id, device.name);
        }
    }
    Ok(())
}

fn run_daemon() -> Result<(), String> {
    let (input_tx, input_rx) = mpsc::channel();
    spawn_stdin_reader(input_tx);
    let shutdown = install_signal_handler()?;
    let mut daemon = Daemon::new();

    loop {
        if shutdown.load(Ordering::Acquire) {
            daemon.shutdown();
            return Ok(());
        }
        match input_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(InputMessage::Request(request)) => {
                let should_shutdown = matches!(request.command, Command::Shutdown);
                daemon.handle_request(request);
                if should_shutdown {
                    return Ok(());
                }
            }
            Ok(InputMessage::Invalid { id, error }) => emit(Output::error(id, error)),
            Ok(InputMessage::Eof) => {
                daemon.shutdown();
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                daemon.shutdown();
                return Ok(());
            }
        }
        daemon.tick();
    }
}

fn run_compat_stream(args: &[String]) -> Result<(), String> {
    let host = required_arg(args, "--host")?;
    let rtp_port = required_arg(args, "--port")?
        .parse::<u16>()
        .map_err(|error| format!("invalid --port: {error}"))?;
    let bitrate_kbps = optional_arg(args, "--bitrate-kbps")
        .unwrap_or_else(|| "32".to_string())
        .parse::<u32>()
        .map_err(|error| format!("invalid --bitrate-kbps: {error}"))?;
    let frame_ms = optional_arg(args, "--frame-ms")
        .unwrap_or_else(|| "10".to_string())
        .parse::<u32>()
        .map_err(|error| format!("invalid --frame-ms: {error}"))?;
    let selected = optional_arg(args, "--device-id");
    let source = if args.iter().any(|arg| arg == "--test-sine") {
        SourceKind::Sine
    } else {
        SourceKind::Microphone
    };
    let config = SessionConfig {
        session_id: optional_arg(args, "--session-id")
            .unwrap_or_else(|| "compat-stream".to_string()),
        host,
        rtp_port,
        rtcp_port: optional_arg(args, "--rtcp-port").and_then(|value| value.parse().ok()),
        rtcp_listen_port: optional_arg(args, "--rtcp-listen-port")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        bitrate: bitrate_kbps.saturating_mul(1_000),
        frame_ms,
        fec: args.iter().any(|arg| arg == "--fec"),
        packet_loss_percent: optional_arg(args, "--packet-loss-percent")
            .and_then(|value| value.parse().ok())
            .unwrap_or(5),
        dtx: args.iter().any(|arg| arg == "--dtx"),
        ssrc: optional_arg(args, "--ssrc").and_then(|value| value.parse().ok()),
        sequence_offset: optional_arg(args, "--sequence-offset")
            .and_then(|value| value.parse().ok()),
        timestamp_offset: optional_arg(args, "--timestamp-offset")
            .and_then(|value| value.parse().ok()),
        source,
    };
    config.validate()?;

    let shutdown = install_signal_handler()?;
    let mut daemon = Daemon::new();
    daemon.selected_device_id = selected;
    daemon.start_session(config)?;
    while !shutdown.load(Ordering::Acquire) {
        daemon.tick();
        thread::sleep(Duration::from_millis(100));
    }
    daemon.shutdown();
    Ok(())
}

fn required_arg(args: &[String], name: &str) -> Result<String, String> {
    optional_arg(args, name).ok_or_else(|| format!("missing required argument {name}"))
}

fn optional_arg(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn install_signal_handler() -> Result<Arc<AtomicBool>, String> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let signal_shutdown = shutdown.clone();
    ctrlc::set_handler(move || signal_shutdown.store(true, Ordering::Release))
        .map_err(|error| format!("failed installing signal handler: {error}"))?;
    Ok(shutdown)
}

enum InputMessage {
    Request(Request),
    Invalid { id: Value, error: String },
    Eof,
}

fn spawn_stdin_reader(sender: Sender<InputMessage>) {
    thread::Builder::new()
        .name("noland-mic-ipc".to_string())
        .spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => {
                        let _ = sender.send(InputMessage::Invalid {
                            id: Value::Null,
                            error: format!("failed reading stdin: {error}"),
                        });
                        break;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let value = match serde_json::from_str::<Value>(&line) {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = sender.send(InputMessage::Invalid {
                            id: Value::Null,
                            error: format!("invalid JSON request: {error}"),
                        });
                        continue;
                    }
                };
                let id = value.get("id").cloned().unwrap_or(Value::Null);
                match serde_json::from_value::<Request>(value) {
                    Ok(request) => {
                        if sender.send(InputMessage::Request(request)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(InputMessage::Invalid {
                            id,
                            error: format!("invalid command: {error}"),
                        });
                    }
                }
            }
            let _ = sender.send(InputMessage::Eof);
        })
        .expect("stdin reader thread creation must succeed");
}

fn emit(output: Output) {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    if serde_json::to_writer(&mut lock, &output).is_ok() {
        let _ = lock.write_all(b"\n");
        let _ = lock.flush();
    }
}

struct Daemon {
    state: StateMachine,
    metrics: Arc<Metrics>,
    input: SharedInput,
    capture: CaptureController,
    capture_tx: Sender<CaptureSignal>,
    capture_rx: Receiver<CaptureSignal>,
    pipeline: Option<PipelineSession>,
    muted: Arc<AtomicBool>,
    selected_device_id: Option<String>,
    active_device_id: Option<String>,
    session_config: Option<SessionConfig>,
    retry_at: Instant,
    next_device_poll: Instant,
}

impl Daemon {
    fn new() -> Self {
        let (capture_tx, capture_rx) = mpsc::channel();
        Self {
            state: StateMachine::default(),
            metrics: Arc::new(Metrics::default()),
            input: Arc::new(ArcSwap::from(AudioInput::silent())),
            capture: CaptureController::default(),
            capture_tx,
            capture_rx,
            pipeline: None,
            muted: Arc::new(AtomicBool::new(false)),
            selected_device_id: None,
            active_device_id: None,
            session_config: None,
            retry_at: Instant::now(),
            next_device_poll: Instant::now(),
        }
    }

    fn handle_request(&mut self, request: Request) {
        let id = request.id;
        let result = match request.command {
            Command::ListDevices => {
                list_devices().map(|devices| ResponseResult::Devices { devices })
            }
            Command::GetStatus => Ok(ResponseResult::Status(self.status())),
            Command::SelectDevice { device_id } => self
                .select_device(device_id)
                .map(|_| ResponseResult::Status(self.status())),
            Command::StartSession { config } => self
                .start_session(config.clone())
                .map(|_| ResponseResult::SessionConfig(config)),
            Command::StopSession => {
                self.stop_session();
                Ok(ResponseResult::Ack { acknowledged: true })
            }
            Command::Mute => {
                self.muted.store(true, Ordering::Release);
                self.emit_event("muteChanged", json!({ "muted": true }));
                Ok(ResponseResult::Status(self.status()))
            }
            Command::Unmute => {
                self.muted.store(false, Ordering::Release);
                self.emit_event("muteChanged", json!({ "muted": false }));
                Ok(ResponseResult::Status(self.status()))
            }
            Command::SetBitrate { bitrate } => self
                .set_bitrate(bitrate)
                .map(|_| ResponseResult::Ack { acknowledged: true }),
            Command::GetMetrics => Ok(ResponseResult::Metrics(self.metrics.snapshot())),
            Command::Shutdown => {
                self.shutdown();
                Ok(ResponseResult::Ack { acknowledged: true })
            }
        };
        match result {
            Ok(result) => emit(Output::success(id, result)),
            Err(error) => emit(Output::error(id, error)),
        }
    }

    fn status(&self) -> Status {
        Status {
            session_id: self
                .session_config
                .as_ref()
                .map(|config| config.session_id.clone()),
            state: self.state.state(),
            health: self.state.health(),
            muted: self.muted.load(Ordering::Acquire),
            selected_device_id: self.selected_device_id.clone(),
            active_device_id: self.active_device_id.clone(),
            active_sample_rate: self.input.load().sample_rate,
            session_active: self.pipeline.is_some(),
            last_error: self.state.last_error(),
        }
    }

    fn start_session(&mut self, config: SessionConfig) -> Result<(), String> {
        if self.pipeline.is_some() {
            return Err("a session is already active".to_string());
        }
        config.validate()?;
        self.state.transition(SidecarState::Starting)?;
        self.emit_state();
        let pipeline = match PipelineSession::start(
            &config,
            self.input.clone(),
            self.muted.clone(),
            self.metrics.clone(),
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                let _ = self.state.transition(SidecarState::Idle);
                self.state.failed(error.clone());
                self.emit_state();
                return Err(error);
            }
        };
        self.emit_event(
            "sessionStarted",
            json!({
                "sessionId": config.session_id.clone(),
                "rtpPayloadType": pipeline::RTP_PAYLOAD_TYPE,
                "maxRtpPayloadBytes": pipeline::MAX_RTP_PAYLOAD_BYTES,
                "rtpOffsets": pipeline.rtp_offsets,
                "webrtcDspEnabled": pipeline.webrtc_dsp_enabled,
                "rtcpMux": false,
                "rtcpPort": config.resolved_rtcp_port()?,
                "rtcpListenPort": config.resolved_rtcp_listen_port()?
            }),
        );
        self.pipeline = Some(pipeline);
        self.session_config = Some(config.clone());
        self.restart_capture(&config);
        Ok(())
    }

    fn stop_session(&mut self) {
        if self.pipeline.is_none() {
            return;
        }
        let _ = self.state.transition(SidecarState::Stopping);
        self.emit_state();
        self.capture.stop();
        self.active_device_id = None;
        if let Some(mut pipeline) = self.pipeline.take() {
            pipeline.stop();
        }
        self.session_config = None;
        self.input.store(AudioInput::silent());
        let _ = self.state.transition(SidecarState::Idle);
        self.state.healthy();
        self.emit_state();
        self.emit_event("sessionStopped", json!({}));
    }

    fn shutdown(&mut self) {
        self.stop_session();
        let _ = self.state.transition(SidecarState::Shutdown);
        self.emit_state();
    }

    fn select_device(&mut self, device_id: Option<String>) -> Result<(), String> {
        self.selected_device_id = device_id.filter(|id| !id.trim().is_empty() && id != "default");
        self.emit_event(
            "deviceSelected",
            json!({ "deviceId": self.selected_device_id }),
        );
        if let Some(config) = self.session_config.clone() {
            if config.source == SourceKind::Microphone {
                self.restart_capture(&config);
            }
        }
        Ok(())
    }

    fn set_bitrate(&mut self, bitrate: u32) -> Result<(), String> {
        if let Some(pipeline) = &self.pipeline {
            pipeline.set_bitrate(bitrate)?;
        } else if !(6_000..=128_000).contains(&bitrate) {
            return Err("bitrate must be between 6000 and 128000 bits/s".to_string());
        }
        if let Some(config) = &mut self.session_config {
            config.bitrate = bitrate;
        }
        self.emit_event("bitrateChanged", json!({ "bitrate": bitrate }));
        Ok(())
    }

    fn restart_capture(&mut self, config: &SessionConfig) {
        self.metrics.capture_restart();
        match self.capture.start(
            config.source,
            self.selected_device_id.as_deref(),
            &self.input,
            self.metrics.clone(),
            self.capture_tx.clone(),
        ) {
            Ok(started) => {
                self.active_device_id = Some(started.active_device_id.clone());
                self.emit_event(
                    "captureStarted",
                    json!({
                        "deviceId": started.active_device_id,
                        "deviceName": started.active_device_name,
                        "sampleRate": started.sample_rate,
                        "fallback": started.used_fallback
                    }),
                );
                if started.used_fallback {
                    let message = "selected device is unavailable; capturing from the default device while retrying";
                    let _ = self.state.transition(SidecarState::Recovering);
                    self.state.degraded(message);
                    self.retry_at = Instant::now() + CAPTURE_RETRY_INTERVAL;
                    self.emit_event("deviceFallback", json!({ "reason": message }));
                } else {
                    let _ = self.state.transition(SidecarState::Running);
                    self.state.healthy();
                }
                self.emit_state();
            }
            Err(error) => {
                self.capture.stop();
                self.active_device_id = None;
                self.metrics.capture_error();
                let _ = self.state.transition(SidecarState::Recovering);
                self.state.degraded(error.clone());
                self.retry_at = Instant::now() + CAPTURE_RETRY_INTERVAL;
                self.emit_event("captureRecovery", json!({ "error": error }));
                self.emit_state();
            }
        }
    }

    fn tick(&mut self) {
        while let Ok(CaptureSignal::Error(error)) = self.capture_rx.try_recv() {
            self.metrics.capture_error();
            self.capture.stop();
            self.active_device_id = None;
            let _ = self.state.transition(SidecarState::Recovering);
            self.state
                .degraded(format!("CPAL capture stream failed: {error}"));
            self.retry_at = Instant::now() + CAPTURE_RETRY_INTERVAL;
            self.emit_event("captureError", json!({ "error": error }));
            self.emit_state();
        }

        if let Some(error) = self.pipeline.as_ref().and_then(PipelineSession::poll_error) {
            self.metrics.pipeline_error();
            self.state.failed(error.clone());
            self.emit_event("pipelineError", json!({ "error": error }));
            self.emit_state();
        }

        let Some(config) = self.session_config.clone() else {
            return;
        };
        if config.source == SourceKind::Sine {
            return;
        }

        let now = Instant::now();
        if self.capture.active_device_id().is_none() && now >= self.retry_at {
            self.restart_capture(&config);
            return;
        }
        if now < self.next_device_poll {
            return;
        }
        self.next_device_poll = now + DEVICE_POLL_INTERVAL;

        let should_restart = if let Some(preferred) = self.selected_device_id.as_deref() {
            self.capture.active_device_id() != Some(preferred) || !device_available(preferred)
        } else {
            current_default_device_id().as_deref() != self.capture.active_device_id()
        };
        if should_restart {
            self.emit_event(
                "deviceChangeDetected",
                json!({ "selectedDeviceId": self.selected_device_id }),
            );
            self.restart_capture(&config);
        }
    }

    fn emit_state(&self) {
        self.emit_event(
            "stateChanged",
            serde_json::to_value(self.status()).unwrap_or_else(|_| json!({})),
        );
    }

    fn emit_event(&self, event: &str, data: Value) {
        emit(Output::Event {
            event: event.to_string(),
            data,
        });
    }
}
