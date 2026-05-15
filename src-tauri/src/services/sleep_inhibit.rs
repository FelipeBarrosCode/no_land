use std::process::Command;

use crate::errors::{AppError, AppResult};

pub struct SleepInhibitService;

impl SleepInhibitService {
    pub fn ensure_active() -> AppResult<String> {
        if Self::is_active() {
            return Ok("Sleep prevention is already active on this client".to_string());
        }

        #[cfg(target_os = "macos")]
        {
            start_macos_terminal_inhibitor()?;
            return Ok("Sleep prevention enabled using caffeinate terminal session".to_string());
        }

        #[cfg(target_os = "linux")]
        {
            start_linux_terminal_inhibitor()?;
            return Ok("Sleep prevention enabled using Linux terminal session".to_string());
        }

        #[cfg(target_os = "windows")]
        {
            start_windows_terminal_inhibitor()?;
            return Ok("Sleep prevention enabled using PowerShell terminal session".to_string());
        }

        #[allow(unreachable_code)]
        Err(AppError::Command(
            "Sleep prevention is not supported on this platform".to_string(),
        ))
    }

    pub fn stop() -> AppResult<String> {
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("sh")
                .args(["-lc", "pkill -f 'caffeinate -dims' >/dev/null 2>&1 || true"])
                .status();
            return Ok("Sleep prevention stopped on macOS".to_string());
        }

        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("sh")
                .args([
                    "-lc",
                    "pkill -f 'systemd-inhibit --what=sleep:idle --why=Noland Moonlight streaming session bash -lc while true; do sleep 60; done' >/dev/null 2>&1 || true; pkill -f 'while true; do sleep 60; done # NOLAND_SLEEP_BLOCK' >/dev/null 2>&1 || true",
                ])
                .status();
            return Ok("Sleep prevention stopped on Linux".to_string());
        }

        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*NOLAND_SLEEP_BLOCK*' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }",
                ])
                .status();
            return Ok("Sleep prevention stopped on Windows".to_string());
        }

        #[allow(unreachable_code)]
        Err(AppError::Command(
            "Sleep prevention stop is not supported on this platform".to_string(),
        ))
    }

    pub fn is_active() -> bool {
        #[cfg(target_os = "macos")]
        {
            return Command::new("sh")
                .args(["-lc", "pgrep -f 'caffeinate -dims' >/dev/null 2>&1"])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
        }

        #[cfg(target_os = "linux")]
        {
            return Command::new("sh")
                .args([
                    "-lc",
                    "pgrep -f 'systemd-inhibit --what=sleep:idle --why=Noland Moonlight streaming session bash -lc while true; do sleep 60; done' >/dev/null 2>&1 || pgrep -f 'while true; do sleep 60; done # NOLAND_SLEEP_BLOCK' >/dev/null 2>&1",
                ])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
        }

        #[cfg(target_os = "windows")]
        {
            let output = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    "$p = Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*NOLAND_SLEEP_BLOCK*' }; if ($p) { exit 0 } else { exit 1 }",
                ])
                .status();
            return output.map(|status| status.success()).unwrap_or(false);
        }

        #[allow(unreachable_code)]
        false
    }
}

#[cfg(target_os = "macos")]
fn start_macos_terminal_inhibitor() -> AppResult<()> {
    let command = "caffeinate -dims >/dev/null 2>&1 # NOLAND_SLEEP_BLOCK";
    let script = format!(
        "tell application \"Terminal\" to do script \"{}\"",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|error| AppError::Command(format!("Failed to launch Terminal: {error}")))?;
    if !output.status.success() {
        return Err(AppError::Command(format!(
            "Failed to start caffeinate terminal session: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn start_linux_terminal_inhibitor() -> AppResult<()> {
    let _ = Command::new("sh")
        .args(["-lc", "xset s off -dpms >/dev/null 2>&1 || true"])
        .status();

    let payload = if command_exists("systemd-inhibit") {
        "systemd-inhibit --what=sleep:idle --why='Noland Moonlight streaming session' bash -lc 'while true; do sleep 60; done' # NOLAND_SLEEP_BLOCK"
    } else {
        "bash -lc 'while true; do sleep 60; done' # NOLAND_SLEEP_BLOCK"
    };

    let launchers = [
        format!(
            "gnome-terminal -- bash -lc \"{}\"",
            shell_escape_double_quotes(payload)
        ),
        format!(
            "konsole -e bash -lc \"{}\"",
            shell_escape_double_quotes(payload)
        ),
        format!(
            "xfce4-terminal -e \"bash -lc '{}\'\"",
            payload.replace('"', "\\\"")
        ),
        format!(
            "x-terminal-emulator -e bash -lc \"{}\"",
            shell_escape_double_quotes(payload)
        ),
        format!(
            "xterm -e bash -lc \"{}\"",
            shell_escape_double_quotes(payload)
        ),
    ];

    for launcher in launchers {
        let status = Command::new("sh").args(["-lc", &launcher]).status();
        if status.map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }
    }

    Err(AppError::Command(
        "Could not launch a secondary terminal for sleep prevention on Linux. Install gnome-terminal, konsole, xfce4-terminal, xterm, or x-terminal-emulator."
            .to_string(),
    ))
}

#[cfg(target_os = "windows")]
fn start_windows_terminal_inhibitor() -> AppResult<()> {
    let script = r#"powershell -NoProfile -ExecutionPolicy Bypass -Command "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class SleepUtil { [DllImport(\"kernel32.dll\", SetLastError=true)] public static extern uint SetThreadExecutionState(uint esFlags); }'; [SleepUtil]::SetThreadExecutionState(0x80000000 -bor 0x00000001 -bor 0x00000002) | Out-Null; while ($true) { Start-Sleep -Seconds 60 } # NOLAND_SLEEP_BLOCK""#;
    let output = Command::new("cmd")
        .args([
            "/C",
            "start",
            "Noland Sleep Prevention",
            "cmd",
            "/K",
            script,
        ])
        .output()
        .map_err(|error| AppError::Command(format!("Failed to launch terminal: {error}")))?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "Failed to start sleep prevention terminal session: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn command_exists(command: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn shell_escape_double_quotes(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
