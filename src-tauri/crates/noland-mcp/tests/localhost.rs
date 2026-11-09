use async_trait::async_trait;
use noland_mcp::{
    core::{Backend, Core, Execution, ResultValue},
    policy::{self, Config, Scope, Spec},
    server,
};
use serde_json::{json, Map, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Default)]
struct App {
    calls: AtomicUsize,
}

#[async_trait]
impl Backend for App {
    fn specs(&self) -> Vec<Spec> {
        [("app_context", Scope::Read, false, false, false), ("ssh_exec", Scope::Shell, true, false, true),
            ("instance_destroy", Scope::Control, true, true, true), ("instance_inspect", Scope::Read, true, false, false)]
            .into_iter().map(|(name, scope, machine, approval, background)| Spec {
                name, scope, machine, approval, background, description: "Test-only app operation",
                schema: json!({"type":"object","additionalProperties":false,"properties":{
                    "instance_id":{"type":"integer","minimum":1},"script":{"type":"string","maxLength":1000},
                    "as_root":{"type":"boolean"},"approval_id":{"type":"string","maxLength":80},"request_id":{"type":"string","maxLength":80}},
                    "required":if machine {vec!["instance_id"]}else{vec![]}}).as_object().unwrap().clone(),
            }).collect()
    }
    async fn execute(&self, tool: &str, args: Map<String, Value>, e: Execution) -> ResultValue {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if args.get("script").and_then(Value::as_str) == Some("wait") {
            e.log("running\n").await;
            e.cancelled.cancelled().await;
            return Err("Cancelled; remote effects may remain".into());
        }
        Ok(json!({"tool":tool,"instanceId":args.get("instance_id"),"secretTest":"protected-value"}))
    }
    async fn sanitize(&self, value: Value) -> Value {
        noland_mcp::redaction::Redactor::new(["protected-value".into()]).sanitize(value)
    }
}

fn config() -> Config {
    Config {
        enabled: true,
        instances: vec![42],
        control: true,
        shell: true,
        ..Default::default()
    }
}
fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

async fn finish(core: &Arc<Core>, id: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = core
                .call("job_get", arguments(json!({"job_id":id})))
                .await
                .unwrap();
            if !matches!(
                job["status"].as_str(),
                Some("running" | "queued" | "cancelling")
            ) {
                break job;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("job must complete")
}

#[tokio::test]
async fn simple_local_access_includes_new_instances_but_keeps_destroy_approval() {
    let directory = tempfile::tempdir().unwrap();
    let app = Arc::new(App::default());
    let core = Core::new(
        app.clone(),
        config().for_local_agents(),
        "token",
        directory.path().into(),
    );
    let accepted = core
        .call(
            "ssh_exec",
            arguments(json!({
                "instance_id": 99, "as_root": true, "script": "inspect"
            })),
        )
        .await
        .unwrap();
    let job = finish(&core, accepted["jobId"].as_str().unwrap()).await;
    assert_eq!(job["status"], "completed");
    assert_eq!(
        core.call("jobs_list", arguments(json!({})))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let pending = core
        .call("instance_destroy", arguments(json!({"instance_id":99})))
        .await
        .unwrap();
    assert!(pending["approvalId"].is_string());
    assert_eq!(
        app.calls.load(Ordering::SeqCst),
        1,
        "destroy was not dispatched"
    );
    core.config.write().await.enabled = false;
    assert!(core
        .call("instance_inspect", arguments(json!({"instance_id":99})))
        .await
        .is_err());
}

#[tokio::test]
async fn scope_instance_root_and_schema_are_enforced_on_the_server() {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::new(
        Arc::new(App::default()),
        Config {
            enabled: true,
            ..Default::default()
        },
        "token",
        directory.path().into(),
    );
    assert!(core
        .call("ssh_exec", arguments(json!({"instance_id":42})))
        .await
        .is_err());
    *core.config.write().await = config();
    assert!(core
        .call("ssh_exec", arguments(json!({"instance_id":7})))
        .await
        .is_err());
    assert!(core
        .call(
            "ssh_exec",
            arguments(json!({"instance_id":42,"as_root":true}))
        )
        .await
        .is_err());
    assert!(core
        .call(
            "ssh_exec",
            arguments(json!({"instance_id":42,"extra":"not allowed"}))
        )
        .await
        .is_err());
    assert!(
        core.call("mcp_configure", arguments(json!({})))
            .await
            .is_err(),
        "no self-grant tool"
    );
    assert!(core
        .call("app_context", arguments(json!({})))
        .await
        .unwrap()
        .to_string()
        .contains("[redacted]"));
}

#[tokio::test]
async fn approval_is_exact_one_use_and_idempotent_across_restarts() {
    let directory = tempfile::tempdir().unwrap();
    let app = Arc::new(App::default());
    let core = Core::new(app.clone(), config(), "token", directory.path().into());
    let args = arguments(json!({"instance_id":42,"request_id":"destroy-once"}));
    let pending = core.call("instance_destroy", args.clone()).await.unwrap();
    let repeat = core.call("instance_destroy", args.clone()).await.unwrap();
    assert_eq!(pending["approvalId"], repeat["approvalId"]);
    assert_eq!(app.calls.load(Ordering::SeqCst), 0);
    let approval = pending["approvalId"].as_str().unwrap();
    core.decide(approval, true).await.unwrap();
    let mut approved = args.clone();
    approved.insert("approval_id".into(), json!(approval));
    let accepted = core
        .call("instance_destroy", approved.clone())
        .await
        .unwrap();
    let again = core.call("instance_destroy", approved).await.unwrap();
    assert_eq!(accepted["jobId"], again["jobId"]);
    finish(&core, accepted["jobId"].as_str().unwrap()).await;
    let mut changed = args.clone();
    changed.insert("request_id".into(), json!("another-request"));
    changed.insert("approval_id".into(), json!(approval));
    assert!(
        core.call("instance_destroy", changed).await.is_err(),
        "approval cannot authorize a second operation"
    );
    assert_eq!(app.calls.load(Ordering::SeqCst), 1);
    let restarted = Core::new(app.clone(), config(), "token", directory.path().into());
    assert_eq!(
        restarted.call("instance_destroy", args).await.unwrap()["jobId"],
        accepted["jobId"]
    );
    assert_eq!(app.calls.load(Ordering::SeqCst), 1);
    let metadata = std::fs::read_to_string(
        directory
            .path()
            .join("jobs")
            .join(format!("{}.json", accepted["jobId"].as_str().unwrap())),
    )
    .unwrap();
    assert!(!metadata.contains("protected-value"));
    assert!(!metadata.contains("secretTest"));
}

#[tokio::test]
async fn revocation_cancels_work_invalidates_approvals_and_hides_cached_results() {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::new(
        Arc::new(App::default()),
        config(),
        "token",
        directory.path().into(),
    );
    let pending = core
        .call("instance_destroy", arguments(json!({"instance_id":42})))
        .await
        .unwrap();
    let job = core
        .call(
            "ssh_exec",
            arguments(json!({"instance_id":42,"script":"wait"})),
        )
        .await
        .unwrap();
    let generation = core.generation.load(Ordering::SeqCst);
    core.revoke().await;
    assert!(core
        .decide(pending["approvalId"].as_str().unwrap(), true)
        .await
        .is_err());
    assert!(core
        .call_for_generation("app_context", Map::new(), generation)
        .await
        .is_err());
    let job = finish(&core, job["jobId"].as_str().unwrap()).await;
    assert_eq!(job["status"], "cancelled");
    assert!(job["result"].is_null());
    *core.config.write().await = Config {
        instances: vec![],
        ..config()
    };
    assert!(core
        .call("job_get", arguments(json!({"job_id":job["id"]})))
        .await
        .is_err());
}

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session: Option<&str>,
    body: Value,
) -> reqwest::Response {
    let mut request = client
        .post(url)
        .bearer_auth(token)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&body);
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    request.send().await.unwrap()
}

async fn response_json(response: reqwest::Response) -> Value {
    assert!(response.status().is_success(), "{}", response.status());
    let text = response.text().await.unwrap();
    if let Ok(value) = serde_json::from_str(&text) {
        return value;
    }
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .find_map(|data| serde_json::from_str(data).ok())
        .expect("JSON or SSE response")
}

#[tokio::test]
async fn real_streamable_http_handshake_resources_auth_rotation_and_port_conflict() {
    let directory = tempfile::tempdir().unwrap();
    let available = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = available.local_addr().unwrap().port();
    drop(available);
    let token = policy::generate_token();
    let core = Core::new(
        Arc::new(App::default()),
        Config { port, ..config() },
        &token,
        directory.path().into(),
    );
    let listener = server::start(core.clone()).await.unwrap();
    assert!(server::start(core.clone()).await.is_err());
    let url = format!("http://127.0.0.1:{port}/mcp");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"local-test-agent","version":"1"}}});
    assert_eq!(
        rpc(&client, &url, "wrong", None, initialize.clone())
            .await
            .status(),
        401
    );
    let foreign = client
        .post(&url)
        .bearer_auth(&token)
        .header("Origin", "https://evil.example")
        .json(&initialize)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);
    let rebind = client
        .post(&url)
        .bearer_auth(&token)
        .header("Host", format!("evil.example:{port}"))
        .json(&initialize)
        .send()
        .await
        .unwrap();
    assert_eq!(rebind.status(), 403);
    let response = rpc(&client, &url, &token, None, initialize).await;
    let session = response
        .headers()
        .get("Mcp-Session-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        response_json(response).await["result"]["protocolVersion"],
        "2025-11-25"
    );
    let initialized = rpc(
        &client,
        &url,
        &token,
        Some(&session),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    assert!(initialized.status().is_success());
    let tools = response_json(
        rpc(
            &client,
            &url,
            &token,
            Some(&session),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await,
    )
    .await;
    assert!(tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "app_context"));
    let resource=response_json(rpc(&client,&url,&token,Some(&session),json!({"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"noland://instances/42/status"}})).await).await;
    assert!(resource["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("instance_inspect"));
    assert_eq!(core.clients.read().await.len(), 1);
    let oversized = client
        .post(&url)
        .bearer_auth(&token)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(" ".repeat(1024 * 1024 + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);
    core.revoke().await;
    assert_eq!(
        rpc(
            &client,
            &url,
            &token,
            Some(&session),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/list"})
        )
        .await
        .status(),
        401
    );
    listener.stop().await;
    *core.token_hash.write().await = policy::digest(b"new-token");
    let restarted = server::start(core).await.unwrap();
    assert_eq!(
        rpc(&client, &url, &token, None, json!({})).await.status(),
        401
    );
    restarted.stop().await;
    assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok());
}
