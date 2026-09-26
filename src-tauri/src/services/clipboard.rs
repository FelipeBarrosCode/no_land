use std::time::Duration;

use arboard::Clipboard;

use crate::errors::{AppError, AppResult};

use super::remote_exec::RemoteExec;

const MAX_CLIPBOARD_BYTES: usize = 1_048_576;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

const LINUX_SET_CLIPBOARD: &str = r#"
export DISPLAY="${DISPLAY:-:0}"
if [ -z "${XAUTHORITY:-}" ] && [ -f "$HOME/.Xauthority" ]; then
  export XAUTHORITY="$HOME/.Xauthority"
fi
if command -v xclip >/dev/null 2>&1; then
  exec xclip -selection clipboard -in
elif command -v xsel >/dev/null 2>&1; then
  exec xsel --clipboard --input
else
  echo 'Remote clipboard needs xclip or xsel' >&2
  exit 127
fi
"#;

const LINUX_GET_CLIPBOARD: &str = r#"
export DISPLAY="${DISPLAY:-:0}"
if [ -z "${XAUTHORITY:-}" ] && [ -f "$HOME/.Xauthority" ]; then
  export XAUTHORITY="$HOME/.Xauthority"
fi
if command -v xclip >/dev/null 2>&1; then
  exec xclip -selection clipboard -out
elif command -v xsel >/dev/null 2>&1; then
  exec xsel --clipboard --output
else
  echo 'Remote clipboard needs xclip or xsel' >&2
  exit 127
fi
"#;

const WINDOWS_SET_CLIPBOARD: &str =
    r#"powershell.exe -NoProfile -NonInteractive -Command "$text = [Console]::In.ReadToEnd(); Set-Clipboard -Value $text""#;
const WINDOWS_GET_CLIPBOARD: &str =
    r#"powershell.exe -NoProfile -NonInteractive -Command "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::Out.Write((Get-Clipboard -Raw))""#;

#[derive(Debug, Clone, Copy)]
enum RemotePlatform {
    Linux,
    Windows,
}

pub fn read_local_text() -> AppResult<String> {
    let mut clipboard = Clipboard::new()
        .map_err(|error| AppError::Command(format!("Could not open local clipboard: {error}")))?;
    let content = clipboard.get_text().map_err(|error| {
        AppError::Command(format!("Local clipboard does not contain readable text: {error}"))
    })?;
    validate_text(&content)?;
    Ok(content)
}

pub fn write_local_text(content: &str) -> AppResult<()> {
    validate_text(content)?;
    let mut clipboard = Clipboard::new()
        .map_err(|error| AppError::Command(format!("Could not open local clipboard: {error}")))?;
    clipboard
        .set_text(content.to_owned())
        .map_err(|error| AppError::Command(format!("Could not write local clipboard: {error}")))
}

pub fn write_remote_text(remote: &RemoteExec, content: String) -> AppResult<()> {
    validate_text(&content)?;
    let command = match detect_remote_platform(remote)? {
        RemotePlatform::Linux => LINUX_SET_CLIPBOARD,
        RemotePlatform::Windows => WINDOWS_SET_CLIPBOARD,
    };
    let output = remote.ssh_with_stdin(command, content.into_bytes(), COMMAND_TIMEOUT)?;
    ensure_remote_success(output.status_code, &output.stderr, "write")
}

pub fn read_remote_text(remote: &RemoteExec) -> AppResult<String> {
    let command = match detect_remote_platform(remote)? {
        RemotePlatform::Linux => LINUX_GET_CLIPBOARD,
        RemotePlatform::Windows => WINDOWS_GET_CLIPBOARD,
    };
    let output = remote.ssh(command, COMMAND_TIMEOUT)?;
    ensure_remote_success(output.status_code, &output.stderr, "read")?;
    validate_text(&output.stdout)?;
    Ok(output.stdout)
}

fn detect_remote_platform(remote: &RemoteExec) -> AppResult<RemotePlatform> {
    let output = remote.ssh("uname -s", COMMAND_TIMEOUT)?;
    if output.status_code == 0 && output.stdout.to_ascii_lowercase().contains("linux") {
        Ok(RemotePlatform::Linux)
    } else {
        Ok(RemotePlatform::Windows)
    }
}

fn ensure_remote_success(status_code: i32, stderr: &str, action: &str) -> AppResult<()> {
    if status_code == 0 {
        return Ok(());
    }
    let detail = stderr.trim();
    Err(AppError::Command(if detail.is_empty() {
        format!("Could not {action} the remote clipboard (exit code {status_code})")
    } else {
        format!("Could not {action} the remote clipboard: {detail}")
    }))
}

fn validate_text(content: &str) -> AppResult<()> {
    if content.len() > MAX_CLIPBOARD_BYTES {
        return Err(AppError::InvalidInput(
            "Clipboard text is larger than the 1 MB MVP limit".to_string(),
        ));
    }
    Ok(())
}
