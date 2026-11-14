//! Operation-local bypasses must never disable normal session protection.
use super::*;
use crate::AgentConfig;
use noland_rclone_adapter::{TransferWorkload, WorkerResources};
use noland_state_core::AppId;
use std::sync::Arc;

fn fixture() -> (PathBuf, Arc<StateAgent>) {
    let root = std::env::temp_dir().join(format!(
        "noland-active-transfer-policy-{}",
        uuid::Uuid::new_v4()
    ));
    let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
    let app = AppIdentity::new(AppId::desktop("protected-session"), "Protected session");
    agent.db.upsert_app(&app).unwrap();
    // An unknown process must retain the existing conservative protection,
    // including on hosts where Linux procfs is unavailable.
    agent
        .db
        .insert_session(&AppSession::new(
            app.app_id,
            0,
            SessionSource::ExecutableDiscovery,
        ))
        .unwrap();
    assert_normal_protection(&agent);
    (root, agent)
}

fn normal_profile(agent: &StateAgent) -> TransferWorkload {
    if agent.has_latency_sensitive_sessions().unwrap() {
        TransferWorkload::GameplaySafe
    } else {
        TransferWorkload::Throughput
    }
}

fn assert_normal_protection(agent: &StateAgent) {
    let resources = WorkerResources {
        logical_cpus: 30,
        available_memory: Some(20 << 30),
        io_pressure: false,
    };
    let normal = normal_profile(agent);
    assert_eq!(normal, TransferWorkload::GameplaySafe);
    assert_eq!(resources.cpu_workers(normal, 96 << 20), 8);
}

#[tokio::test]
async fn completion_and_failure_leave_normal_gameplay_protection_enabled() {
    let (root, agent) = fixture();
    for fail in [false, true] {
        let result: Result<()> = async {
            let active = normal_profile(&agent).for_active_transfer();
            assert_eq!(active, TransferWorkload::Throughput);
            assert_normal_protection(&agent);
            if fail {
                Err(noland_state_core::StateError::Storage(
                    "injected transfer failure".into(),
                ))
            } else {
                Ok(())
            }
        }
        .await;
        assert_eq!(result.is_err(), fail);
        assert_normal_protection(&agent);
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelling_a_transfer_cannot_leave_gameplay_protection_disabled() {
    let (root, agent) = fixture();
    let (started, entered) = tokio::sync::oneshot::channel();
    let worker_agent = Arc::clone(&agent);
    let task = tokio::spawn(async move {
        let active = normal_profile(&worker_agent).for_active_transfer();
        assert_eq!(active, TransferWorkload::Throughput);
        started.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    entered.await.unwrap();
    assert_normal_protection(&agent);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_normal_protection(&agent);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn one_finishing_transfer_does_not_restrict_an_overlapping_transfer() {
    let (root, agent) = fixture();
    let (first_done, first_wait) = tokio::sync::oneshot::channel();
    let (second_done, second_wait) = tokio::sync::oneshot::channel();
    let (first_started, first_entered) = tokio::sync::oneshot::channel();
    let (second_started, second_entered) = tokio::sync::oneshot::channel();
    let spawn = |started: tokio::sync::oneshot::Sender<()>,
                 done: tokio::sync::oneshot::Receiver<()>| {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            let active = normal_profile(&agent).for_active_transfer();
            started.send(()).unwrap();
            done.await.unwrap();
            assert_eq!(active, TransferWorkload::Throughput);
            assert_normal_protection(&agent);
        })
    };
    let first = spawn(first_started, first_wait);
    let second = spawn(second_started, second_wait);
    first_entered.await.unwrap();
    second_entered.await.unwrap();
    first_done.send(()).unwrap();
    first.await.unwrap();
    assert_normal_protection(&agent);
    assert_eq!(
        normal_profile(&agent).for_active_transfer(),
        TransferWorkload::Throughput
    );
    second_done.send(()).unwrap();
    second.await.unwrap();
    assert_normal_protection(&agent);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
