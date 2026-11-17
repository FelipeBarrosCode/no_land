use super::*;
use crate::rpc_handler::{AgentRpc, AGENT_API_VERSION};
use crate::{AgentConfig, StateAgent};
use async_trait::async_trait;
use noland_rpc::RpcHandler;
use noland_state_core::{OperationMetrics, OperationProgress, OperationRecord};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use tokio::sync::{oneshot, watch, Barrier};
use uuid::Uuid;

const PROBE_DEADLINE: Duration = Duration::from_secs(3);
const RPC_DEADLINE: Duration = Duration::from_secs(8);

#[derive(Default)]
struct BlockingGate {
    released: Mutex<bool>,
    changed: Condvar,
    expired: AtomicBool,
}

impl BlockingGate {
    fn wait(&self) {
        let (_guard, timeout) = self
            .changed
            .wait_timeout_while(self.released.lock().unwrap(), RPC_DEADLINE, |open| !*open)
            .unwrap();
        if timeout.timed_out() {
            self.expired.store(true, Ordering::SeqCst);
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

struct ReleaseOnDrop(Arc<BlockingGate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn request(method: &str, params: Value) -> RpcRequest {
    RpcRequest {
        id: Uuid::new_v4().to_string(),
        method: method.into(),
        params,
    }
}

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(PROBE_DEADLINE, async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("synchronization deadline");
}

#[test]
fn only_committed_display_reads_use_the_reserved_lane() {
    for method in ["GetHealth", "ListApps", "ListBackupCandidateIds"] {
        assert_eq!(
            Lane::for_request(&request(method, json!({}))),
            Lane::Display
        );
    }
    for method in [
        "GetOperationStatus",
        "GetBackupStatus",
        "GetRestoreStatus",
        "GetSealStatus",
    ] {
        assert_eq!(
            Lane::for_request(&request(method, json!({"projection":"progress"}))),
            Lane::Display
        );
        for params in [json!({}), json!({"projection":"matching"})] {
            assert_eq!(Lane::for_request(&request(method, params)), Lane::Standard);
        }
    }
    for method in [
        "StartBackup",
        "StartRestore",
        "StartSeal",
        "RetryOperation",
        "CancelOperation",
        "VerifyBackupCommit",
        "ListRecentOperations",
        "GetActiveAppSessions",
        "RefreshIndex",
        "ReconcileApp",
        "PrepareDeletion",
        "StartDeletion",
        "unknown",
    ] {
        assert_eq!(
            Lane::for_request(&request(method, json!({"projection":"progress"}))),
            Lane::Standard,
            "{method}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn queue_and_workers_are_bounded_and_aborted_waiters_cannot_release_running_mutations() {
    let budget = Arc::new(ExecutionBudget::new(1, 1, 1));
    let gate = Arc::new(BlockingGate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (started, waiting) = oneshot::channel();
    let (finished, finishing) = oneshot::channel();
    let running_budget = budget.clone();
    let running_gate = gate.clone();
    let running = tokio::spawn(async move {
        running_budget
            .run(Lane::Standard, move || async move {
                let _ = started.send(());
                running_gate.wait();
                let _ = finished.send(());
                Ok(json!("mutation drained"))
            })
            .await
    });
    tokio::time::timeout(PROBE_DEADLINE, waiting)
        .await
        .unwrap()
        .unwrap();
    let queued_budget = budget.clone();
    let queued = tokio::spawn(async move {
        queued_budget
            .run(Lane::Standard, || async { Ok(json!("queued")) })
            .await
    });
    until(|| budget.standard.admitted.available_permits() == 0).await;
    let dispatched = Arc::new(AtomicBool::new(false));
    let attempted = dispatched.clone();
    let rejected = budget
        .run(Lane::Standard, move || async move {
            attempted.store(true, Ordering::SeqCst);
            Ok(Value::Null)
        })
        .await;
    assert!(rejected.unwrap_err().to_string().contains("not dispatched"));
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(
        tokio::time::timeout(
            PROBE_DEADLINE,
            budget.run(Lane::Display, || async { Ok(json!("display")) })
        )
        .await
        .unwrap()
        .unwrap(),
        json!("display")
    );
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert_eq!(budget.standard.workers.available_permits(), 0);
    assert_eq!(budget.standard.admitted.available_permits(), 0);
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    assert_eq!(budget.standard.admitted.available_permits(), 1);
    assert_eq!(budget.standard.workers.available_permits(), 0);
    gate.release();
    tokio::time::timeout(PROBE_DEADLINE, finishing)
        .await
        .unwrap()
        .unwrap();
    until(|| {
        budget.standard.workers.available_permits() == 1
            && budget.standard.admitted.available_permits() == 2
    })
    .await;
    assert!(!gate.expired.load(Ordering::SeqCst));
}

#[tokio::test]
async fn handler_errors_and_panics_are_errors_and_return_all_capacity() {
    let budget = ExecutionBudget::new(1, 1, 0);
    let error = budget
        .run(Lane::Display, || async {
            Err(noland_state_core::StateError::Database(
                "fixture read failure".into(),
            ))
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("fixture read failure"));
    let error = budget
        .run(Lane::Standard, || async {
            if true {
                panic!("intentional handler panic");
            }
            Ok(Value::Null)
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("worker terminated"));
    assert_eq!(budget.standard.workers.available_permits(), 1);
    assert_eq!(budget.standard.admitted.available_permits(), 1);
    assert_eq!(budget.display.workers.available_permits(), 1);
    assert_eq!(budget.display.admitted.available_permits(), 1);
}

#[derive(Clone)]
struct SocketHandler {
    agent: Arc<StateAgent>,
    gate: Arc<BlockingGate>,
    started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    finished: Arc<AtomicBool>,
}

#[async_trait]
impl RpcHandler for SocketHandler {
    async fn handle(&self, incoming: &RpcRequest) -> Result<Value> {
        let handler = self.clone();
        let owned = incoming.clone();
        // Same production admission, lane selection, spawn_blocking and
        // captured-runtime block_on. Only the heavy inner body is substituted;
        // no test-only wire command or server/framing changes are needed.
        execute(incoming, move || async move {
            if owned.method == "ReconcileApp" {
                handler
                    .started
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                handler.gate.wait();
                handler.finished.store(true, Ordering::SeqCst);
                Ok(json!({"reconciled":1}))
            } else {
                AgentRpc(handler.agent).dispatch(&owned).await
            }
        })
        .await
    }
}

fn fixture() -> (std::path::PathBuf, Arc<StateAgent>, Uuid, OperationProgress) {
    // Keep the Unix socket below sockaddr_un's macOS length bound even with the
    // canonical (rather than symlink-shortened) approved TMPDIR.
    let root =
        std::env::temp_dir().join(format!("rpc-{}", &Uuid::new_v4().simple().to_string()[..8]));
    let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
    let id = Uuid::new_v4();
    agent
        .db
        .upsert_operation(&OperationRecord {
            operation_id: id,
            kind: "backup".into(),
            app_id: None,
            state: "RECONCILING".into(),
            created_at: "2026-11-16T14:16:11Z".parse().unwrap(),
            updated_at: "2026-11-16T14:19:06Z".parse().unwrap(),
            last_error: None,
            detail_json: json!({}),
        })
        .unwrap();
    let mut progress = OperationProgress::new("reconciling", 72192);
    progress.total_units = Some(186735);
    progress.unit = Some("files".into());
    progress.detail_json = json!({"streaming_pipeline":{"completed_files":72192}});
    agent
        .db
        .set_operation_progress(id, Some(&progress))
        .unwrap();
    agent
        .db
        .set_operation_metrics(id, &OperationMetrics::default())
        .unwrap();
    (root, agent, id, progress)
}

fn socket_probes_with_workers(workers: usize) {
    let (root, agent, id, progress) = fixture();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let socket = root.join("s");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let gate = Arc::new(BlockingGate::default());
            let _release = ReleaseOnDrop(gate.clone());
            let (started, waiting) = oneshot::channel();
            let finished = Arc::new(AtomicBool::new(false));
            let handler = SocketHandler {
                agent: agent.clone(),
                gate: gate.clone(),
                started: Arc::new(Mutex::new(Some(started))),
                finished: finished.clone(),
            };
            let serving = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                for _ in 0..7 {
                    let (stream, _) = listener.accept().await.unwrap();
                    let handler = handler.clone();
                    connections
                        .spawn(async move { noland_rpc::serve_connection(stream, handler).await });
                }
                while let Some(connection) = connections.join_next().await {
                    // The deliberately disconnected heavy caller may produce
                    // BrokenPipe; it must not cancel the executing handler.
                    let _ = connection.unwrap();
                }
            });
            let heavy_socket = socket.clone();
            let heavy = tokio::spawn(async move {
                tokio::time::timeout(
                    RPC_DEADLINE,
                    noland_rpc::call(&heavy_socket, "ReconcileApp", json!({"app_id":"fixture"})),
                )
                .await
            });
            tokio::time::timeout(PROBE_DEADLINE, waiting)
                .await
                .unwrap()
                .unwrap();
            heavy.abort();
            let _ = heavy.await;
            let probes = tokio::time::timeout(PROBE_DEADLINE, async {
                let health = tokio::time::timeout(
                    RPC_DEADLINE,
                    noland_rpc::call(&socket, "GetHealth", json!({})),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(health["agent_api_version"], AGENT_API_VERSION);
                assert_eq!(health["unfinished_operations"], 1);
                for key in [
                    "writer_lock_acquisitions",
                    "writer_lock_wait_us",
                    "monitoring_lock_acquisitions",
                    "monitoring_lock_wait_us",
                ] {
                    assert!(health["database_contention"][key].is_u64(), "{key}");
                }
                for method in [
                    "GetOperationStatus",
                    "GetBackupStatus",
                    "GetRestoreStatus",
                    "GetSealStatus",
                ] {
                    let status = tokio::time::timeout(
                        RPC_DEADLINE,
                        noland_rpc::call(
                            &socket,
                            method,
                            json!({"operation_id":id,"projection":"progress"}),
                        ),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert_eq!(status["operation_id"], id.to_string());
                    assert_eq!(status["state"], "RECONCILING");
                    assert_eq!(status["progress"], serde_json::to_value(&progress).unwrap());
                    assert_eq!(status["created_at"], "2026-11-16T14:16:11Z");
                }
                let apps = tokio::time::timeout(
                    RPC_DEADLINE,
                    noland_rpc::call(&socket, "ListApps", json!({})),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(apps, json!([]));
                assert!(
                    !finished.load(Ordering::SeqCst),
                    "all socket probes must finish BEFORE releasing the heavy handler"
                );
                assert!(!gate.expired.load(Ordering::SeqCst));
            })
            .await;
            gate.release();
            if probes.is_err() {
                serving.abort();
            }
            let _ = tokio::time::timeout(RPC_DEADLINE, serving).await.unwrap();
            until(|| finished.load(Ordering::SeqCst)).await;
            assert!(
                probes.is_ok(),
                "display probes exceeded 3s with {workers} I/O workers"
            );
        });
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_uds_health_and_progress_finish_before_blocker_release_with_one_io_worker() {
    socket_probes_with_workers(1);
}

#[test]
fn real_uds_health_and_progress_finish_before_blocker_release_with_two_io_workers() {
    socket_probes_with_workers(2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn eight_nested_retries_do_not_reacquire_the_outer_standard_budget() {
    let (root, agent, _, _) = fixture();
    let ready = Arc::new(Barrier::new(STANDARD_WORKERS));
    let (stop, stopping) = watch::channel(false);
    let mut requests = tokio::task::JoinSet::new();
    let mut ids = Vec::new();
    for _ in 0..STANDARD_WORKERS {
        let id = Uuid::new_v4();
        ids.push(id);
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id: id,
                kind: "backup".into(),
                app_id: None,
                state: "FAILED".into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                last_error: Some("fixture failure".into()),
                detail_json: json!({}),
            })
            .unwrap();
        let rpc = AgentRpc(agent.clone());
        let ready = ready.clone();
        let mut stopping = stopping.clone();
        let request = request(
            "RetryOperation",
            json!({
                "operation_id":id,
                // Reach the nested StartBackup dispatcher, then fail validation
                // before any work/credentials/filesystem/cloud action can start.
                "session":{}
            }),
        );
        requests.spawn(async move {
            execute(&request.clone(), move || async move {
                tokio::select! {
                    result = async {
                        ready.wait().await;
                        rpc.dispatch(&request).await
                    } => result,
                    _ = stopping.changed() => Err(StateError::Message("test drain".into())),
                }
            })
            .await
        });
    }
    let completed = tokio::time::timeout(PROBE_DEADLINE, async {
        let mut results = Vec::new();
        while let Some(result) = requests.join_next().await {
            results.push(result.unwrap());
        }
        results
    })
    .await;
    // A regression that recursively enters handle() is cancellable here, so
    // its eight blocked workers are drained rather than hanging runtime drop.
    stop.send(true).unwrap();
    while requests.join_next().await.is_some() {}
    let results = completed.expect("eight retries must not deadlock their own lane");
    assert_eq!(results.len(), STANDARD_WORKERS);
    for result in results {
        let error = result.unwrap_err().to_string();
        assert!(error.contains("missing field"), "{error}");
    }
    for id in ids {
        assert_eq!(agent.db.get_operation(id).unwrap().unwrap().state, "FAILED");
        assert!(!agent.operations.is_running(id));
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
