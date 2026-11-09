use crate::policy::{self, Config, Spec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;

pub type ResultValue = Result<Value, String>;
pub const OUTPUT_LIMIT: usize = 262_144;
const HISTORY_LIMIT: usize = 200;
const CACHE_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct Execution {
    pub config: Config,
    pub job_id: String,
    pub cancelled: CancellationToken,
    pub output: Arc<RwLock<String>>,
}

impl Execution {
    pub async fn log(&self, message: &str) {
        let mut output = self.output.write().await;
        output.push_str(message);
        if output.len() > OUTPUT_LIMIT {
            let mut cut = output.len() - OUTPUT_LIMIT;
            while !output.is_char_boundary(cut) {
                cut += 1;
            }
            output.drain(..cut);
        }
    }
}

#[async_trait]
pub trait Backend: Send + Sync + 'static {
    fn specs(&self) -> Vec<Spec>;
    async fn execute(
        &self,
        tool: &str,
        args: Map<String, Value>,
        execution: Execution,
    ) -> ResultValue;
    async fn job_control(&self, _job: &Job, _input: Option<String>) -> ResultValue {
        Err("This operation does not support interactive input or direct cancellation".into())
    }
    async fn sanitize(&self, value: Value) -> Value {
        value
    }
    async fn approval_preview(&self, _tool: &str, args: &Map<String, Value>) -> ResultValue {
        Ok(Value::Object(args.clone()))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub tool: String,
    pub instance_id: Option<u64>,
    pub as_root: bool,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub result: Option<Value>,
    pub error: Option<String>,
    #[serde(default, rename = "requestIdHash", alias = "requestId")]
    pub request_id: Option<String>,
    #[serde(default)]
    pub argument_hash: Option<String>,
}

impl Job {
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running" | "cancelling")
    }
    fn metadata(&self) -> Self {
        let mut job = self.clone();
        job.result = None;
        job
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: String,
    pub tool: String,
    pub instance_id: Option<u64>,
    pub arguments: Value,
    pub status: String,
    pub created_at: String,
    #[serde(skip)]
    hash: String,
    #[serde(skip)]
    generation: u64,
}

struct Running {
    job: Job,
    cancel: CancellationToken,
    output: Arc<RwLock<String>>,
    generation: u64,
}

pub struct Core {
    pub backend: Arc<dyn Backend>,
    pub config: RwLock<Config>,
    pub token_hash: RwLock<[u8; 32]>,
    pub generation: AtomicU64,
    // Configuration/revocation and dispatch admission use the same gate. Work
    // already dispatched is cancelled best-effort, never described as rolled back.
    admission: RwLock<()>,
    jobs: RwLock<BTreeMap<String, Running>>,
    approvals: Mutex<Vec<Approval>>,
    requests: Mutex<BTreeMap<String, (String, Value)>>,
    pub clients: RwLock<BTreeMap<String, Value>>,
    events: RwLock<VecDeque<Value>>,
    sequence: AtomicU64,
    slots: Arc<Semaphore>,
    directory: PathBuf,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl Core {
    pub fn new(
        backend: Arc<dyn Backend>,
        config: Config,
        token: &str,
        directory: PathBuf,
    ) -> Arc<Self> {
        let mut jobs = BTreeMap::new();
        let mut requests = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(directory.join("jobs")) {
            for entry in entries.flatten().take(HISTORY_LIMIT) {
                if !entry.file_type().is_ok_and(|t| t.is_file())
                    || !entry.metadata().is_ok_and(|m| m.len() <= 32_768)
                {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(entry.path()) {
                    if let Ok(mut job) = serde_json::from_slice::<Job>(&bytes) {
                        if uuid::Uuid::parse_str(&job.id).is_err() {
                            continue;
                        }
                        if job.active() {
                            job.status = "interrupted".into();
                            job.error = Some("App restarted. Remote work may still be running; inspect before retrying.".into());
                        }
                        // File contents, images, logs and raw commands are never persisted.
                        job.result = None;
                        if let (Some(key), Some(hash)) = (&job.request_id, &job.argument_hash) {
                            requests.insert(
                                key.clone(),
                                (hash.clone(), json!({"jobId":job.id,"status":job.status})),
                            );
                        }
                        jobs.insert(
                            job.id.clone(),
                            Running {
                                job,
                                cancel: CancellationToken::new(),
                                output: Arc::new(RwLock::new(String::new())),
                                generation: 0,
                            },
                        );
                    }
                }
            }
        }
        let mut events = VecDeque::new();
        let mut sequence = 0;
        let audit = directory.join("audit.jsonl");
        if std::fs::symlink_metadata(&audit).is_ok_and(|m| m.is_file() && m.len() <= 2_100_000) {
            if let Ok(text) = std::fs::read_to_string(audit) {
                for line in text
                    .lines()
                    .rev()
                    .take(HISTORY_LIMIT)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    if let Ok(value) = serde_json::from_str::<Value>(line) {
                        sequence = sequence.max(value["sequence"].as_u64().unwrap_or(0));
                        events.push_back(value);
                    }
                }
            }
        }
        Arc::new(Self {
            backend,
            config: RwLock::new(config),
            token_hash: RwLock::new(policy::digest(token.as_bytes())),
            generation: AtomicU64::new(1),
            admission: RwLock::new(()),
            jobs: RwLock::new(jobs),
            approvals: Mutex::new(vec![]),
            requests: Mutex::new(requests),
            clients: RwLock::new(BTreeMap::new()),
            events: RwLock::new(events),
            sequence: AtomicU64::new(sequence),
            slots: Arc::new(Semaphore::new(4)),
            directory,
        })
    }

    pub async fn event(&self, kind: &str, details: Value) {
        let details = self.backend.sanitize(details).await;
        let mut events = self.events.write().await;
        let value = json!({"sequence":self.sequence.fetch_add(1, Ordering::SeqCst)+1,"at":now(),"kind":kind,"details":details});
        events.push_back(value.clone());
        if events.len() > HISTORY_LIMIT {
            events.pop_front();
        }
        // Audit only metadata, with bounded on-disk rotation and owner-only access.
        if private_directory(&self.directory).is_ok() {
            use std::io::Write;
            let path = self.directory.join("audit.jsonl");
            if path.metadata().is_ok_and(|m| m.len() > 2_000_000) {
                let _ = std::fs::rename(&path, self.directory.join("audit.previous.jsonl"));
            }
            let mut options = std::fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            if let Ok(mut file) = options.open(path) {
                let _ = writeln!(file, "{value}");
            }
        }
    }

    pub async fn recent_events(&self, after: u64) -> Vec<Value> {
        self.events
            .read()
            .await
            .iter()
            .filter(|v| v["sequence"].as_u64().unwrap_or(0) > after)
            .cloned()
            .collect()
    }

    fn persist(&self, job: &Job) -> std::io::Result<()> {
        let directory = self.directory.join("jobs");
        private_directory(&directory)?;
        let path = directory.join(format!("{}.json", job.id));
        let temp = directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec(&job.metadata()).map_err(std::io::Error::other)?;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let result = (|| {
            let mut file = options.open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, path)
        })();
        let _ = std::fs::remove_file(temp);
        result
    }

    pub async fn approvals(&self) -> Vec<Approval> {
        self.approvals
            .lock()
            .await
            .iter()
            .filter(|a| self.fresh(a))
            .cloned()
            .collect()
    }

    fn fresh(&self, a: &Approval) -> bool {
        a.generation == self.generation.load(Ordering::SeqCst)
            && chrono::DateTime::parse_from_rfc3339(&a.created_at).is_ok_and(|t| {
                (0..300).contains(&chrono::Utc::now().signed_duration_since(t).num_seconds())
            })
    }

    pub async fn decide(&self, id: &str, approve: bool) -> Result<(), String> {
        let _admission = self.admission.read().await;
        let mut approvals = self.approvals.lock().await;
        let approval = approvals
            .iter_mut()
            .find(|a| a.id == id && self.fresh(a) && a.status == "pending")
            .ok_or("Approval is stale or already answered")?;
        approval.status = if approve { "approved" } else { "denied" }.into();
        let metadata = json!({"approvalId":id,"tool":approval.tool,"instanceId":approval.instance_id,"approved":approve});
        drop(approvals);
        self.event("approval_decided", metadata).await;
        Ok(())
    }

    /// Invalidate every session/approval and redact cached data immediately.
    /// Cancellation does not imply rollback or destruction of remote processes.
    pub async fn revoke(&self) {
        let _admission = self.admission.write().await;
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.clients.write().await.clear();
        self.approvals.lock().await.clear();
        for item in self.jobs.write().await.values_mut() {
            item.job.result = None;
            item.output.write().await.clear();
            if item.job.active() {
                item.cancel.cancel();
            }
        }
    }

    pub async fn active_jobs(&self) -> usize {
        self.jobs
            .read()
            .await
            .values()
            .filter(|j| j.job.active())
            .count()
    }

    pub async fn call(self: &Arc<Self>, tool: &str, args: Map<String, Value>) -> ResultValue {
        let generation = self.generation.load(Ordering::SeqCst);
        self.call_for_generation(tool, args, generation).await
    }

    pub async fn call_for_generation(
        self: &Arc<Self>,
        tool: &str,
        args: Map<String, Value>,
        generation: u64,
    ) -> ResultValue {
        let result = self.dispatch(tool, args, generation).await;
        if generation != self.generation.load(Ordering::SeqCst) {
            return Err("The connection or permissions changed; reconnect and inspect state before retrying".into());
        }
        match result {
            Ok(value) => Ok(self.backend.sanitize(value).await),
            Err(error) => Err(self
                .backend
                .sanitize(json!(error))
                .await
                .as_str()
                .unwrap_or("Operation failed")
                .to_owned()),
        }
    }

    async fn dispatch(
        self: &Arc<Self>,
        tool: &str,
        mut args: Map<String, Value>,
        generation: u64,
    ) -> ResultValue {
        let admission = self.admission.read().await;
        if generation != self.generation.load(Ordering::SeqCst) {
            return Err("Agent session was disconnected; reconnect".into());
        }
        let config = self.config.read().await.clone();
        if !config.enabled {
            return Err("MCP is disabled".into());
        }
        if tool == "app_events" {
            policy::validate_args(
                &json!({"properties":{"after":{"type":"integer","minimum":0}}})
                    .as_object()
                    .unwrap()
                    .clone(),
                &args,
            )?;
            let after = args.get("after").and_then(Value::as_u64).unwrap_or(0);
            let events = self
                .recent_events(after)
                .await
                .into_iter()
                .filter(|v| {
                    v["details"]["instanceId"]
                        .as_u64()
                        .is_none_or(|id| config.allows_instance(id))
                })
                .collect::<Vec<_>>();
            return Ok(json!({"events":events}));
        }
        if tool.starts_with("job_") || tool == "jobs_list" {
            // Revocation must not wait for a remote job_control round-trip.
            drop(admission);
            return self.job_call(tool, args, &config, generation).await;
        }
        let spec = self
            .backend
            .specs()
            .into_iter()
            .find(|s| s.name == tool)
            .ok_or("Unknown tool")?;
        spec.authorize(&config, &args)?;
        let hash = policy::argument_hash(tool, &args);
        let mut requests = self.requests.lock().await;
        let request_id = args
            .get("request_id")
            .and_then(Value::as_str)
            .map(policy::fingerprint);
        if let Some(key) = &request_id {
            if let Some((previous, result)) = requests.get(key) {
                return if previous == &hash {
                    Ok(result.clone())
                } else {
                    Err("request_id was already used for different arguments".into())
                };
            }
            if requests.len() >= 1000 {
                return Err(
                    "Idempotency history is full; restart the app or inspect existing jobs".into(),
                );
            }
        }
        if spec.approval {
            let id = args.get("approval_id").and_then(Value::as_str);
            let mut approvals = self.approvals.lock().await;
            approvals.retain(|a| self.fresh(a));
            if let Some(id) = id {
                let a = approvals
                    .iter_mut()
                    .find(|a| a.id == id && a.hash == hash)
                    .ok_or("Approval is stale or does not match these exact arguments")?;
                if a.status != "approved" {
                    return Err(format!(
                        "Approval is {}. Ask the user in Noland Connect.",
                        a.status
                    ));
                }
                a.status = "consumed".into();
            } else {
                if let Some(a) = approvals
                    .iter()
                    .find(|a| a.hash == hash && matches!(a.status.as_str(), "pending" | "approved"))
                {
                    return Ok(
                        json!({"status":"needs_approval","approvalId":a.id,"decision":a.status}),
                    );
                }
                if approvals.len() >= 50 {
                    return Err("Too many pending/recent approvals".into());
                }
                let a = Approval {
                    id: uuid::Uuid::new_v4().to_string(),
                    tool: tool.into(),
                    instance_id: args.get("instance_id").and_then(Value::as_u64),
                    arguments: self
                        .backend
                        .sanitize(self.backend.approval_preview(tool, &args).await?)
                        .await,
                    status: "pending".into(),
                    created_at: now(),
                    hash: hash.clone(),
                    generation,
                };
                let value = json!({"status":"needs_approval","approvalId":a.id,"message":"Approve this exact operation in Noland Connect, then retry with approval_id. The agent cannot approve itself."});
                approvals.push(a);
                return Ok(value);
            }
        }
        args.remove("request_id");
        args.remove("approval_id");
        self.event(
            "tool_requested",
            json!({"tool":tool,"instanceId":args.get("instance_id")}),
        )
        .await;
        let execution = Execution {
            config,
            job_id: uuid::Uuid::new_v4().to_string(),
            cancelled: CancellationToken::new(),
            output: Arc::new(RwLock::new(String::new())),
        };
        if !spec.background {
            // Only fast/idempotent context, navigation and stop requests are inline.
            drop(requests);
            drop(admission);
            return self.backend.execute(tool, args, execution).await;
        }
        let mut jobs = self.jobs.write().await;
        if jobs.values().filter(|j| j.job.active()).count() >= 16 {
            return Err("Too many active operations".into());
        }
        while jobs.len() >= HISTORY_LIMIT {
            let oldest = jobs
                .iter()
                .filter(|(_, r)| !r.job.active())
                .min_by_key(|(_, r)| r.job.started_at.clone())
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                jobs.remove(&id);
                let _ =
                    std::fs::remove_file(self.directory.join("jobs").join(format!("{id}.json")));
            } else {
                break;
            }
        }
        let job = Job {
            id: execution.job_id.clone(),
            tool: tool.into(),
            instance_id: args.get("instance_id").and_then(Value::as_u64),
            as_root: args
                .get("as_root")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            status: "queued".into(),
            started_at: now(),
            finished_at: None,
            result: None,
            error: None,
            request_id: request_id.clone(),
            argument_hash: Some(hash.clone()),
        };
        self.persist(&job).map_err(|e| {
            format!("Cannot persist job metadata; no operation was dispatched: {e}")
        })?;
        jobs.insert(
            job.id.clone(),
            Running {
                job: job.clone(),
                cancel: execution.cancelled.clone(),
                output: execution.output.clone(),
                generation,
            },
        );
        drop(jobs);
        let value = json!({"jobId":job.id,"status":"queued"});
        if let Some(key) = request_id {
            requests.insert(key, (hash, value.clone()));
        }
        drop(requests);
        drop(admission);
        let this = self.clone();
        let tool = tool.to_owned();
        tokio::spawn(async move {
            let permit = tokio::select! {
                permit = this.slots.clone().acquire_owned() => permit.ok(),
                _ = execution.cancelled.cancelled() => None,
            };
            let admission = this.admission.read().await;
            let admitted = if permit.is_none()
                || execution.cancelled.is_cancelled()
                || generation != this.generation.load(Ordering::SeqCst)
            {
                false
            } else {
                if let Some(r) = this.jobs.write().await.get_mut(&job.id) {
                    r.job.status = "running".into();
                    let _ = this.persist(&r.job);
                }
                true
            };
            drop(admission);
            let result = if admitted {
                this.backend.execute(&tool, args, execution.clone()).await
            } else {
                Err("Operation cancelled before execution".into())
            };
            drop(permit);
            let result = match result {
                Ok(value) => Ok(this.backend.sanitize(value).await),
                Err(error) => Err(this
                    .backend
                    .sanitize(json!(error))
                    .await
                    .as_str()
                    .unwrap_or("Operation failed")
                    .to_owned()),
            };
            let mut jobs = this.jobs.write().await;
            let mut final_status = "interrupted".to_string();
            if let Some(r) = jobs.get_mut(&job.id) {
                r.job.finished_at = Some(now());
                match result {
                    Ok(value) => {
                        r.job.status = "completed".into();
                        if generation == this.generation.load(Ordering::SeqCst) {
                            r.job.result = Some(if value.to_string().len() <= 4 * 1024 * 1024 {
                                value
                            } else {
                                json!({"message":"Result exceeded the 4 MiB limit; inspect the remote operation directly"})
                            });
                        }
                    }
                    Err(error) => {
                        r.job.status = if execution.cancelled.is_cancelled() {
                            "cancelled"
                        } else {
                            "failed"
                        }
                        .into();
                        r.job.error = Some(error.chars().take(2_048).collect());
                    }
                }
                if execution.cancelled.is_cancelled() {
                    r.job.error.get_or_insert_with(|| "Cancellation was requested. Remote effects may remain; completion does not imply rollback.".into());
                }
                final_status = r.job.status.clone();
                if this.persist(&r.job).is_err() {
                    r.job.error.get_or_insert_with(||"Completion metadata could not be saved. Inspect the remote outcome before retrying.".into());
                }
            }
            // Bound cached payloads, not only the number of jobs.
            let mut bytes = 0;
            let mut order = jobs.keys().cloned().collect::<Vec<_>>();
            order.sort_by_key(|id| std::cmp::Reverse(jobs[id].job.started_at.clone()));
            for id in order {
                let r = jobs.get_mut(&id).unwrap();
                bytes += r.job.result.as_ref().map_or(0, |v| v.to_string().len());
                if bytes > CACHE_LIMIT && !r.job.active() {
                    r.job.result = None;
                }
            }
            drop(jobs);
            this.event("job_finished", json!({"jobId":job.id,"tool":tool,"instanceId":job.instance_id,"status":final_status})).await;
        });
        Ok(value)
    }

    async fn job_call(
        &self,
        tool: &str,
        args: Map<String, Value>,
        config: &Config,
        generation: u64,
    ) -> ResultValue {
        if tool == "jobs_list" {
            policy::validate_args(json!({"properties":{}}).as_object().unwrap(), &args)?;
            let jobs = self.jobs.read().await;
            let mut visible = jobs
                .values()
                .filter(|r| r.job.instance_id.is_none_or(|i| config.allows_instance(i)))
                .map(|r| r.job.metadata())
                .collect::<Vec<_>>();
            visible.sort_by_key(|j| std::cmp::Reverse(j.started_at.clone()));
            visible.truncate(50);
            return Ok(json!(visible));
        }
        let mut properties = json!({"job_id":{"type":"string","maxLength":80}});
        let mut required = vec!["job_id"];
        if tool == "job_input" {
            properties["input"] = json!({"type":"string","maxLength":65536});
            required.push("input");
        }
        policy::validate_args(
            json!({"properties":properties,"required":required})
                .as_object()
                .unwrap(),
            &args,
        )?;
        let id = args["job_id"].as_str().ok_or("job_id required")?;
        let (job, cancel, output, job_generation) = {
            let jobs = self.jobs.read().await;
            let r = jobs.get(id).ok_or("Unknown job")?;
            (
                r.job.clone(),
                r.cancel.clone(),
                r.output.clone(),
                r.generation,
            )
        };
        if job.instance_id.is_some_and(|i| !config.allows_instance(i)) {
            return Err("Access to this job's instance is not granted".into());
        }
        let scope = self
            .backend
            .specs()
            .into_iter()
            .find(|s| s.name == job.tool)
            .map(|s| s.scope)
            .ok_or("Unknown job tool")?;
        if !scope.allowed(config) || (job.as_root && !config.elevated) {
            return Err("The original job capability is no longer granted".into());
        }
        match tool {
            "job_get" => Ok(json!(if job_generation == generation {
                job
            } else {
                job.metadata()
            })),
            "job_logs" => Ok(
                json!({"output":if job_generation == generation { output.read().await.clone() } else { String::new() },"boundedTailBytes":OUTPUT_LIMIT}),
            ),
            "job_input" => {
                if job.tool != "ssh_exec" || job.status != "running" || job_generation != generation
                {
                    return Err("Only a running interactive SSH job accepts input".into());
                }
                self.backend
                    .job_control(&job, Some(args["input"].as_str().unwrap().to_owned()))
                    .await
            }
            "job_cancel" => {
                if !job.active() && !(job.tool == "ssh_exec" && job.status == "interrupted") {
                    return Ok(
                        json!({"status":"already_finished","message":"This job has finished; no other operation was cancelled."}),
                    );
                }
                // Read jobs can also be cancelled by their owner, without granting
                // unrelated machine-control permission.
                cancel.cancel();
                if let Some(r) = self.jobs.write().await.get_mut(id) {
                    if r.job.active() {
                        r.job.status = "cancelling".into();
                    }
                }
                let remote = if job.status == "queued" {
                    json!({"status":"cancelled_before_dispatch"})
                } else if job.active() {
                    json!({"status":"cancellation_token_set","note":"SSH and provisioning observe cancellation; other typed operations may finish their current operation."})
                } else {
                    self.backend
                        .job_control(&job, None)
                        .await
                        .unwrap_or_else(|e| json!({"note":e}))
                };
                self.event(
                    "job_cancel_requested",
                    json!({"jobId":id,"tool":job.tool,"instanceId":job.instance_id}),
                )
                .await;
                Ok(
                    json!({"status":"cancellation_requested","remote":remote,"message":"Cancellation is best effort. Inspect job and remote process state; do not assume rollback."}),
                )
            }
            _ => Err("Unknown job tool".into()),
        }
    }
}

pub fn private_directory(path: &Path) -> std::io::Result<()> {
    if path.exists() && std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::other("MCP storage must not be a symlink"));
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
