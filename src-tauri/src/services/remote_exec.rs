use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use tracing::{debug, info, warn};

use crate::errors::{AppError, AppResult};

use super::os_detection::OsDetection;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOutput {
    pub command: String,
    pub status_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone)]
pub struct RemoteExec {
    pub ssh_user: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub private_key_path: String,
}

impl RemoteExec {
    pub fn run_local(program: &str, args: &[&str], timeout: Duration) -> AppResult<ExecOutput> {
        let mut command = Command::new(program);
        command.args(args);
        run_with_timeout(command, Some(timeout))
    }

    pub fn ssh(&self, remote_command: &str, timeout: Duration) -> AppResult<ExecOutput> {
        ensure_command_available("ssh")?;
        self.ssh_with_key(remote_command, timeout)
    }

    pub fn ssh_until_complete(&self, remote_command: &str) -> AppResult<ExecOutput> {
        ensure_command_available("ssh")?;
        self.ssh_with_key_until_complete(remote_command)
    }

    fn ssh_with_key(&self, remote_command: &str, timeout: Duration) -> AppResult<ExecOutput> {
        let os = OsDetection::new();
        let connection_string = format!("{}@{}", self.ssh_user, self.ssh_host);
        let port_str = self.ssh_port.to_string();

        info!(
            "SSH command: ssh -p {} -i <key> -o StrictHostKeyChecking=no {} {}",
            port_str, connection_string, remote_command
        );

        let mut command = Command::new("ssh");
        command
            .arg("-p")
            .arg(&port_str)
            .arg("-i")
            .arg(&self.private_key_path)
            .arg("-o")
            .arg("StrictHostKeyChecking=no")
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                os.ssh_known_hosts_null_file()
            ))
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ServerAliveInterval=30")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg(&connection_string)
            .arg(remote_command);

        run_with_timeout(command, Some(timeout))
    }

    fn ssh_with_key_until_complete(&self, remote_command: &str) -> AppResult<ExecOutput> {
        let os = OsDetection::new();
        let connection_string = format!("{}@{}", self.ssh_user, self.ssh_host);
        let port_str = self.ssh_port.to_string();

        info!(
            "SSH command (no timeout): ssh -p {} -i <key> -o StrictHostKeyChecking=no {} {}",
            port_str, connection_string, remote_command
        );

        let mut command = Command::new("ssh");
        command
            .arg("-p")
            .arg(&port_str)
            .arg("-i")
            .arg(&self.private_key_path)
            .arg("-o")
            .arg("StrictHostKeyChecking=no")
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                os.ssh_known_hosts_null_file()
            ))
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ServerAliveInterval=30")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg(&connection_string)
            .arg(remote_command);

        run_with_timeout(command, None)
    }

    #[allow(dead_code)]
    pub fn scp(
        &self,
        local_path: &Path,
        remote_path: &str,
        timeout: Duration,
    ) -> AppResult<ExecOutput> {
        ensure_command_available("scp")?;
        let os = OsDetection::new();
        let mut command = Command::new("scp");
        command
            .arg("-i")
            .arg(&self.private_key_path)
            .arg("-P")
            .arg(self.ssh_port.to_string())
            .arg("-o")
            .arg("StrictHostKeyChecking=no")
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                os.ssh_known_hosts_null_file()
            ))
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg(local_path)
            .arg(format!("{}@{}:{remote_path}", self.ssh_user, self.ssh_host));
        run_with_timeout(command, Some(timeout))
    }
}

fn ensure_command_available(command: &str) -> AppResult<()> {
    let os = OsDetection::new();
    if os.command_exists(command) {
        return Ok(());
    }

    Err(AppError::Command(format!(
        "`{command}` is not available in PATH. {}",
        os.install_hint_for_tool(command)
    )))
}

fn run_with_timeout(mut command: Command, timeout: Option<Duration>) -> AppResult<ExecOutput> {
    let rendered = render_command(&command);
    let started = Instant::now();
    match timeout {
        Some(value) => info!("Running command with timeout {:?}: {}", value, rendered),
        None => info!("Running command without timeout: {}", rendered),
    }

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::Command(format!("Failed to spawn `{rendered}`: {error}")))?;

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Command(format!("Failed to capture stdout for `{rendered}`")))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Command(format!("Failed to capture stderr for `{rendered}`")))?;

    let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));

    let stdout_buf_reader = Arc::clone(&stdout_buf);
    let stdout_handle = thread::spawn(move || -> Result<(), String> {
        let mut reader = stdout_pipe;
        let mut data = Vec::new();
        reader
            .read_to_end(&mut data)
            .map_err(|error| format!("Failed reading stdout: {error}"))?;
        let mut guard = stdout_buf_reader
            .lock()
            .map_err(|_| "Failed locking stdout buffer".to_string())?;
        *guard = data;
        Ok(())
    });

    let stderr_buf_reader = Arc::clone(&stderr_buf);
    let stderr_handle = thread::spawn(move || -> Result<(), String> {
        let mut reader = stderr_pipe;
        let mut data = Vec::new();
        reader
            .read_to_end(&mut data)
            .map_err(|error| format!("Failed reading stderr: {error}"))?;
        let mut guard = stderr_buf_reader
            .lock()
            .map_err(|_| "Failed locking stderr buffer".to_string())?;
        *guard = data;
        Ok(())
    });

    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if let Some(limit) = timeout {
                    if started.elapsed() > limit {
                        warn!("command timed out after {:?}: {}", limit, rendered);
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(AppError::Timeout(format!(
                            "Command exceeded {:?}: {rendered}",
                            limit
                        )));
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                break Err(AppError::Command(format!(
                    "Failed polling `{rendered}`: {error}"
                )));
            }
        }
    };

    let stdout_join = stdout_handle
        .join()
        .map_err(|_| AppError::Command(format!("stdout reader panicked for `{rendered}`")))?;
    if let Err(error) = stdout_join {
        return Err(AppError::Command(format!("{error} for `{rendered}`")));
    }

    let stderr_join = stderr_handle
        .join()
        .map_err(|_| AppError::Command(format!("stderr reader panicked for `{rendered}`")))?;
    if let Err(error) = stderr_join {
        return Err(AppError::Command(format!("{error} for `{rendered}`")));
    }

    let status = exit_status?;

    let stdout_bytes = stdout_buf
        .lock()
        .map_err(|_| AppError::Command(format!("Failed locking stdout data for `{rendered}`")))?
        .clone();
    let stderr_bytes = stderr_buf
        .lock()
        .map_err(|_| AppError::Command(format!("Failed locking stderr data for `{rendered}`")))?
        .clone();

    let result = ExecOutput {
        command: rendered.clone(),
        status_code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        duration_ms: started.elapsed().as_millis(),
    };

    let stdout = if result.stdout.trim().is_empty() {
        "<empty>"
    } else {
        result.stdout.trim()
    };
    let stderr = if result.stderr.trim().is_empty() {
        "<empty>"
    } else {
        result.stderr.trim()
    };

    info!(
        "command finished (exit {}) in {}ms: {} | stdout: {} | stderr: {}",
        result.status_code, result.duration_ms, result.command, stdout, stderr
    );

    if result.status_code != 0 {
        warn!(
            "command exited non-zero ({}) in {}ms: {} | stderr: {}",
            result.status_code,
            result.duration_ms,
            result.command,
            result.stderr.trim()
        );
    } else {
        debug!(
            "command completed in {}ms: {}",
            result.duration_ms, result.command
        );
    }

    Ok(result)
}

fn render_command(command: &Command) -> String {
    let program = command.get_program().to_string_lossy();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!("{program} {args}")
}
