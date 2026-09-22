use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use noland_lifecycle_agent::activity::{
    monitor_stream_input_devices, serve_activity_socket, ActivityProcessor, ActivityRecorder,
};
use noland_lifecycle_agent::clock::{Clock, SystemClock};
use noland_lifecycle_agent::config::{Config, DEFAULT_CONFIG_PATH, EVALUATION_TICK_MS};
use noland_lifecycle_agent::db::Database;
use noland_lifecycle_agent::engine::{
    FileCapabilitySource, LifecycleEngine, Sleeper, TokioSleeper,
};
use noland_lifecycle_agent::provider::{ProviderLifecycle, VastLifecycleProvider};
use noland_lifecycle_agent::rpc_server::{serve_status_socket, LifecycleRpcService};
use noland_lifecycle_agent::state_agent::{
    ForegroundPidBackend, StateAgentClient, UnixStateAgentClient, XpropForegroundPidBackend,
};
use noland_lifecycle_agent::{AgentError, Result};
use tokio::time::MissedTickBehavior;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    if let Err(error) = run().await {
        error!(error = %error, "lifecycle agent terminated");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let config_path = PathBuf::from(DEFAULT_CONFIG_PATH);
    let initial_config = Config::load(&config_path)?;
    initial_config.validate()?;
    let database = Arc::new(Database::open(&initial_config.database_path)?);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
    database.initialize_monitoring(initial_config.enabled, clock.now_utc())?;

    let state_agent_socket = initial_config.state_agent_socket.clone();
    let status_socket = initial_config.status_socket.clone();
    let activity_socket = initial_config.activity_socket.clone();
    let config = Arc::new(RwLock::new(initial_config));
    let activity = Arc::new(ActivityProcessor::new(
        Arc::clone(&database),
        Arc::clone(&config),
        Arc::clone(&clock),
    ));
    let state_agent: Arc<dyn StateAgentClient> =
        Arc::new(UnixStateAgentClient::new(state_agent_socket));
    let foreground: Arc<dyn ForegroundPidBackend> = Arc::new(XpropForegroundPidBackend);
    let provider: Arc<dyn ProviderLifecycle> = Arc::new(VastLifecycleProvider::new()?);
    let sleeper: Arc<dyn Sleeper> = Arc::new(TokioSleeper);
    let engine = Arc::new(LifecycleEngine::new(
        Arc::clone(&config),
        database,
        Arc::clone(&activity),
        state_agent,
        foreground,
        provider,
        Arc::new(FileCapabilitySource),
        Arc::clone(&clock),
        sleeper,
    )?);

    let activity_recorder: Arc<dyn ActivityRecorder> = activity;
    let rpc_service = Arc::new(LifecycleRpcService::new(
        Arc::clone(&engine),
        Arc::clone(&activity_recorder),
        config_path,
    ));
    let input_activity_recorder = Arc::clone(&activity_recorder);
    let activity_task =
        tokio::spawn(
            async move { serve_activity_socket(&activity_socket, activity_recorder).await },
        );
    let input_task =
        tokio::spawn(async move { monitor_stream_input_devices(input_activity_recorder).await });
    let status_task =
        tokio::spawn(async move { serve_status_socket(&status_socket, rpc_service).await });
    let evaluation_task = tokio::spawn(evaluation_loop(engine));

    info!("noland lifecycle agent started");
    tokio::select! {
        result = activity_task => task_result("activity listener", result)?,
        result = status_task => task_result("status listener", result)?,
        result = input_task => task_result("stream input monitor", result)?,
        result = evaluation_task => task_result("evaluation loop", result)?,
        signal = tokio::signal::ctrl_c() => signal.map_err(AgentError::from)?,
    }
    info!("noland lifecycle agent stopping");
    Ok(())
}

async fn evaluation_loop(engine: Arc<LifecycleEngine>) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(EVALUATION_TICK_MS));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval.tick().await;
    loop {
        interval.tick().await;
        if let Err(error) = engine.tick().await {
            warn!(error = %error, "lifecycle evaluation tick failed");
        }
    }
}

fn task_result(
    name: &str,
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    match result {
        Ok(Ok(())) => Err(AgentError::new(format!("{name} stopped unexpectedly"))),
        Ok(Err(error)) => Err(error),
        Err(error) => Err(AgentError::new(format!("{name} task failed: {error}"))),
    }
}
