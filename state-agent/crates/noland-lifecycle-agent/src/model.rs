use std::fmt::{Display, Formatter};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    #[serde(rename = "DISABLED")]
    Disabled,
    #[serde(rename = "MONITORING/IDLE")]
    MonitoringIdle,
    #[serde(rename = "TIMEOUT_REACHED")]
    TimeoutReached,
    #[serde(rename = "SELECTING_APPLICATIONS")]
    SelectingApplications,
    #[serde(rename = "BACKING_UP")]
    BackingUp,
    #[serde(rename = "VERIFYING")]
    Verifying,
    #[serde(rename = "SHUTTING_DOWN")]
    ShuttingDown,
    #[serde(rename = "BACKUP_FAILED_SAFE")]
    BackupFailedSafe,
    #[serde(rename = "SHUTDOWN_RETRY_WAIT")]
    ShutdownRetryWait,
    #[serde(rename = "SHUTDOWN_FAILED_SAFE")]
    ShutdownFailedSafe,
    #[serde(rename = "COMPLETED")]
    Completed,
}

impl LifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "DISABLED",
            Self::MonitoringIdle => "MONITORING/IDLE",
            Self::TimeoutReached => "TIMEOUT_REACHED",
            Self::SelectingApplications => "SELECTING_APPLICATIONS",
            Self::BackingUp => "BACKING_UP",
            Self::Verifying => "VERIFYING",
            Self::ShuttingDown => "SHUTTING_DOWN",
            Self::BackupFailedSafe => "BACKUP_FAILED_SAFE",
            Self::ShutdownRetryWait => "SHUTDOWN_RETRY_WAIT",
            Self::ShutdownFailedSafe => "SHUTDOWN_FAILED_SAFE",
            Self::Completed => "COMPLETED",
        }
    }

    pub fn run_is_active(self) -> bool {
        matches!(
            self,
            Self::TimeoutReached
                | Self::SelectingApplications
                | Self::BackingUp
                | Self::Verifying
                | Self::ShuttingDown
                | Self::ShutdownRetryWait
        )
    }
}

impl Display for LifecycleState {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LifecycleState {
    type Err = AgentError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "DISABLED" => Ok(Self::Disabled),
            "MONITORING/IDLE" => Ok(Self::MonitoringIdle),
            "TIMEOUT_REACHED" => Ok(Self::TimeoutReached),
            "SELECTING_APPLICATIONS" => Ok(Self::SelectingApplications),
            "BACKING_UP" => Ok(Self::BackingUp),
            "VERIFYING" => Ok(Self::Verifying),
            "SHUTTING_DOWN" => Ok(Self::ShuttingDown),
            "BACKUP_FAILED_SAFE" => Ok(Self::BackupFailedSafe),
            "SHUTDOWN_RETRY_WAIT" => Ok(Self::ShutdownRetryWait),
            "SHUTDOWN_FAILED_SAFE" => Ok(Self::ShutdownFailedSafe),
            "COMPLETED" => Ok(Self::Completed),
            other => Err(AgentError::new(format!("unknown lifecycle state {other}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RankedApp {
    pub app_id: String,
    pub foreground_active_ms: u64,
    pub process_runtime_ms: u64,
    pub launch_count: u64,
    pub last_active_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupApp {
    pub run_id: String,
    pub app_id: String,
    pub rank: u32,
    pub status: String,
    pub attempts: u32,
    pub operation_id: Option<String>,
    pub bundle_id: Option<String>,
    pub commit_id: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleSnapshot {
    pub state: LifecycleState,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub timeout_reached_at: Option<DateTime<Utc>>,
    pub active_run_id: Option<String>,
    pub last_error: Option<String>,
    pub shutdown_started_at: Option<DateTime<Utc>>,
}
