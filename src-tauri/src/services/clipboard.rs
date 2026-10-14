use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use arboard::Clipboard;

use crate::errors::{AppError, AppResult};
use crate::moonlight::domain::SessionState;

use super::{app_context::AppContext, remote_exec::RemoteExec};

const MAX_CLIPBOARD_BYTES: usize = 1_048_576;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll the native clipboard, not the webview: copies made in other applications
/// must reach the remote even while the stream window is unfocused.
pub async fn run_local_clipboard_sync(
    context: AppContext,
    mut session: tokio::sync::watch::Receiver<SessionState>,
    active_instance: Arc<Mutex<Option<u64>>>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut target: Option<(u64, RemoteExec)> = None;
    let mut last_sent: Option<String> = None;
    let mut retry_after = tokio::time::Instant::now();

    loop {
        tokio::select! {
            changed = session.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            _ = interval.tick() => {}
        }
        let state = session.borrow().clone();
        if !matches!(state, SessionState::Streaming | SessionState::Reconnecting) {
            target = None;
            last_sent = None;
            retry_after = tokio::time::Instant::now();
            continue;
        }
        if !matches!(state, SessionState::Streaming) {
            continue;
        }
        let Some(instance_id) = active_instance.lock().ok().and_then(|id| *id) else {
            continue;
        };
        if target.as_ref().is_some_and(|(id, _)| *id != instance_id) {
            target = None;
            last_sent = None;
            retry_after = tokio::time::Instant::now();
        }
        if tokio::time::Instant::now() < retry_after {
            continue;
        }
        // Empty, non-text, temporarily locked and oversized clipboards must not
        // interrupt streaming or clear the remote clipboard.
        let Ok(Ok(content)) = tokio::task::spawn_blocking(read_local_text).await else {
            continue;
        };
        if !should_send_text(&content, last_sent.as_deref()) {
            continue;
        }
        if target.is_none() {
            match crate::commands::build_remote_exec_for_instance(&context, instance_id).await {
                Ok(remote) => target = Some((instance_id, remote)),
                Err(error) => {
                    tracing::warn!(instance_id, %error, "Automatic clipboard sync could not resolve remote");
                    retry_after = tokio::time::Instant::now() + Duration::from_secs(3);
                    continue;
                }
            }
        }
        // Resolving SSH details can take time. Do not start a transfer after
        // disconnect, or to an instance replaced by another stream.
        if !matches!(*session.borrow(), SessionState::Streaming)
            || active_instance.lock().ok().and_then(|id| *id) != Some(instance_id)
        {
            continue;
        }
        let remote = target.as_ref().unwrap().1.clone();
        let pending = content.clone();
        match tokio::task::spawn_blocking(move || write_remote_text(&remote, pending)).await {
            Ok(Ok(())) => last_sent = Some(content),
            result => {
                tracing::warn!(
                    instance_id,
                    ?result,
                    "Automatic clipboard sync failed; will retry"
                );
                target = None;
                retry_after = tokio::time::Instant::now() + Duration::from_secs(3);
            }
        }
    }
}

fn should_send_text(content: &str, last_sent: Option<&str>) -> bool {
    !content.is_empty() && last_sent != Some(content)
}

const LINUX_SET_CLIPBOARD: &str = r#"
set -e
export DISPLAY="${DISPLAY:-:0}"
CLIPBOARD_USER="$(ps -eo user=,comm= | awk '$2 == "sunshine" { print $1; exit }')"
CLIPBOARD_USER="${CLIPBOARD_USER:-${SUDO_USER:-$(id -un)}}"
XAUTHORITY="${XAUTHORITY:-/etc/X11/.Xauthority-noland}"
CLIPBOARD_FILE="$(mktemp /tmp/noland-clipboard.XXXXXX)"
trap 'rm -f "$CLIPBOARD_FILE" "$CLIPBOARD_FILE.error"' EXIT
cat > "$CLIPBOARD_FILE"
chown "$CLIPBOARD_USER" "$CLIPBOARD_FILE"
run_clipboard() {
  # xclip/xsel detach after acquiring ownership. Redirect their inherited
  # descriptors so the clipboard owner does not keep the SSH session open.
  if runuser -u "$CLIPBOARD_USER" -- env DISPLAY="$DISPLAY" XAUTHORITY="$XAUTHORITY" nohup "$@" < "$CLIPBOARD_FILE" > /dev/null 2> "$CLIPBOARD_FILE.error"; then
    return 0
  else
    status=$?
    cat "$CLIPBOARD_FILE.error" >&2
    return "$status"
  fi
}
if command -v xclip >/dev/null 2>&1; then
  run_clipboard xclip -selection clipboard -in
elif command -v xsel >/dev/null 2>&1; then
  run_clipboard xsel --clipboard --input
else
  echo 'Remote clipboard needs xclip or xsel (install xclip during provisioning)' >&2
  exit 127
fi
"#;

const LINUX_GET_CLIPBOARD: &str = r#"
export DISPLAY="${DISPLAY:-:0}"
CLIPBOARD_USER="$(ps -eo user=,comm= | awk '$2 == "sunshine" { print $1; exit }')"
CLIPBOARD_USER="${CLIPBOARD_USER:-${SUDO_USER:-$(id -un)}}"
XAUTHORITY="${XAUTHORITY:-/etc/X11/.Xauthority-noland}"
run_clipboard() { runuser -u "$CLIPBOARD_USER" -- env DISPLAY="$DISPLAY" XAUTHORITY="$XAUTHORITY" "$@"; }
if command -v xclip >/dev/null 2>&1; then
  run_clipboard xclip -selection clipboard -out
elif command -v xsel >/dev/null 2>&1; then
  run_clipboard xsel --clipboard --output
else
  echo 'Remote clipboard needs xclip or xsel (install xclip during provisioning)' >&2
  exit 127
fi
"#;

const WINDOWS_SET_CLIPBOARD: &str = r#"powershell.exe -NoProfile -NonInteractive -Command "$ErrorActionPreference = 'Stop'; [Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); $text = [Console]::In.ReadToEnd(); Set-Clipboard -Value $text""#;
const WINDOWS_GET_CLIPBOARD: &str = r#"powershell.exe -NoProfile -NonInteractive -Command "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::Out.Write((Get-Clipboard -Raw))""#;

#[derive(Debug, Clone, Copy)]
enum RemotePlatform {
    Linux,
    Windows,
}

pub fn read_local_text() -> AppResult<String> {
    let mut clipboard = Clipboard::new()
        .map_err(|error| AppError::Command(format!("Could not open local clipboard: {error}")))?;
    let content = clipboard.get_text().map_err(|error| {
        AppError::Command(format!(
            "Local clipboard does not contain readable text: {error}"
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_sends_initial_and_changed_text_but_not_duplicates_or_empty_text() {
        assert!(should_send_text("copied", None));
        assert!(should_send_text("new copy", Some("copied")));
        assert!(!should_send_text("copied", Some("copied")));
        assert!(!should_send_text("", None));
        assert!(!should_send_text("", Some("copied")));
        assert!(should_send_text(" \n", Some("copied")));
    }

    #[test]
    fn clipboard_limit_counts_utf8_bytes() {
        assert!(validate_text(&"a".repeat(MAX_CLIPBOARD_BYTES)).is_ok());
        assert!(validate_text(&"a".repeat(MAX_CLIPBOARD_BYTES + 1)).is_err());
        assert!(validate_text(&"é".repeat(MAX_CLIPBOARD_BYTES / 2 + 1)).is_err());
    }
}
