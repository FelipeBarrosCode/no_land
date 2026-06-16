use std::{io::Write, process::{Command, Stdio}, thread, time::Duration};

use crate::errors::{AppError, AppResult};

use super::super::core::session::TunnelSession;

#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxNativeDriver;

impl LinuxNativeDriver {
    pub fn start(&self, session: &TunnelSession) -> AppResult<()> {
        let existing = sudo_output(&["ip", "link", "show", &session.interface_name])?;
        if existing.status.success() {
            let show = sudo_output(&["wg", "show", &session.interface_name])?;
            let stdout = String::from_utf8_lossy(&show.stdout);
            if !stdout.contains(&session.server_public_key) {
                return Err(AppError::Command(format!(
                    "Interface {} already exists and is not owned by the current Noland tunnel",
                    session.interface_name
                )));
            }
            let _ = sudo_output(&["ip", "link", "delete", "dev", &session.interface_name])?;
            thread::sleep(Duration::from_millis(300));
        }

        let _ = sudo_output(&["ip", "link", "delete", "dev", &session.interface_name]);
        ensure_success(sudo_output(&[
            "ip",
            "link",
            "add",
            "dev",
            &session.interface_name,
            "type",
            "wireguard",
        ])?, "create WireGuard interface")?;

        ensure_success(
            sudo_wg_set(session)?,
            "configure WireGuard interface",
        )?;

        ensure_success(
            sudo_output(&[
                "ip",
                "address",
                "replace",
                &format!("{}/32", session.client_tunnel_ip),
                "dev",
                &session.interface_name,
            ])?,
            "assign WireGuard address",
        )?;

        ensure_success(
            sudo_output(&[
                "ip",
                "link",
                "set",
                "dev",
                &session.interface_name,
                "mtu",
                &session.mtu.to_string(),
                "up",
            ])?,
            "bring WireGuard interface up",
        )?;

        ensure_success(
            sudo_output(&[
                "ip",
                "route",
                "replace",
                &format!("{}/32", session.server_tunnel_ip),
                "dev",
                &session.interface_name,
            ])?,
            "install WireGuard route",
        )?;

        Ok(())
    }

    pub fn stop(&self, session: &TunnelSession) -> AppResult<()> {
        let _ = sudo_output(&["ip", "link", "delete", "dev", &session.interface_name])?;
        Ok(())
    }
}

fn sudo_output(args: &[&str]) -> AppResult<std::process::Output> {
    Command::new("sudo")
        .arg("-n")
        .args(args)
        .output()
        .map_err(|error| AppError::Command(format!("Failed to run sudo {}: {error}", args.join(" "))))
}

fn sudo_wg_set(session: &TunnelSession) -> AppResult<std::process::Output> {
    let mut child = Command::new("sudo")
        .arg("-n")
        .args([
            "wg",
            "set",
            session.interface_name.as_str(),
            "private-key",
            "/dev/stdin",
            "peer",
            session.server_public_key.as_str(),
            "endpoint",
            &format!("{}:{}", session.endpoint_host, session.endpoint_port),
            "allowed-ips",
            session.allowed_ips.as_str(),
            "persistent-keepalive",
            &session.persistent_keepalive_secs.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::Command(format!("Failed to spawn sudo wg set: {error}")))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(session.client_private_key.as_bytes()).map_err(|error| {
            AppError::Command(format!("Failed writing WireGuard private key to sudo wg set stdin: {error}"))
        })?;
    }

    child.wait_with_output().map_err(|error| {
        AppError::Command(format!("Failed waiting for sudo wg set output: {error}"))
    })
}

fn ensure_success(output: std::process::Output, action: &str) -> AppResult<()> {
    if output.status.success() {
        return Ok(());
    }

    Err(AppError::Command(format!(
        "Failed to {action} (exit {}): {}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}
