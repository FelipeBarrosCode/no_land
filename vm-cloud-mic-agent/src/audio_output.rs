use async_trait::async_trait;
use std::process::Command;
use tracing::{info, warn};

/// Abstract audio output for the Cloud Mic virtual device.
#[async_trait]
pub trait AudioOutput {
    async fn create_or_verify(&self) -> Result<(), Box<dyn std::error::Error>>;
    async fn recreate(&self) -> Result<(), Box<dyn std::error::Error>>;
    async fn write_pcm(&self, _frames: &[i16]) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    async fn write_silence(&self, _duration_ms: u32) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    async fn set_default(&self) -> Result<(), Box<dyn std::error::Error>>;
    async fn is_ready(&self) -> Result<bool, Box<dyn std::error::Error>>;
    fn backend_name(&self) -> &'static str;
}

// ===================================================================
// PipeWire-native implementation (preferred)
// ===================================================================

pub struct PipewireAudio {
    node_name: String,
    description: String,
}

impl PipewireAudio {
    pub fn new(node_name: &str, description: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Check if pipewire and pw-cli are available
        let check = Command::new("sh")
            .arg("-c")
            .arg("command -v pw-cli && command -v pactl")
            .output()?;

        if !check.status.success() {
            return Err("PipeWire tools not found".into());
        }

        Ok(Self {
            node_name: node_name.to_string(),
            description: description.to_string(),
        })
    }
}

#[async_trait]
impl AudioOutput for PipewireAudio {
    async fn create_or_verify(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Check if node already exists
        let list = Command::new("pactl")
            .args(["list", "sources", "short"])
            .output()?;

        let stdout = String::from_utf8_lossy(&list.stdout);
        if stdout.contains(&self.node_name) {
            info!("PipeWire source '{}' already exists", self.node_name);
            return Ok(());
        }

        // Create a null sink with monitor source
        let cmd = format!(
            "pactl load-module module-null-sink sink_name={name} sink_properties='device.description=\"{desc}\"' rate=48000 channels=1",
            name = self.node_name,
            desc = self.description,
        );

        let output = Command::new("sh").arg("-c").arg(&cmd).output()?;
        if !output.status.success() {
            return Err(format!(
                "Failed to create PipeWire sink: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        info!("Created PipeWire null sink '{}'", self.node_name);
        Ok(())
    }

    async fn recreate(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Unload existing module for this sink if any
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "pactl list | grep -A2 'Name: {}' | grep 'Module:' | awk '{{print $2}}' | xargs -r pactl unload-module",
                self.node_name
            ))
            .output()?;

        self.create_or_verify().await
    }

    async fn set_default(&self) -> Result<(), Box<dyn std::error::Error>> {
        let monitor_name = format!("{}.monitor", self.node_name);
        let output = Command::new("pactl")
            .args(["set-default-source", &monitor_name])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to set default source: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        info!("Set default source to '{}'", monitor_name);
        Ok(())
    }

    async fn is_ready(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let list = Command::new("pactl")
            .args(["list", "sources", "short"])
            .output()?;

        let stdout = String::from_utf8_lossy(&list.stdout);
        Ok(stdout.contains(&self.node_name))
    }

    fn backend_name(&self) -> &'static str {
        "pipewire"
    }
}

// ===================================================================
// PulseAudio fallback implementation
// ===================================================================

pub struct PulseFallbackAudio {
    sink_name: String,
    description: String,
}

impl PulseFallbackAudio {
    pub fn new(sink_name: &str, description: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let check = Command::new("sh")
            .arg("-c")
            .arg("command -v pactl")
            .output()?;

        if !check.status.success() {
            return Err("pactl not found".into());
        }

        Ok(Self {
            sink_name: sink_name.to_string(),
            description: description.to_string(),
        })
    }
}

#[async_trait]
impl AudioOutput for PulseFallbackAudio {
    async fn create_or_verify(&self) -> Result<(), Box<dyn std::error::Error>> {
        let list = Command::new("pactl")
            .args(["list", "sources", "short"])
            .output()?;

        let stdout = String::from_utf8_lossy(&list.stdout);
        if stdout.contains(&self.sink_name) {
            return Ok(());
        }

        let cmd = format!(
            "pactl load-module module-null-sink sink_name={name} sink_properties='device.description=\"{desc}\"' rate=48000 channels=1",
            name = self.sink_name,
            desc = self.description,
        );

        let output = Command::new("sh").arg("-c").arg(&cmd).output()?;
        if !output.status.success() {
            return Err(format!(
                "Failed to create PulseAudio null sink: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        info!("Created PulseAudio null sink '{}'", self.sink_name);
        Ok(())
    }

    async fn recreate(&self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "pactl list | grep -A2 'Name: {}' | grep 'Module:' | awk '{{print $2}}' | xargs -r pactl unload-module",
                self.sink_name
            ))
            .output()?;

        self.create_or_verify().await
    }

    async fn set_default(&self) -> Result<(), Box<dyn std::error::Error>> {
        let monitor = format!("{}.monitor", self.sink_name);
        let output = Command::new("pactl")
            .args(["set-default-source", &monitor])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to set default source: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        Ok(())
    }

    async fn is_ready(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let list = Command::new("pactl")
            .args(["list", "sources", "short"])
            .output()?;

        let stdout = String::from_utf8_lossy(&list.stdout);
        Ok(stdout.contains(&self.sink_name))
    }

    fn backend_name(&self) -> &'static str {
        "pulse_fallback"
    }
}
