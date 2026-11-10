use noland_mcp::policy::{Scope, Spec};
use serde_json::{json, Value};

fn text() -> Value {
    json!({"type":"string","maxLength":4096})
}
fn number(max: u64) -> Value {
    json!({"type":"integer","minimum":0,"maximum":max})
}
fn choice(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn boolean() -> Value {
    json!({"type":"boolean"})
}

fn tool(
    name: &'static str,
    description: &'static str,
    scope: Scope,
    machine: bool,
    approval: bool,
    background: bool,
    mut properties: Value,
    required: &[&str],
) -> Spec {
    let p = properties.as_object_mut().unwrap();
    let mut required = required.iter().map(|v| v.to_string()).collect::<Vec<_>>();
    if machine {
        p.insert("instance_id".into(), json!({"type":"integer","minimum":1}));
        required.push("instance_id".into());
    }
    p.insert("request_id".into(), json!({"type":"string","maxLength":80}));
    p.insert(
        "approval_id".into(),
        json!({"type":"string","maxLength":80}),
    );
    Spec { name, description, scope, machine, approval, background,
        schema:json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}).as_object().unwrap().clone() }
}

pub fn specs() -> Vec<Spec> {
    use Scope::*;
    vec![
        tool("app_context","Redacted live application, machine, job and pending-dialog context",Read,false,false,false,json!({}),&[]),
        tool("app_health","Run local application prerequisite checks (no credentials returned)",Read,false,false,true,json!({}),&[]),
        tool("app_logs","Read bounded redacted provisioning logs",Read,false,false,false,json!({"limit":number(100)}),&[]),
        tool("instances_list","List provider machines within the configured instance grants",Read,false,false,true,json!({}),&[]),
        tool("instance_inspect","Inspect a registered machine's safe state",Read,true,false,false,json!({}),&[]),
        tool("offers_search","Search GPU offers using current app preferences",Read,false,false,true,json!({}),&[]),
        tool("instance_create","Rent/provision the exact searched offer after human approval. max_hourly_price bounds the accepted offer estimate, not provider extras; storage must match the search. New PCs are available once registered in the app.",Control,false,true,true,json!({"offer_id":number(u64::MAX),"offer_fingerprint":text(),"storage_gb":number(10000),"max_hourly_price":{"type":"number","minimum":0,"maximum":1000}}),&["offer_id","offer_fingerprint","storage_gb","max_hourly_price"]),
        tool("instance_power","Stop/start an instance; cloud billing and running work can be affected",Control,true,true,true,json!({"action":choice(&["stop","start"])}),&["action"]),
        tool("instance_destroy","Permanently destroy a cloud instance; fresh human approval required",Control,true,true,true,json!({}),&[]),
        tool("instance_provision","Run the app's normal provisioning/resume flow for a registered PC",Control,true,false,true,json!({}),&[]),
        tool("provisioning_retry","Retry the current post-WireGuard setup stage through the existing guard",Control,true,false,true,json!({"stage":text()}),&["stage"]),
        tool("provisioning_stop","Request a safe stop after the current provisioning stage",Control,true,false,false,json!({}),&[]),
        tool("connection_status","Inspect tunnel/transport status",Read,true,false,true,json!({}),&[]),
        tool("connection_repair","Repair the app-managed connection using its existing mutation lock",Control,true,false,true,json!({}),&[]),
        tool("display_status","Inspect the remote display",Read,true,false,true,json!({}),&[]),
        tool("display_update","Apply a remote display mode",Control,true,false,true,json!({"mode":{"type":"object"}}),&["mode"]),
        tool("services_restart","Restart the instance's desktop services; current streaming can disconnect",Control,true,true,true,json!({}),&[]),
        tool("ssh_exec","Run a remote shell script as the desktop user (or explicitly granted root). Arbitrary shell access is NOT a sandbox. Returns an operation ID; optional stdin via job_input.",Shell,true,false,true,
            json!({"script":{"type":"string","maxLength":131072},"cwd":text(),"timeout_seconds":number(3600),"interactive":boolean(),"as_root":boolean()}),&["script"]),
        tool("files_list","List up to 200 remote directory entries",Read,true,false,true,json!({"path":text()}),&["path"]),
        tool("files_read","Read a bounded regular remote file; binary data is base64",Read,true,false,true,json!({"path":text(),"offset":number(u64::MAX),"limit":number(196608)}),&["path"]),
        tool("files_write","Atomically write a remote file. Existing files require an expected SHA256",Control,true,false,true,json!({"path":text(),"content":{"type":"string","maxLength":262144},"encoding":choice(&["utf8","base64"]),"expected_sha256":text()}),&["path","content"]),
        tool("files_mkdir","Create a remote directory as the desktop user",Control,true,false,true,json!({"path":text()}),&["path"]),
        tool("files_move","Move a remote path without overwriting an existing destination",Control,true,false,true,json!({"path":text(),"destination":text()}),&["path","destination"]),
        tool("files_delete","Delete a remote file/directory after human approval",Control,true,true,true,json!({"path":text(),"recursive":boolean()}),&["path"]),
        tool("files_upload","Upload a local file from an explicitly granted directory to the cloud PC; no overwrite",Control,true,true,true,json!({"local_path":text(),"remote_path":text()}),&["local_path","remote_path"]),
        tool("files_download","Download a cloud file into an explicitly granted local directory; no overwrite",Control,true,true,true,json!({"local_path":text(),"remote_path":text()}),&["local_path","remote_path"]),
        tool("processes_list","List the desktop user's remote processes with start identities",Read,true,false,true,json!({"match":text()}),&[]),
        tool("process_inspect","Inspect one remote process, rejecting a stale/reused PID",Read,true,false,true,json!({"pid":number(u32::MAX as u64),"start_identity":text()}),&["pid","start_identity"]),
        tool("process_signal","Signal an owned process after matching its start identity and human approval",Control,true,true,true,json!({"pid":number(u32::MAX as u64),"start_identity":text(),"signal":choice(&["TERM","KILL","STOP","CONT"])}),&["pid","start_identity","signal"]),
        tool("runtime_status","Inspect the cloud-PC runtime and registered apps",Read,true,false,true,json!({}),&[]),
        tool("runtime_install","Install/repair the same runtime and desktop icon as normal provisioning",Control,true,false,true,json!({}),&[]),
        tool("runtime_doctor","Check namespace, GPU, display and audio; a small test window may briefly appear",Read,true,false,true,json!({}),&[]),
        tool("runtime_register_app","Register a trusted Linux executable/AppImage on the cloud PC",Control,true,false,true,json!({"path":text(),"make_executable":boolean()}),&["path"]),
        tool("runtime_launch_app","Launch a registered runtime app; returns supervisor/process-log details",Control,true,false,true,json!({"app_id":text()}),&["app_id"]),
        tool("library_list","Load the app's launch library for a machine",Read,true,false,true,json!({}),&[]),
        tool("library_launch","Launch a library entry using the app's normal native/Steam/runtime route",Control,true,false,true,json!({"app_id":text()}),&["app_id"]),
        tool("stream_status","Inspect the native streaming session",Read,false,false,false,json!({}),&[]),
        tool("stream_connect","Open a stream using the app's normal Launch PC flow",Control,true,false,true,json!({}),&[]),
        tool("stream_disconnect","Disconnect streaming without stopping or destroying the cloud PC",Control,true,false,true,json!({}),&[]),
        tool("stream_pair","Begin/complete embedded Moonlight pairing",Control,true,false,true,json!({"action":choice(&["begin","complete"]),"session_id":text()}),&["action"]),
        tool("settings_get","Read non-secret stream/server settings; authentication settings are never exposed",Read,false,false,false,json!({}),&[]),
        tool("settings_update","Update allowlisted stream settings, with explicit human approval",Control,false,true,true,json!({"patch":{"type":"object"}}),&["patch"]),
        tool("storage_status","Inspect backup state",Read,true,false,true,json!({}),&[]),
        tool("storage_backup","Back up selected remote objects through the normal storage service",Control,true,false,true,json!({"object_ids":{"type":"array"}}),&["object_ids"]),
        tool("storage_restore","Restore selected remote objects; human approval required",Control,true,true,true,json!({"object_ids":{"type":"array"}}),&["object_ids"]),
        tool("ui_snapshot","Inspect the main app's registered actions and dialogs, not the entire local computer",Read,false,false,false,json!({}),&[]),
        tool("ui_navigate","Navigate to dashboard, provisioning, or ordinary settings; MCP permissions are human-only",Dialog,false,false,false,json!({"route":choice(&["/","/provisioning","/settings"])}),&["route"]),
        tool("ui_action","Invoke an explicitly delegated visible UI action at the exact snapshot revision",Dialog,false,false,false,json!({"action_id":text(),"revision":text()}),&["action_id","revision"]),
        tool("dialogs_list","List visible dialogs, including why some require a human",Read,false,false,false,json!({}),&[]),
        tool("dialog_respond","Respond to an explicitly delegated dialog button, never MCP/security approvals",Dialog,false,false,false,json!({"action_id":text(),"revision":text()}),&["action_id","revision"]),
        tool("desktop_snapshot","View cloud-PC windows and optionally a screenshot. Can reveal private screen contents.",Desktop,true,false,true,json!({"screenshot":boolean()}),&[]),
        tool("desktop_input","Send mouse/keyboard input to the cloud desktop, NOT the local Mac/Windows desktop",Desktop,true,false,true,json!({"action":choice(&["click","move","key","type"]),"x":number(16384),"y":number(16384),"button":number(5),"text":text()}),&["action"]),
        tool("desktop_window_action","Focus/minimize/maximize/close an identified cloud application window",Desktop,true,false,true,json!({"window_id":text(),"action":choice(&["focus","minimize","maximize","close"])}),&["window_id","action"]),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn tools_are_unique_and_dangerous_operations_require_approval() {
        let specs = super::specs();
        let mut names = std::collections::HashSet::new();
        for s in &specs {
            assert!(names.insert(s.name));
        }
        for name in [
            "instance_create",
            "instance_destroy",
            "files_delete",
            "storage_restore",
            "process_signal",
        ] {
            assert!(specs.iter().find(|s| s.name == name).unwrap().approval);
        }
    }
}
