use std::{
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use tracing::{debug, info, warn};

use crate::errors::{AppError, AppResult};

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
        run_with_timeout(command, timeout)
    }

    pub fn ssh(&self, remote_command: &str, timeout: Duration) -> AppResult<ExecOutput> {
        self.ssh_with_key(remote_command, timeout)
    }

    pub fn wait_for_dpkg_lock(&self, max_wait_secs: u64) -> AppResult<bool> {
        let connection_string = format!("{}@{}", self.ssh_user, self.ssh_host);
        let port_str = self.ssh_port.to_string();

        info!(
            "Waiting for dpkg lock to be released on {}:{} (max {}s, checking every 1s)...",
            self.ssh_host, port_str, max_wait_secs
        );

        let wait_script = format!(
            r#"#!/bin/bash
max_wait={}
check_count=0
while sudo fuser /var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/cache/apt/archives/lock /var/lib/apt/lists/lock >/dev/null 2>&1; do
    check_count=$((check_count + 1))
    if [ $((check_count % 10)) -eq 0 ]; then
        echo "Still waiting for dpkg lock... ({}s elapsed)"
    fi
    sleep 1
    max_wait=$((max_wait - 1))
    if [ $max_wait -le 0 ]; then
        echo "Timeout waiting for dpkg lock after {} seconds"
        exit 1
    fi
done
echo "dpkg lock released after $((check_count)) checks"
exit 0"#,
            max_wait_secs, max_wait_secs, max_wait_secs
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
            .arg("UserKnownHostsFile=/dev/null")
            .arg("-o")
            .arg("ConnectTimeout=30")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg(&connection_string)
            .arg(&wait_script);

        let output = run_with_timeout(command, Duration::from_secs(max_wait_secs + 30))?;

        if output.status_code != 0 {
            warn!(
                "Timeout waiting for dpkg lock after {} seconds",
                max_wait_secs
            );
            return Ok(false);
        }

        info!("dpkg lock released successfully");
        Ok(true)
    }

    fn ssh_with_key(&self, remote_command: &str, timeout: Duration) -> AppResult<ExecOutput> {
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
            .arg("UserKnownHostsFile=/dev/null")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg(&connection_string)
            .arg(remote_command);

        run_with_timeout(command, timeout)
    }

    #[allow(dead_code)]
    pub fn scp(
        &self,
        local_path: &Path,
        remote_path: &str,
        timeout: Duration,
    ) -> AppResult<ExecOutput> {
        let mut command = Command::new("scp");
        command
            .arg("-i")
            .arg(&self.private_key_path)
            .arg("-P")
            .arg(self.ssh_port.to_string())
            .arg("-o")
            .arg("StrictHostKeyChecking=no")
            .arg("-o")
            .arg("UserKnownHostsFile=/dev/null")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg(local_path)
            .arg(format!("{}@{}:{remote_path}", self.ssh_user, self.ssh_host));
        run_with_timeout(command, timeout)
    }
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> AppResult<ExecOutput> {
    let rendered = render_command(&command);
    let started = Instant::now();
    info!("Running command with timeout {:?}: {}", timeout, rendered);

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::Command(format!("Failed to spawn `{rendered}`: {error}")))?;

    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                let output = child.wait_with_output().map_err(|error| {
                    AppError::Command(format!("Failed waiting for `{rendered}`: {error}"))
                })?;

                let result = ExecOutput {
                    command: rendered.clone(),
                    status_code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
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

                return Ok(result);
            }
            Ok(None) => {
                if started.elapsed() > timeout {
                    warn!("command timed out after {:?}: {}", timeout, rendered);
                    let _ = child.kill();
                    return Err(AppError::Timeout(format!(
                        "Command exceeded {:?}: {rendered}",
                        timeout
                    )));
                }

                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                return Err(AppError::Command(format!(
                    "Failed polling `{rendered}`: {error}"
                )));
            }
        }
    }
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
