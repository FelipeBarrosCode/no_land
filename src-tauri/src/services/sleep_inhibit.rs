use std::{
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
};

use crate::errors::{AppError, AppResult};

static SLEEP_INHIBITOR: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn inhibitor_slot() -> &'static Mutex<Option<Child>> {
    SLEEP_INHIBITOR.get_or_init(|| Mutex::new(None))
}

pub struct SleepInhibitService;

impl SleepInhibitService {
    pub fn ensure_active() -> AppResult<String> {
        let mut slot = inhibitor_slot().lock().map_err(|_| {
            AppError::State("Failed to lock sleep inhibitor state".to_string())
        })?;

        if let Some(child) = slot.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                *slot = None;
            }
        }

        if slot.is_some() {
            return Ok("Sleep prevention is already active on this client".to_string());
        }

        #[cfg(target_os = "macos")]
        {
            let child = Command::new("caffeinate")
                .args(["-dims"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| {
                    AppError::Command(format!("Failed to start caffeinate: {error}"))
                })?;
            *slot = Some(child);
            return Ok("Sleep prevention enabled using caffeinate".to_string());
        }

        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("sh")
                .args(["-lc", "xset s off -dpms >/dev/null 2>&1 || true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let child = Command::new("systemd-inhibit")
                .args([
                    "--what=sleep:idle",
                    "--why=Noland Moonlight streaming session",
                    "bash",
                    "-lc",
                    "while true; do sleep 60; done",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| {
                    AppError::Command(format!("Failed to start systemd-inhibit: {error}"))
                })?;
            *slot = Some(child);
            return Ok("Sleep prevention enabled using systemd-inhibit".to_string());
        }

        #[cfg(target_os = "windows")]
        {
            let script = r#"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class SleepUtil {
  [DllImport("kernel32.dll", SetLastError = true)]
  public static extern uint SetThreadExecutionState(uint esFlags);
}
"@
[SleepUtil]::SetThreadExecutionState(0x80000000 -bor 0x00000001 -bor 0x00000002) | Out-Null
while ($true) { Start-Sleep -Seconds 60 }
"#;

            let child = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    script,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| {
                    AppError::Command(format!("Failed to start PowerShell sleep blocker: {error}"))
                })?;
            *slot = Some(child);
            return Ok("Sleep prevention enabled using SetThreadExecutionState".to_string());
        }

        #[allow(unreachable_code)]
        Err(AppError::Command(
            "Sleep prevention is not supported on this platform".to_string(),
        ))
    }
}
