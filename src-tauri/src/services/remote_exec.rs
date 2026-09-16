use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::{debug, info, warn};

use crate::{
    errors::{AppError, AppResult},
    utils::{managed_binaries::configure_bundled_linux_runtime, process::configure_no_window},
};

use super::os_detection::OsDetection;

fn locate_ssh_binary(tool: &str) -> Option<std::path::PathBuf> {
    let os = OsDetection::new();

    match tool {
        "ssh" => os.locate_app_managed_binary("ssh", "NOLAND_SSH_BIN", cfg!(target_os = "windows")),
        "scp" => os.locate_app_managed_binary("scp", "NOLAND_SCP_BIN", cfg!(target_os = "windows")),
        _ => None,
    }
}

fn resolve_ssh_binary(tool: &str) -> AppResult<std::path::PathBuf> {
    let os = OsDetection::new();
    locate_ssh_binary(tool).ok_or_else(|| {
        AppError::Command(format!(
            "`{tool}` is not available in the app bundle. {}",
            os.install_hint_for_tool(tool)
        ))
    })
}

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
    pub ssh_password: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    pub session_id: String,
    pub ssh_user: String,
    pub ssh_host: String,
    pub ssh_port: u16,
}

struct InteractiveSession {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
}

static INTERACTIVE_SESSIONS: OnceLock<
    Mutex<std::collections::HashMap<String, Arc<InteractiveSession>>>,
> = OnceLock::new();

fn interactive_sessions(
) -> &'static Mutex<std::collections::HashMap<String, Arc<InteractiveSession>>> {
    INTERACTIVE_SESSIONS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

impl RemoteExec {
    pub fn is_root(&self) -> bool {
        self.ssh_user == "root"
    }

    pub fn sudo_prefix(&self) -> String {
        if self.is_root() {
            String::new()
        } else if self.ssh_password.trim().is_empty() {
            "sudo ".to_string()
        } else {
            format!(
                "printf %s {} | sudo -S -p '' ",
                shell_single_quote_escape(&self.ssh_password)
            )
        }
    }

    pub fn sudo_as_user_prefix(&self, target_user: &str) -> String {
        if self.is_root() {
            format!("sudo -u {} ", target_user)
        } else if self.ssh_password.trim().is_empty() {
            format!("sudo -u {} ", target_user)
        } else {
            format!(
                "printf %s {} | sudo -S -p '' -u {} ",
                shell_single_quote_escape(&self.ssh_password),
                target_user
            )
        }
    }

    pub fn ssh(&self, remote_command: &str, timeout: Duration) -> AppResult<ExecOutput> {
        ensure_command_available("ssh")?;
        self.ssh_with_key(remote_command, timeout)
    }

    pub fn ssh_until_complete(&self, remote_command: &str) -> AppResult<ExecOutput> {
        ensure_command_available("ssh")?;
        self.ssh_with_key_until_complete(remote_command)
    }

    pub fn open_terminal(&self, app: AppHandle) -> AppResult<TerminalSession> {
        ensure_command_available("ssh")?;
        let os = OsDetection::new();
        let ssh_binary = resolve_ssh_binary("ssh")?;
        let connection_string = format!("{}@{}", self.ssh_user, self.ssh_host);
        let session_id = uuid::Uuid::new_v4().to_string();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| AppError::Command(format!("Could not create terminal: {error}")))?;
        let mut command = CommandBuilder::new(&ssh_binary);
        let port = self.ssh_port.to_string();
        let known_hosts = format!("UserKnownHostsFile={}", os.ssh_known_hosts_null_file());
        for arg in [
            "-tt",
            "-p",
            &port,
            "-i",
            &self.private_key_path,
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            &known_hosts,
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "BatchMode=yes",
            "-o",
            "PreferredAuthentications=publickey",
            "-o",
            "IdentitiesOnly=yes",
            &connection_string,
        ] {
            command.arg(arg);
        }
        #[cfg(target_os = "linux")]
        if let Some(binary_dir) = ssh_binary.parent() {
            let runtime_dir = binary_dir
                .join("ssh-runtime")
                .join(os.managed_binary_target_triple());
            if runtime_dir.is_dir() {
                command.env("LD_LIBRARY_PATH", runtime_dir);
            }
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| AppError::Command(format!("Could not open SSH terminal: {error}")))?;
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| AppError::Command(format!("Could not read SSH terminal: {error}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| AppError::Command(format!("Could not write SSH terminal: {error}")))?;
        let session = Arc::new(InteractiveSession {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
        });
        interactive_sessions()
            .lock()
            .map_err(|_| AppError::Command("Terminal session lock was poisoned".into()))?
            .insert(session_id.clone(), session);
        spawn_terminal_reader(app, session_id.clone(), reader);
        Ok(TerminalSession {
            session_id,
            ssh_user: self.ssh_user.clone(),
            ssh_host: self.ssh_host.clone(),
            ssh_port: self.ssh_port,
        })
    }

    pub fn write_terminal(session_id: &str, input: &str) -> AppResult<()> {
        let session = interactive_sessions()
            .lock()
            .map_err(|_| AppError::Command("Terminal session lock was poisoned".into()))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                AppError::InvalidInput("Terminal session is no longer connected".into())
            })?;
        use std::io::Write;
        let mut writer = session
            .writer
            .lock()
            .map_err(|_| AppError::Command("Terminal input lock was poisoned".into()))?;
        writer
            .write_all(input.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|error| AppError::Command(format!("Could not write to SSH terminal: {error}")))
    }

    pub fn resize_terminal(session_id: &str, rows: u16, cols: u16) -> AppResult<()> {
        let session = interactive_sessions()
            .lock()
            .map_err(|_| AppError::Command("Terminal session lock was poisoned".into()))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                AppError::InvalidInput("Terminal session is no longer connected".into())
            })?;
        let result = session
            .master
            .lock()
            .map_err(|_| AppError::Command("Terminal resize lock was poisoned".into()))?
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| AppError::Command(format!("Could not resize SSH terminal: {error}")));
        result
    }

    pub fn close_terminal(session_id: &str) -> AppResult<()> {
        if let Some(session) = interactive_sessions()
            .lock()
            .map_err(|_| AppError::Command("Terminal session lock was poisoned".into()))?
            .remove(session_id)
        {
            let mut child = session
                .child
                .lock()
                .map_err(|_| AppError::Command("Terminal process lock was poisoned".into()))?;
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    fn ssh_with_key(&self, remote_command: &str, timeout: Duration) -> AppResult<ExecOutput> {
        let os = OsDetection::new();
        let connection_string = format!("{}@{}", self.ssh_user, self.ssh_host);
        let port_str = self.ssh_port.to_string();

        info!(
            "SSH command: ssh -T -p {} -i <key> -o StrictHostKeyChecking=no {} {}",
            port_str, connection_string, remote_command
        );

        let ssh_binary = resolve_ssh_binary("ssh")?;
        let mut command = Command::new(&ssh_binary);
        configure_bundled_linux_runtime(
            &mut command,
            &ssh_binary,
            "ssh-runtime",
            os.managed_binary_target_triple(),
        );
        command
            .arg("-T")
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
            "SSH command (no timeout): ssh -T -p {} -i <key> -o StrictHostKeyChecking=no {} {}",
            port_str, connection_string, remote_command
        );

        let ssh_binary = resolve_ssh_binary("ssh")?;
        let mut command = Command::new(&ssh_binary);
        configure_bundled_linux_runtime(
            &mut command,
            &ssh_binary,
            "ssh-runtime",
            os.managed_binary_target_triple(),
        );
        command
            .arg("-T")
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
        self.scp_path(local_path, remote_path, false, timeout)
    }

    pub fn scp_path(
        &self,
        local_path: &Path,
        remote_path: &str,
        recursive: bool,
        timeout: Duration,
    ) -> AppResult<ExecOutput> {
        ensure_command_available("scp")?;
        let os = OsDetection::new();
        let scp_binary = resolve_ssh_binary("scp")?;
        let ssh_binary = resolve_ssh_binary("ssh")?;
        let mut command = Command::new(&scp_binary);
        configure_bundled_linux_runtime(
            &mut command,
            &scp_binary,
            "ssh-runtime",
            os.managed_binary_target_triple(),
        );
        command
            .arg("-S")
            .arg(&ssh_binary)
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
            .arg("IdentitiesOnly=yes");
        if recursive {
            command.arg("-r");
        }
        command
            .arg(local_path)
            .arg(format!("{}@{}:{remote_path}", self.ssh_user, self.ssh_host));
        run_with_timeout(command, Some(timeout))
    }
}

fn spawn_terminal_reader<R: Read + Send + 'static>(
    app: AppHandle,
    session_id: String,
    mut reader: R,
) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    let data = String::from_utf8_lossy(&buffer[..size]).into_owned();
                    let _ = app.emit(
                        "remote-terminal-output",
                        serde_json::json!({ "sessionId": session_id, "data": data }),
                    );
                }
                Err(_) => break,
            }
        }
        let session = interactive_sessions()
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&session_id));
        let exit_code: Option<u32> = session.and_then(|session| {
            session
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.wait().ok())
                .map(|status| status.exit_code())
        });
        let _ = app.emit(
            "remote-terminal-closed",
            serde_json::json!({ "sessionId": session_id, "exitCode": exit_code }),
        );
    });
}

fn shell_single_quote_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn ensure_command_available(command: &str) -> AppResult<()> {
    if locate_ssh_binary(command).is_some() {
        return Ok(());
    }

    let os = OsDetection::new();
    Err(AppError::Command(format!(
        "`{command}` is not available in the app bundle. {}",
        os.install_hint_for_tool(command)
    )))
}

fn run_with_timeout(mut command: Command, timeout: Option<Duration>) -> AppResult<ExecOutput> {
    configure_no_window(&mut command);
    let rendered = render_command(&command);
    let started = Instant::now();
    match timeout {
        Some(value) => info!("Running command with timeout {:?}: {}", value, rendered),
        None => info!("Running command without timeout: {}", rendered),
    }

    let mut child = command
        .stdin(Stdio::null())
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
