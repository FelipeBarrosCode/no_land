use std::time::Duration;

use tokio::time::sleep;
use tracing::warn;

use crate::{
    errors::{AppError, AppResult},
    models::vast::VastInstance,
};

use super::vast_api::VastApiClient;

#[derive(Debug, Clone)]
pub struct InstanceManager {
    pub poll_interval: Duration,
    pub max_attempts: usize,
}

impl InstanceManager {
    pub async fn create_instance(
        &self,
        api: &VastApiClient,
        offer_id: u64,
        template_hash: &str,
        storage_gb: u32,
        env_vars: Option<serde_json::Value>,
    ) -> AppResult<VastInstance> {
        api.create_instance(
            offer_id,
            template_hash,
            storage_gb,
            "Noland Connect Session",
            env_vars,
        )
        .await
    }

    pub async fn wait_until_ssh_ready<F>(
        &self,
        api: &VastApiClient,
        instance_id: u64,
        mut on_poll: F,
        should_cancel: impl Fn() -> bool,
    ) -> AppResult<VastInstance>
    where
        F: FnMut(usize, &VastInstance),
    {
        const FAST_API_RETRY_DELAY: Duration = Duration::from_secs(10);
        let mut last_error: Option<String> = None;

        let started = tokio::time::Instant::now();
        // Keep the existing overall wait budget, rather than reducing it by
        // increasing polling frequency on slow-booting high-RAM hosts.
        let deadline = started + self.poll_interval.saturating_mul(self.max_attempts as u32);
        let mut attempt = 0;
        while tokio::time::Instant::now() < deadline {
            attempt += 1;
            if should_cancel() {
                return Err(AppError::Cancelled);
            }

            let instance = match api.get_instance(instance_id).await {
                Ok(instance) => {
                    last_error = None;
                    instance
                }
                Err(error) => {
                    let message = format!(
                        "Attempt {attempt}/{} failed to fetch instance {}: {}",
                        self.max_attempts, instance_id, error
                    );
                    warn!("{message}");
                    last_error = Some(message);

                    if tokio::time::Instant::now() < deadline {
                        let retry_delay = if self.poll_interval > FAST_API_RETRY_DELAY {
                            FAST_API_RETRY_DELAY
                        } else {
                            self.poll_interval
                        };
                        warn!(
                            "Retrying instance {} fetch in {}s (fast retry for transient API/network errors)",
                            instance_id,
                            retry_delay.as_secs()
                        );
                        sleep(retry_delay).await;
                        continue;
                    }

                    break;
                }
            };
            on_poll(attempt, &instance);

            if !instance.image_runtype.trim().is_empty() && !instance.is_vm_runtime() {
                return Err(AppError::Provisioning(format!(
                    "Instance {} is not VM runtime while waiting for readiness (runtime='{}'). Choose a VM-backed offer/template.",
                    instance_id,
                    instance.image_runtype.trim()
                )));
            }

            if instance.is_inactive() {
                return Err(AppError::Provisioning(format!(
                    "Instance {} is not available at this moment (status: {}). Please choose another server and try again.",
                    instance_id,
                    instance.status
                )));
            }

            if instance.ssh_ready() && is_usable_ssh_host(&instance.ssh_host) {
                return Ok(instance);
            }

            // Don't fail immediately if instance is still loading - high-RAM machines take time
            if instance.is_loading() || !is_usable_ssh_host(&instance.ssh_host) {
                warn!(
                    "Instance {} still loading (status: {}, ssh_host: {}) - attempt {}/{} - high-RAM machines may take 30+ minutes",
                    instance_id,
                    instance.status,
                    if is_usable_ssh_host(&instance.ssh_host) {
                        &instance.ssh_host
                    } else {
                        "pending"
                    },
                    attempt,
                    self.max_attempts
                );
            }

            if should_cancel() {
                return Err(AppError::Cancelled);
            }

            sleep(readiness_poll_delay(started.elapsed(), self.poll_interval)).await;
        }

        if let Some(last_error) = last_error {
            return Err(AppError::Timeout(format!(
                "Instance {instance_id} did not become SSH-ready after {} attempts. Last API poll error: {last_error}",
                self.max_attempts
            )));
        }

        Err(AppError::Timeout(format!(
            "Instance {instance_id} did not become SSH-ready after {} attempts",
            self.max_attempts
        )))
    }
}

fn readiness_poll_delay(elapsed: Duration, maximum: Duration) -> Duration {
    let delay = if elapsed < Duration::from_secs(60) {
        Duration::from_secs(5)
    } else if elapsed < Duration::from_secs(180) {
        Duration::from_secs(10)
    } else {
        Duration::from_secs(30)
    };
    delay.min(maximum).max(Duration::from_millis(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_polls_fast_initially_without_exceeding_configured_interval() {
        let maximum = Duration::from_secs(60);
        assert_eq!(readiness_poll_delay(Duration::ZERO, maximum).as_secs(), 5);
        assert_eq!(
            readiness_poll_delay(Duration::from_secs(90), maximum).as_secs(),
            10
        );
        assert_eq!(
            readiness_poll_delay(Duration::from_secs(200), maximum).as_secs(),
            30
        );
        assert_eq!(
            readiness_poll_delay(Duration::ZERO, Duration::from_secs(2)).as_secs(),
            2
        );
    }
}

fn is_usable_ssh_host(host: &str) -> bool {
    let normalized = host.trim().to_ascii_lowercase();
    !normalized.is_empty()
        && normalized != "pending"
        && normalized != "none"
        && normalized != "null"
        && normalized != "0.0.0.0"
        && normalized != "-"
}
