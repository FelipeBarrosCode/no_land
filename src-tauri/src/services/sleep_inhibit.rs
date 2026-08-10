use std::{
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
};

use crate::errors::{AppError, AppResult};

static ACTIVE_INHIBITOR: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

pub struct SleepInhibitService;

impl SleepInhibitService {
    pub fn ensure_active() -> AppResult<String> {
        let mut active = inhibitor_slot()
            .lock()
            .map_err(|_| AppError::State("Sleep inhibitor state is unavailable".to_string()))?;
        if child_is_running(active.as_mut()) {
            return Ok("Sleep prevention is already active on this client".to_string());
        }

        *active = None;
        let child = spawn_platform_inhibitor()?;
        *active = Some(child);
        Ok("Sleep prevention enabled for this streaming session".to_string())
    }

    pub fn stop() -> AppResult<String> {
        let mut active = inhibitor_slot()
            .lock()
            .map_err(|_| AppError::State("Sleep inhibitor state is unavailable".to_string()))?;
        if let Some(mut child) = active.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok("Sleep prevention stopped".to_string())
    }
}

fn inhibitor_slot() -> &'static Mutex<Option<Child>> {
    ACTIVE_INHIBITOR.get_or_init(|| Mutex::new(None))
}

fn child_is_running(child: Option<&mut Child>) -> bool {
    child
        .map(|child| matches!(child.try_wait(), Ok(None)))
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn spawn_platform_inhibitor() -> AppResult<Child> {
    Command::new("/usr/bin/caffeinate")
        .args(["-dims"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            AppError::Command(format!(
                "Failed to start the built-in macOS sleep inhibitor: {error}"
            ))
        })
}

#[cfg(target_os = "linux")]
fn spawn_platform_inhibitor() -> AppResult<Child> {
    Command::new("systemd-inhibit")
        .args([
            "--what=sleep:idle",
            "--why=Noland streaming session",
            "--mode=block",
            "sh",
            "-c",
            "while :; do sleep 3600; done",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            AppError::Command(format!(
                "The Linux desktop session did not provide its standard sleep-inhibition service: {error}"
            ))
        })
}

#[cfg(target_os = "windows")]
fn spawn_platform_inhibitor() -> AppResult<Child> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class SleepUtil { [DllImport(\"kernel32.dll\", SetLastError=true)] public static extern uint SetThreadExecutionState(uint esFlags); }'; [SleepUtil]::SetThreadExecutionState(0x80000000 -bor 0x00000001 -bor 0x00000002) | Out-Null; try { while ($true) { Start-Sleep -Seconds 3600 } } finally { [SleepUtil]::SetThreadExecutionState(0x80000000) | Out-Null }";

    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            AppError::Command(format!(
                "Failed to start the built-in Windows sleep inhibitor: {error}"
            ))
        })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn spawn_platform_inhibitor() -> AppResult<Child> {
    Err(AppError::Command(
        "Sleep prevention is not supported on this platform".to_string(),
    ))
}
