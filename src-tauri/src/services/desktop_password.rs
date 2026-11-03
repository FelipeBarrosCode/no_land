use std::time::Duration;

use crate::errors::{AppError, AppResult};

use super::{
    app_context::AppContext, platform_credentials::validate_platform_password,
    remote_exec::RemoteExec,
};

const SCRIPT: &str = include_str!("../../scripts/set_desktop_password.sh");

/// Only initial/incomplete setup opts into automatic application. Older usable
/// machines and already-applied passwords require the explicit settings action.
pub async fn provision(
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
) -> AppResult<()> {
    {
        let state = context.state.read().await;
        let server = state
            .provisioned_servers
            .iter()
            .find(|s| s.instance_id == instance_id)
            .ok_or_else(|| {
                AppError::InvalidInput("Remote machine is not tracked locally".into())
            })?;
        if server.steps.desktop_password_configured
            || server.steps.sunshine_configured
            || server.steps.post_provision_completed
        {
            return Ok(());
        }
    }
    apply(context, remote, instance_id).await
}

/// Applies the saved platform password to just one configured desktop account.
/// No SSH configuration, username, home directory, or root password is changed.
pub async fn apply(context: &AppContext, remote: &RemoteExec, instance_id: u64) -> AppResult<()> {
    let password = {
        let state = context.state.read().await;
        if !state
            .provisioned_servers
            .iter()
            .any(|s| s.instance_id == instance_id)
        {
            return Err(AppError::InvalidInput(
                "Remote machine is not tracked locally".into(),
            ));
        }
        state.credentials.app_password.clone()
    };
    let target = &context.config.audio_target_user;
    validate_platform_password(&password).map_err(AppError::InvalidInput)?;
    let command = password_command(target, remote.is_root())?;
    // Move the secret into SSH stdin; never interpolate it into the command.
    let mut input = password.into_bytes();
    input.splice(..0, format!("{target}:").bytes());
    input.push(b'\n');
    let remote = remote.clone();
    let result = tokio::task::spawn_blocking(move || {
        remote.ssh_with_stdin(&command, input, Duration::from_secs(45))
    })
    .await
    .map_err(|_| AppError::Command("Remote desktop password task failed".into()))??;
    if result.status_code != 0 || result.stdout.trim() != "NOLAND_DESKTOP_PASSWORD_SET" {
        let message = match result.status_code {
            41 => "Desktop account is missing or privileged; root passwords cannot be changed",
            42 => "Remote password-management tools are missing",
            43 => "Linux rejected the desktop password; check the remote password policy",
            44 => "Could not verify that the desktop account has an unlocked password",
            _ => "Could not apply the desktop password; check SSH access and root or passwordless sudo privileges",
        };
        return Err(AppError::Provisioning(message.into()));
    }
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|s| s.instance_id == instance_id)
            {
                server.steps.desktop_password_configured = true;
            }
        })
        .await?;
    Ok(())
}

fn password_command(target: &str, root_ssh: bool) -> AppResult<String> {
    let bytes = target.as_bytes();
    if target == "root"
        || bytes.is_empty()
        || bytes.len() > 32
        || !(bytes[0].is_ascii_lowercase() || bytes[0] == b'_')
        || !bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(b))
    {
        return Err(AppError::InvalidInput(
            "Desktop account must be a non-root Linux username".into(),
        ));
    }
    let script = SCRIPT.replace('\'', "'\\''");
    // -k prevents cached sudo authentication from silently consuming no password.
    // Never use sudo -S: its input must not be mixed with the chpasswd record.
    let prefix = if root_ssh { "" } else { "sudo -n -k -- " };
    Ok(format!("{prefix}sh -c '{script}' sh '{target}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_root_and_command_injection_targets() {
        for target in [
            "",
            "root",
            "-user",
            "user:name",
            "user\nroot",
            "us'er",
            "user;id",
            "usér",
        ] {
            assert!(password_command(target, true).is_err());
        }
        assert!(password_command(&"a".repeat(33), true).is_err());
    }

    #[test]
    fn preserves_key_auth_and_keeps_sudo_off_password_stdin() {
        assert!(password_command("user", true)
            .unwrap()
            .starts_with("sh -c "));
        let command = password_command("user", false).unwrap();
        assert!(command.starts_with("sudo -n -k -- sh -c "));
        assert!(!command.contains("sshd_config"));
        assert!(!command.contains("usermod"));
    }
}
