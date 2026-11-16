//! Actual AgentRpc -> RetryOperation -> StartBackup -> managed worker coverage.
//! The first interrupted publication uses LocalStorage; RPC replay uses the
//! real session adapter with a filesystem-only fake rclone in a helper test
//! process. No production hooks, real rclone/cloud, global PATH mutation, or
//! application/service launch is involved. Python's standard library is enough.

use super::*;
use crate::{session_storage::SessionStorage, AgentConfig};
use noland_observer::QueuedEvent;
use noland_storage::{
    read_committed_manifest, read_pack_index, Health, LocalStorage, ProviderRootIdentity,
    RemoteEntry, RemoteKey, RemoteMeta,
};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const CHILD_ROOT: &str = "NOLAND_FROZEN_RPC_TEST_ROOT";
const CHILD_CASE: &str = "NOLAND_FROZEN_RPC_TEST_CASE";
const CHILD_TEST: &str = "rpc_handler::frozen_retry_tests::local_fake_rclone_child";
const OLD_SAVE: &[u8] = b"captured old save";
const LIVE_SAVE: &[u8] = b"later live save must not be overwritten";

// All operands and the explicit local-only config are checked before any I/O.
// Unknown commands fail closed; there is no fallback to a real rclone binary.
const FAKE_RCLONE: &str = r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import shutil
import sys

root = Path(os.environ["NOLAND_FROZEN_RPC_TEST_ROOT"]).resolve(strict=True)
repository = root / "repository"

def reject(message):
    print("permission denied: local fixture " + message, file=sys.stderr)
    sys.exit(1)

def within(path, boundary):
    path = Path(path).resolve()
    try:
        path.relative_to(boundary)
    except ValueError:
        reject("path escaped its owned directory")
    return path

def operand(value):
    if value.startswith("fixture:"):
        return within(value[len("fixture:"):], repository)
    if ":" in value or not Path(value).is_absolute():
        reject("non-local operand")
    return within(value, root)

def remote(value):
    if not value.startswith("fixture:"):
        reject("remote is not the fixture")
    return operand(value)

def missing():
    print("object not found", file=sys.stderr)
    sys.exit(3)

args = sys.argv[1:]
if len(args) < 4 or args[0] != "--config":
    reject("explicit configuration required")
config = within(args[1], root)
if config.read_text() != "[fixture]\ntype = local\n":
    reject("configuration is not local-only")
method = args[2]
args = args[3:]
value_options = {"--files-from-raw", "--checkers", "--transfers", "--buffer-size",
                 "--retries", "--low-level-retries", "--stats", "--log-level",
                 "--multi-thread-streams"}
switches = {"--no-traverse", "--immutable", "--stats-one-line", "--use-json-log",
            "--no-modtime", "--no-mimetype", "--stat", "-R", "--error-on-no-transfer"}
operands = []
index = 0
while index < len(args):
    arg = args[index]
    if arg in value_options:
        if index + 1 >= len(args):
            reject("missing fixture option value")
        index += 2
        continue
    if arg in switches:
        index += 1
        continue
    if arg.startswith("-"):
        reject("unsupported fixture option " + arg)
    operands.append(arg)
    index += 1
expected_operands = 2 if method in ("copy", "copyto") else 1
if len(operands) != expected_operands:
    reject("unexpected fixture operand count")
with (root / "rclone-commands.jsonl").open("a") as log:
    log.write(json.dumps({"method": method}) + "\n")

def copied(source, destination, name):
    source = within(source, root)
    destination = within(destination, repository)
    data = source.read_bytes()
    if "--immutable" in args and destination.exists():
        if destination.read_bytes() != data:
            reject("immutable object changed")
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(data)
    print(json.dumps({"level":"info","object":name,"msg":"Copied (new)"}), file=sys.stderr)

if method == "mkdir":
    remote(operands[-1]).mkdir(parents=True, exist_ok=True)
elif method == "lsjson":
    path = remote(operands[-1])
    if not path.exists():
        missing()
    if "--stat" in args:
        print(json.dumps({"Path":path.name,"Size":path.stat().st_size,"IsDir":path.is_dir()}))
    else:
        entries = []
        for child in sorted(path.rglob("*")):
            child = within(child, repository)
            entries.append({"Path":child.relative_to(path).as_posix(),
                            "Size":0 if child.is_dir() else child.stat().st_size,
                            "IsDir":child.is_dir()})
        print(json.dumps(entries))
elif method == "cat":
    path = remote(operands[-1])
    if "--files-from-raw" in args:
        keys = sys.stdin.read().splitlines()
        if len(keys) != 1 or Path(keys[0]).is_absolute() or ".." in Path(keys[0]).parts:
            reject("cat requires one literal object")
        path = within(path / keys[0], repository)
    if not path.is_file():
        missing()
    sys.stdout.buffer.write(path.read_bytes())
elif method in ("copy", "copyto"):
    source = operand(operands[-2])
    destination = operand(operands[-1])
    if not operands[-1].startswith("fixture:"):
        # The replay normally needs no download; keep any explicit local
        # download confined to this helper's owned directory as well.
        if not operands[-2].startswith("fixture:") or not source.is_file():
            reject("invalid local download")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
    elif method == "copy":
        if not source.is_dir():
            reject("copy source is not a staging directory")
        for child in sorted(source.rglob("*")):
            if child.is_file():
                relative = child.relative_to(source)
                copied(child, destination / relative, relative.as_posix())
    else:
        copied(source, destination, destination.relative_to(repository).as_posix())
elif method == "deletefile":
    path = remote(operands[-1])
    if not path.is_file():
        missing()
    path.unlink()
elif method == "rmdir":
    remote(operands[-1]).rmdir()
else:
    reject("unsupported command " + method)
"#;

struct OwnedRoot(PathBuf);

impl Drop for OwnedRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_isolated(case: &str) {
    let root = std::env::temp_dir().join(format!("frozen-rpc-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let root = OwnedRoot(fs::canonicalize(root).unwrap());
    let bin = root.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("rclone");
    fs::write(&executable, FAKE_RCLONE).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let temporary = root.0.join("tmp");
    fs::create_dir(&temporary).unwrap();
    let python = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join("python3"))
        .find(|path| path.is_file())
        .expect("local fake-rclone fixture requires Python 3");
    symlink(fs::canonicalize(python).unwrap(), bin.join("python3")).unwrap();
    // Like the CAS restart fixture, run a copy of the TEST harness. Only this
    // child inherits the fake PATH; concurrently running tests are unaffected.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD_ROOT, &root.0)
        .env(CHILD_CASE, case)
        // No inherited PATH entries: even an executable lookup failure cannot
        // fall through to a real rclone installation.
        .env("PATH", bin)
        .env("TMPDIR", temporary)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "local RPC fixture {case} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn coherent_frozen_rpc_retry_ignores_invalid_live_registration_and_preserves_pending_gap() {
    run_isolated("coherent");
}

#[test]
fn corrupt_prepared_rpc_retry_cannot_recapture_or_publish() {
    run_isolated("corrupt");
}

#[test]
fn wrong_repository_rpc_retry_is_rejected_before_journal_reset() {
    run_isolated("wrong-repository");
}

struct InterruptedPublication {
    inner: LocalStorage,
    fail_manifest: AtomicBool,
}

#[async_trait]
impl SharedStorageProvider for InterruptedPublication {
    async fn health_check(&self) -> Result<Health> {
        self.inner.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.inner.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.inner.stat(key).await
    }
    async fn read_small(&self, key: &RemoteKey, limit: u64) -> Result<Vec<u8>> {
        self.inner.read_small(key, limit).await
    }
    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.inner.upload_immutable(local, key).await
    }
    async fn download(&self, key: &RemoteKey, destination: &Path) -> Result<()> {
        self.inner.download(key, destination).await
    }
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.inner.list_prefix(key).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &RemoteKey,
    ) -> Result<RemoteMeta> {
        if key.as_str().ends_with("manifest.enc")
            && self.fail_manifest.swap(false, Ordering::SeqCst)
        {
            return Err(StateError::Storage(
                "fixture interruption after coherent capture".into(),
            ));
        }
        self.inner.put_small_versioned(bytes, key).await
    }
    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        self.inner.delete_object(key).await
    }
    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        self.inner.storage_identity()
    }
}

async fn rpc(agent: &Arc<StateAgent>, method: &str, params: Value) -> Result<Value> {
    AgentRpc(agent.clone())
        .handle(&RpcRequest {
            id: Uuid::new_v4().to_string(),
            method: method.into(),
            params,
        })
        .await
}

fn key_hex(master: &MasterKey) -> String {
    master.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn exercise(root: PathBuf, case: &str) {
    let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
    let app = AppId::desktop("frozen-rpc-fixture");
    let mut identity = AppIdentity::new(app.clone(), "Frozen RPC Fixture");
    identity.canonical_executable = Some(agent.config.home.join("Games/Frozen/game"));
    agent.db.upsert_app(&identity).unwrap();
    let save = agent.config.home.join("Games/Frozen/save.dat");
    fs::create_dir_all(save.parent().unwrap()).unwrap();
    fs::write(&save, OLD_SAVE).unwrap();
    agent
        .db
        .add_known_root(&app, "install", save.parent().unwrap().to_str().unwrap())
        .unwrap();
    let session = EphemeralRcloneSession {
        operation_id: Uuid::new_v4().to_string(),
        provider: "local".into(),
        backend_type: "local".into(),
        remote_name: "fixture".into(),
        root: root.join("repository").to_str().unwrap().into(),
        config_ini: "[fixture]\ntype = local\n".into(),
        expires_at_unix: 0,
        upload_concurrency: None,
    };
    let master = MasterKey::generate();
    let id = Uuid::new_v4();
    let selection = json!({
        "app_id":app,"mode":"complete_application","performance_mode":"balanced",
        "regenerable_caches":"include",
    });
    let mut original = selection.clone();
    original["client_operation_id"] = json!(id);
    original["session"] = serde_json::to_value(&session).unwrap();
    original["master_key_hex"] = json!(key_hex(&master));
    original["client_storage_identity"] = json!({
        "provider":session.provider,"backend_type":session.backend_type,
        "remote_name":session.remote_name,"root":session.root,
    });
    let submission::Claim::New {
        operation_id,
        binding,
    } = submission::resolve(
        &agent.db, &original, &session, &master, "backup", &selection, None,
    )
    .unwrap()
    else {
        panic!("fixture must start with a new bound submission")
    };
    assert_eq!(operation_id, id);
    upsert_queued_operation(
        &agent,
        id,
        "backup",
        Some(app.clone()),
        "QUEUED",
        submission::bind_details(selection, id, binding),
        false,
    )
    .unwrap();
    remember_retry_request(&agent, id, "StartBackup", &original).unwrap();
    let interrupted = SessionStorage::new(
        InterruptedPublication {
            inner: LocalStorage::new(root.join("repository")),
            fail_manifest: AtomicBool::new(true),
        },
        &agent,
        &session,
        &master,
    );
    let first = crate::backup::run_backup(
        &agent,
        &app,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &interrupted,
        &master,
        Some(id),
    )
    .await
    .unwrap_err();
    assert!(first
        .to_string()
        .contains("interruption after coherent capture"));
    drop(interrupted);
    mark_operation_failed(&agent, id, first.to_string(), Instant::now());
    let checkpoint = agent
        .config
        .paths
        .packs
        .join("resume")
        .join(format!("{id}.enc"));
    let checkpoint_bytes = fs::read(&checkpoint).unwrap();
    let prepared: Value = noland_storage::open_metadata(
        &noland_crypto::derive_keys(&master).manifest,
        format!("backup-resume:{id}").as_bytes(),
        checkpoint_bytes.clone(),
    )
    .unwrap();
    assert!(
        prepared["capture"].is_null(),
        "fixture must own a coherent captured revision"
    );
    let captured: BundleManifest = serde_json::from_value(prepared["manifest"].clone()).unwrap();
    assert_eq!(captured.files.len(), 1);
    assert_eq!(
        captured.files[0].file_hash,
        noland_cas::blake3_hex(OLD_SAVE)
    );
    let before = agent.db.get_operation(id).unwrap().unwrap();
    assert_eq!(before.state, "FAILED");
    let journal =
        serde_json::to_value(agent.db.list_sync_journal_entries(id, None).unwrap()).unwrap();

    // Reuse the collector's failing-live-flush fixture, but actually cross the
    // RPC acceptance/managed-worker boundary for the retry below.
    fs::write(&save, LIVE_SAVE).unwrap();
    let pid = i32::MAX - 31;
    agent
        .db
        .insert_session(&AppSession::new(
            app.clone(),
            pid,
            SessionSource::ExecutableDiscovery,
        ))
        .unwrap();
    let registry = agent.config.home.join(".local/share/noland-runtime/apps");
    fs::create_dir_all(&registry).unwrap();
    let invalid_record = registry.join("malformed.json");
    fs::write(&invalid_record, b"{invalid live registration").unwrap();
    assert!(noland_discovery::runtime_backup_binding(&agent.config.home, &identity).is_err());
    agent.hub.report_filesystem_gap(pid);
    assert!(
        agent.process_events().is_err(),
        "the old RPC prelude must be fatal for this live state"
    );
    let loss = agent
        .hub
        .queue
        .loss_snapshot()
        .expect("failed gap must remain sticky");
    let loss_generation = agent.observer.loss_generation();
    let roots = agent.db.known_roots(Some(&app)).unwrap();
    let required = agent.db.runtime_binding_required(&app).unwrap();
    let mut retry_session = session.clone();
    retry_session.operation_id = Uuid::new_v4().to_string();
    let mut retry = json!({
        "operation_id":id,"session":retry_session,"master_key_hex":key_hex(&master),
    });
    match case {
        "wrong-repository" => {
            retry["session"]["root"] = json!(root.join("another-repository"));
            let error = rpc(&agent, "RetryOperation", retry).await.unwrap_err();
            assert!(error.to_string().contains("client_storage_identity"));
            let unchanged = agent.db.get_operation(id).unwrap().unwrap();
            assert_eq!(unchanged.state, before.state);
            assert_eq!(unchanged.detail_json, before.detail_json);
            assert_eq!(
                serde_json::to_value(agent.db.list_sync_journal_entries(id, None).unwrap())
                    .unwrap(),
                journal
            );
            assert_eq!(fs::read(&checkpoint).unwrap(), checkpoint_bytes);
            assert!(!root.join("another-repository").exists());
            assert!(
                !root.join("rclone-commands.jsonl").exists(),
                "rejected identity cannot start adapter I/O"
            );
        }
        "coherent" | "corrupt" => {
            let corrupt_bytes = if case == "corrupt" {
                let mut bytes = checkpoint_bytes.clone();
                *bytes.last_mut().unwrap() ^= 1;
                fs::write(&checkpoint, &bytes).unwrap();
                Some(bytes)
            } else {
                None
            };
            let accepted = rpc(&agent, "RetryOperation", retry).await.unwrap();
            assert_eq!(accepted["operation_id"], id.to_string());
            assert_eq!(accepted["retried_operation_id"], id.to_string());
            assert_eq!(accepted["retried"], true);
            tokio::time::timeout(Duration::from_secs(60), async {
                while agent.operations.is_running(id) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the local managed retry must drain");
            let finished = agent.db.get_operation(id).unwrap().unwrap();
            assert_eq!(finished.operation_id, id);
            assert_eq!(finished.created_at, before.created_at);
            assert_eq!(
                finished.detail_json["submission_binding"],
                before.detail_json["submission_binding"]
            );
            let local = LocalStorage::new(root.join("repository"));
            if let Some(corrupt_bytes) = corrupt_bytes {
                assert_eq!(finished.state, "FAILED");
                assert!(
                    finished
                        .last_error
                        .as_deref()
                        .unwrap_or_default()
                        .contains("decrypt/authenticate failed"),
                    "{:?}",
                    finished.last_error
                );
                assert_eq!(
                    fs::read(&checkpoint).unwrap(),
                    corrupt_bytes,
                    "bad receipt must not trigger recapture"
                );
                assert!(agent.db.latest_commit(&app).unwrap().is_none());
                assert!(
                    read_committed_manifest(&local, &master, &app, captured.bundle_id)
                        .await
                        .is_err()
                );
            } else {
                assert_eq!(finished.state, "COMPLETED", "{:?}", finished.last_error);
                assert_eq!(
                    finished.detail_json["bundle_id"],
                    captured.bundle_id.to_string()
                );
                let committed = read_committed_manifest(&local, &master, &app, captured.bundle_id)
                    .await
                    .unwrap();
                assert_eq!(committed.bundle_id, captured.bundle_id);
                assert_eq!(committed.commit_id, captured.commit_id);
                assert_eq!(
                    committed.files[0].file_hash,
                    noland_cas::blake3_hex(OLD_SAVE)
                );
                let index = read_pack_index(&local, &master, &app, captured.bundle_id)
                    .await
                    .unwrap();
                let mut captured_bytes = Vec::new();
                for chunk in &committed.files[0].chunks {
                    let entry = index
                        .iter()
                        .find(|entry| entry.chunk_hash == chunk.hash)
                        .unwrap();
                    captured_bytes.extend(
                        noland_pack::extract_chunk(
                            &local.root.join(pack_key(&entry.pack_id)),
                            entry,
                            &master,
                        )
                        .unwrap(),
                    );
                }
                assert_eq!(captured_bytes.as_slice(), OLD_SAVE);
                assert!(!checkpoint.exists());
                assert!(agent
                    .db
                    .list_file_states(&app, None)
                    .unwrap()
                    .iter()
                    .all(|state| state.trust != FileStateTrust::Trusted));
                let replay = rpc(&agent, "StartBackup", original).await.unwrap();
                assert_eq!(replay["operation_id"], id.to_string());
                assert_eq!(replay["state"], "COMPLETED");
                assert_eq!(
                    replay["detail_json"]["bundle_id"],
                    captured.bundle_id.to_string()
                );
            }
            assert!(
                root.join("rclone-commands.jsonl").is_file(),
                "RPC must exercise the actual session adapter"
            );
        }
        _ => panic!("unknown fixture case"),
    }
    assert!(!agent.operations.is_running(id));
    assert_eq!(fs::read(save).unwrap(), LIVE_SAVE);
    assert_eq!(
        fs::read(invalid_record).unwrap(),
        b"{invalid live registration"
    );
    assert_eq!(
        agent.db.known_roots(Some(&app)).unwrap(),
        roots,
        "retry cannot invent new authority"
    );
    assert_eq!(agent.db.runtime_binding_required(&app).unwrap(), required);
    assert!(noland_discovery::runtime_backup_binding(&agent.config.home, &identity).is_err());
    assert_eq!(
        agent.hub.queue.loss_snapshot().unwrap().dropped,
        loss.dropped
    );
    assert_eq!(agent.observer.loss_generation(), loss_generation);
    assert!(matches!(
        agent.hub.queue.drain().as_slice(),
        [QueuedEvent::FilesystemGap { pid: retained }] if *retained == pid
    ));
}

#[test]
fn local_fake_rclone_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let case = std::env::var(CHILD_CASE).expect("fixture case is required");
    let root = fs::canonicalize(root).unwrap();
    assert!(root.join("bin/rclone").is_file());
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
        .block_on(exercise(root, &case));
}
