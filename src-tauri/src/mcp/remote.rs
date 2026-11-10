use crate::{
    commands,
    services::{app_context::AppContext, remote_exec::RemoteExec},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{write::GzEncoder, Compression};
use noland_mcp::core::{Execution, Job};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

fn helper(remote: &RemoteExec, user: &str, as_root: bool) -> String {
    let mut compressed = GzEncoder::new(Vec::new(), Compression::fast());
    compressed
        .write_all(include_bytes!("remote.py"))
        .expect("embedded helper");
    let encoded = STANDARD.encode(compressed.finish().expect("embedded helper gzip"));
    let program = format!(
        "import base64,gzip;exec(gzip.decompress(base64.b64decode({})))",
        quote(&encoded)
    );
    let prefix = if as_root {
        if remote.is_root() {
            String::new()
        } else {
            "sudo -n -H -- ".into()
        }
    } else if remote.ssh_user == user {
        String::new()
    } else {
        format!("sudo -n -H -u {} -- ", quote(user))
    };
    format!("{prefix}/usr/bin/python3 -c {}", quote(&program))
}

async fn prepare(
    context: &AppContext,
    directory: &Path,
    id: u64,
    as_root: bool,
) -> Result<tokio::process::Command, String> {
    let remote = commands::build_remote_exec_for_instance(context, id)
        .await
        .map_err(|e| e.to_string())?;
    let known = directory.join("known_hosts");
    noland_mcp::core::private_directory(&known).map_err(|e| e.to_string())?;
    remote
        .automation_command(
            &helper(&remote, &context.config.audio_target_user, as_root),
            &known.join(id.to_string()),
        )
        .map_err(|e| e.to_string())
}

async fn drain(
    mut stream: impl AsyncRead + Unpin,
    limit: usize,
    execution: Option<Execution>,
) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if output.len() < limit {
            output.extend_from_slice(&buffer[..n.min(limit - output.len())]);
        }
        if let Some(e) = &execution {
            let mut tail = e.output.write().await;
            tail.push_str(&String::from_utf8_lossy(&buffer[..n]));
            if tail.len() > 262144 {
                let mut cut = tail.len() - 262144;
                while !tail.is_char_boundary(cut) {
                    cut += 1;
                }
                tail.drain(..cut);
            }
        }
    }
    output
}

pub async fn request(
    context: &AppContext,
    directory: &Path,
    mode: &str,
    mut args: Map<String, Value>,
    execution: &Execution,
) -> Result<Value, String> {
    let id = args
        .get("instance_id")
        .and_then(Value::as_u64)
        .ok_or("instance_id required")?;
    let root = args
        .get("as_root")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut command = prepare(context, directory, id, root).await?;
    args.insert("mode".into(), json!(mode));
    args.insert("job_id".into(), json!(execution.job_id));
    let mut input = serde_json::to_vec(&args).map_err(|e| e.to_string())?;
    input.push(b'\n');
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("Missing SSH stdin")?;
    stdin.write_all(&input).await.map_err(|e| e.to_string())?;
    drop(stdin);
    let stdout = child.stdout.take().ok_or("Missing SSH stdout")?;
    let stderr = child.stderr.take().ok_or("Missing SSH stderr")?;
    let out = tokio::spawn(drain(
        stdout,
        4 * 1024 * 1024,
        if mode == "ssh_exec" {
            Some(execution.clone())
        } else {
            None
        },
    ));
    let err = tokio::spawn(drain(stderr, 131072, Some(execution.clone())));
    let timeout = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(180)
        .clamp(1, 3600)
        + 20;
    let result = tokio::select! {
        status=child.wait()=>status.map_err(|e|e.to_string()),
        _=execution.cancelled.cancelled()=> {
            if mode=="ssh_exec" {
                let job=Job {id:execution.job_id.clone(),tool:mode.into(),instance_id:Some(id),as_root:root,status:"cancelling".into(),started_at:String::new(),finished_at:None,result:None,error:None,request_id:None,argument_hash:None};
                let _=control(context,directory,&job,None).await;
            }
            let _=child.kill().await; Err("Local SSH operation cancelled. Remote work may remain; inspect its process state.".into())
        },
        _=tokio::time::sleep(Duration::from_secs(timeout))=> {
            if mode=="ssh_exec" {
                let job=Job {id:execution.job_id.clone(),tool:mode.into(),instance_id:Some(id),as_root:root,status:"cancelling".into(),started_at:String::new(),finished_at:None,result:None,error:None,request_id:None,argument_hash:None};
                let _=control(context,directory,&job,None).await;
            }
            let _=child.kill().await;Err("SSH timed out. Remote work may remain; inspect before retrying.".into())
        }
    };
    let output = out.await.map_err(|e| e.to_string())?;
    let error = String::from_utf8_lossy(&err.await.map_err(|e| e.to_string())?).into_owned();
    let status = result?;
    if mode == "ssh_exec" {
        return Ok(
            json!({"exitCode":status.code(),"success":status.success(),"stdout":String::from_utf8_lossy(&output),"stderr":error,"remoteJobId":execution.job_id,"outputMayBeTruncated":output.len()>=4*1024*1024}),
        );
    }
    if !status.success() {
        return Err(format!(
            "Remote operation failed: {error} {}",
            String::from_utf8_lossy(&output)
                .chars()
                .take(4096)
                .collect::<String>()
        ));
    }
    serde_json::from_slice(&output)
        .map_err(|_| "Remote response was invalid or exceeded the output limit".into())
}

pub async fn control(
    context: &AppContext,
    directory: &Path,
    job: &Job,
    input: Option<String>,
) -> Result<Value, String> {
    if job.tool != "ssh_exec" {
        return Err(
            "This typed operation supports only its domain's safe stage-boundary cancellation"
                .into(),
        );
    }
    let id = job.instance_id.ok_or("No instance for job")?;
    let mut command = prepare(context, directory, id, job.as_root).await?;
    let body = json!({"mode":"job_control","job_id":job.id,"input":input}).to_string() + "\n";
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("SSH stdin missing")?;
    stdin
        .write_all(body.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    drop(stdin);
    let out = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output())
        .await
        .map_err(|_| "Remote job control timed out")?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .chars()
            .take(2048)
            .collect());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

async fn local_path(
    context: &AppContext,
    config: &noland_mcp::policy::Config,
    input: &str,
    download: bool,
) -> Result<PathBuf, String> {
    let path = Path::new(input);
    if !path.is_absolute() || input.contains(['\n', '\r', '\0']) {
        return Err("An absolute local file path is required".into());
    }
    let canonical = if download {
        let parent = path
            .parent()
            .ok_or("Missing local directory")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        parent.join(path.file_name().ok_or("Missing filename")?)
    } else {
        path.canonicalize().map_err(|e| e.to_string())?
    };
    let app_data = context
        .state_store
        .path()
        .parent()
        .ok_or("Missing app data directory")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let private_key = context.state.read().await.ssh.private_key_path.clone();
    let key_path = Path::new(&private_key).canonicalize().ok();
    if canonical.starts_with(app_data) || key_path.is_some_and(|key|key==canonical) || canonical.components().any(|c|matches!(c,std::path::Component::Normal(s) if [".ssh",".aws",".gnupg",".kube",".azure","keychains"].contains(&s.to_string_lossy().to_ascii_lowercase().as_str()))) {
        return Err("App credentials and local SSH/cloud credential directories cannot be transferred through MCP".into());
    }
    if !config
        .local_roots
        .iter()
        .filter_map(|r| Path::new(r).canonicalize().ok())
        .any(|r| canonical.starts_with(r))
    {
        return Err("This local path is not under a user-granted transfer directory".into());
    }
    Ok(canonical)
}

pub async fn transfer(
    context: &AppContext,
    directory: &Path,
    mode: &str,
    args: Map<String, Value>,
    execution: &Execution,
) -> Result<Value, String> {
    let download = mode == "files_download";
    let id = args
        .get("instance_id")
        .and_then(Value::as_u64)
        .ok_or("instance_id required")?;
    let local = local_path(
        context,
        &execution.config,
        args.get("local_path")
            .and_then(Value::as_str)
            .ok_or("local_path required")?,
        download,
    )
    .await?;
    let mut request = Value::Object(args.clone());
    request["mode"] = json!(mode);
    const MAX: u64 = 1024 * 1024 * 1024;
    if !download {
        let path = local.clone();
        let (size, sha) = tokio::task::spawn_blocking(move || -> Result<(u64, String), String> {
            use std::io::Read;
            let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
            let metadata = f.metadata().map_err(|e| e.to_string())?;
            let size = metadata.len();
            if !metadata.is_file() || size > MAX {
                return Err("Choose a regular file of at most 1 GiB".into());
            }
            let mut h = Sha256::new();
            let mut b = [0u8; 65536];
            loop {
                let n = f.read(&mut b).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                h.update(&b[..n]);
            }
            Ok((size, hex::encode(h.finalize())))
        })
        .await
        .map_err(|e| e.to_string())??;
        request["size"] = json!(size);
        request["sha256"] = json!(sha);
    } else if local.exists() {
        return Err("Local destination already exists; no file was overwritten".into());
    }
    let mut command = prepare(context, directory, id, false).await?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stderr = child.stderr.take().ok_or("SSH stderr missing")?;
    let errors = tokio::spawn(drain(stderr, 65536, Some(execution.clone())));
    let mut stdin = child.stdin.take().ok_or("SSH stdin missing")?;
    stdin
        .write_all((request.to_string() + "\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let temporary = local
        .parent()
        .ok_or("Missing local directory")?
        .join(format!(".noland-mcp-{}", execution.job_id));
    let operation = async {
        if download {
            drop(stdin);
            let mut stdout = BufReader::new(child.stdout.take().ok_or("SSH stdout missing")?);
            let mut line = String::new();
            stdout
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            if line.len() > 1024 {
                return Err("Invalid transfer header".into());
            }
            let info: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            let size = info["size"]
                .as_u64()
                .filter(|n| *n <= MAX)
                .ok_or("Invalid download size")?;
            let mut options = tokio::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).await.map_err(|e| e.to_string())?;
            let mut h = Sha256::new();
            let mut total = 0;
            let mut b = [0u8; 65536];
            loop {
                let n = stdout.read(&mut b).await.map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                if total > size {
                    return Err("Download exceeds announced size".into());
                }
                h.update(&b[..n]);
                file.write_all(&b[..n]).await.map_err(|e| e.to_string())?;
            }
            file.sync_all().await.map_err(|e| e.to_string())?;
            drop(file);
            if total != size || info["sha256"].as_str() != Some(&hex::encode(h.finalize())) {
                return Err("Download integrity check failed".into());
            }
            if !child.wait().await.map_err(|e| e.to_string())?.success() {
                return Err("Remote download failed".into());
            }
            tokio::fs::hard_link(&temporary, &local)
                .await
                .map_err(|e| format!("Could not publish without overwriting: {e}"))?;
            Ok(json!({"path":local,"bytes":total,"sha256":info["sha256"]}))
        } else {
            let mut file = tokio::fs::File::open(&local)
                .await
                .map_err(|e| e.to_string())?;
            tokio::io::copy(&mut file, &mut stdin)
                .await
                .map_err(|e| e.to_string())?;
            drop(stdin);
            let out = drain(
                child.stdout.take().ok_or("SSH stdout missing")?,
                65536,
                None,
            )
            .await;
            if !child.wait().await.map_err(|e| e.to_string())?.success() {
                return Err("Remote upload failed; destination was not overwritten".into());
            }
            serde_json::from_slice(&out).map_err(|e| e.to_string())
        }
    };
    let result = tokio::select! {result=operation=>result,_=execution.cancelled.cancelled()=>Err("Transfer cancelled; inspect remote state".into()),_=tokio::time::sleep(Duration::from_secs(3600))=>Err("Transfer timed out".into())};
    // kill_on_drop closes SSH on interrupted transfers; publish is atomic.
    drop(child);
    let _ = tokio::fs::remove_file(temporary).await;
    let error = errors.await.map_err(|e| e.to_string())?;
    result.map_err(|e| format!("{e}: {}", String::from_utf8_lossy(&error)))
}
