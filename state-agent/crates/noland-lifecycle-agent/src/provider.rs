use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::config::ProviderAction;
use crate::{AgentError, Result};

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ProviderRequest {
    pub base_url: String,
    pub instance_id: u64,
    pub action: ProviderAction,
    pub api_key: String,
}

impl Drop for ProviderRequest {
    fn drop(&mut self) {
        self.api_key.clear();
    }
}

#[async_trait]
pub trait ProviderLifecycle: Send + Sync {
    async fn apply(&self, request: ProviderRequest) -> Result<()>;
}

pub struct VastLifecycleProvider;

impl VastLifecycleProvider {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

#[async_trait]
impl ProviderLifecycle for VastLifecycleProvider {
    async fn apply(&self, request: ProviderRequest) -> Result<()> {
        let endpoint = format!(
            "{}/api/v0/instances/{}/",
            request.base_url.trim_end_matches('/'),
            request.instance_id
        );
        let method = match request.action {
            ProviderAction::Destroy => "DELETE",
            ProviderAction::Stop => "PUT",
        };

        // Feed the bearer credential through curl's stdin config so it never
        // appears in process arguments or diagnostic output.
        let mut config = format!(
            "url = \"{}\"\nrequest = \"{}\"\nheader = \"Authorization: Bearer {}\"\nconnect-timeout = 10\nmax-time = 30\nsilent\nshow-error\noutput = \"/dev/null\"\nwrite-out = \"%{{http_code}}\"\n",
            escape_curl_config(&endpoint),
            method,
            escape_curl_config(&request.api_key),
        );
        if request.action == ProviderAction::Stop {
            config.push_str("header = \"Content-Type: application/json\"\ndata = \"{\\\"state\\\":\\\"stopped\\\"}\"\n");
        }

        let mut child = Command::new("curl")
            .args(["--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| AgentError::new(format!("unable to start Vast request: {error}")))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| AgentError::new("unable to open Vast request input"))?;
        stdin.write_all(config.as_bytes()).await?;
        drop(stdin);

        let output = timeout(
            PROVIDER_TIMEOUT + Duration::from_secs(2),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| AgentError::new("Vast lifecycle request timed out"))??;
        if !output.status.success() {
            return Err(AgentError::new("Vast lifecycle request failed"));
        }
        let status = String::from_utf8_lossy(&output.stdout);
        let status = status.trim().parse::<u16>().map_err(|_| {
            AgentError::new("Vast lifecycle request returned an invalid HTTP status")
        })?;
        if (200..300).contains(&status) || matches!(status, 404 | 410) {
            return Ok(());
        }
        Err(AgentError::new(format!(
            "Vast lifecycle request failed with HTTP {status}"
        )))
    }
}

fn escape_curl_config(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::escape_curl_config;

    #[test]
    fn curl_config_values_are_escaped() {
        assert_eq!(escape_curl_config("a\\b\"c"), "a\\\\b\\\"c");
    }
}
