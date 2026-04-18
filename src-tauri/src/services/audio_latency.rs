use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use super::{app_config::AppConfig, remote_exec::RemoteExec};

const AUDIO_SETUP_SCRIPT: &str = include_str!("../../scripts/setup_low_latency_audio.sh");

#[derive(Debug, Clone)]
pub struct AudioLatencyService {
    target_user: String,
    profile: String,
    force_sink_override: bool,
    sink_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AudioSetupResult {
    pub verification_output: String,
}

impl AudioLatencyService {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            target_user: config.audio_target_user.clone(),
            profile: normalize_profile(&config.audio_profile),
            force_sink_override: config.audio_force_sink_override,
            sink_override: config.audio_sink_override.clone(),
        }
    }

    pub async fn configure(&self, remote: &RemoteExec) -> AppResult<AudioSetupResult> {
        let encoded_script = STANDARD.encode(AUDIO_SETUP_SCRIPT.as_bytes());
        let mut args = format!(
            "--target-user {} --profile {}",
            shell_single_quote(&self.target_user),
            shell_single_quote(&self.profile)
        );

        if self.force_sink_override {
            args.push_str(" --force-sink-override");
        }

        if let Some(sink_override) = &self.sink_override {
            args.push_str(" --sink-override ");
            args.push_str(&shell_single_quote(sink_override));
        }

        let remote_command = format!(
            "sudo bash -lc 'set -euo pipefail; base64 -d > /tmp/noland-lowlatency-audio.sh <<\"EOF\"\n{}\nEOF\nchmod +x /tmp/noland-lowlatency-audio.sh\n/tmp/noland-lowlatency-audio.sh {}'",
            encoded_script, args
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&remote_command, Duration::from_secs(900))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            if matches!(output.status_code, 3 | 5) {
                warn!(
                    "Low-latency audio setup skipped (non-fatal, user session unavailable) (user={}, profile={}): stdout: {} | stderr: {}",
                    self.target_user,
                    self.profile,
                    output.stdout.trim(),
                    output.stderr.trim()
                );
                return Ok(AudioSetupResult {
                    verification_output: format!(
                        "Audio low-latency setup skipped: user session bus unavailable for user {}. stdout: {} | stderr: {}",
                        self.target_user,
                        output.stdout.trim(),
                        output.stderr.trim()
                    ),
                });
            }
            return Err(AppError::Provisioning(format!(
                "Low-latency audio setup failed (user={}, profile={}): {} {}",
                self.target_user,
                self.profile,
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }

        info!(
            "low-latency audio verification output (user={} profile={}):\n{}",
            self.target_user, self.profile, output.stdout
        );

        Ok(AudioSetupResult {
            verification_output: output.stdout,
        })
    }
}

fn normalize_profile(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "fallback1" | "fallback_1" | "512" => "fallback1".to_string(),
        "fallback2" | "fallback_2" | "1024" => "fallback2".to_string(),
        _ => "aggressive".to_string(),
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
