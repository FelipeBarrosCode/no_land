use std::{fs, path::Path};

use crate::errors::{AppError, AppResult};

use super::session::TunnelSession;

pub fn render_server_config_text(
    server_tunnel_ip: &str,
    listen_port: u16,
    server_private_key: &str,
    mtu: u16,
    client_public_key: &str,
    client_tunnel_ip: &str,
) -> String {
    format!(
        "[Interface]\nAddress = {}\nListenPort = {}\nPrivateKey = {}\nMTU = {}\n\n[Peer]\nPublicKey = {}\nAllowedIPs = {}\n",
        server_tunnel_ip,
        listen_port,
        server_private_key,
        mtu,
        client_public_key,
        client_tunnel_ip,
    )
}

pub fn render_client_config_text(
    client_tunnel_ip: &str,
    client_private_key: &str,
    client_listen_port: u16,
    mtu: u16,
    server_public_key: &str,
    endpoint_host: &str,
    endpoint_port: u16,
    allowed_ips: &str,
    persistent_keepalive_secs: u16,
) -> String {
    format!(
        "[Interface]\nAddress = {}\nPrivateKey = {}\nListenPort = {}\nMTU = {}\n\n[Peer]\nPublicKey = {}\nEndpoint = {}:{}\nAllowedIPs = {}\nPersistentKeepalive = {}\n",
        client_tunnel_ip,
        client_private_key,
        client_listen_port,
        mtu,
        server_public_key,
        endpoint_host,
        endpoint_port,
        allowed_ips,
        persistent_keepalive_secs,
    )
}

pub fn parse_tunnel_session_from_file(
    config_path: &Path,
    instance_id: Option<u64>,
    interface_name: &str,
    sunshine_host: &str,
    sunshine_port: u16,
) -> AppResult<TunnelSession> {
    let config_text = fs::read_to_string(config_path).map_err(|error| {
        AppError::Command(format!(
            "Failed reading WireGuard client config {}: {error}",
            config_path.display()
        ))
    })?;

    parse_tunnel_session(
        &config_text,
        config_path,
        instance_id,
        interface_name,
        sunshine_host,
        sunshine_port,
    )
}

pub fn parse_tunnel_session(
    config_text: &str,
    config_path: &Path,
    instance_id: Option<u64>,
    interface_name: &str,
    sunshine_host: &str,
    sunshine_port: u16,
) -> AppResult<TunnelSession> {
    let client_tunnel_ip = required_value(config_text, "Interface", "Address")?;
    let client_private_key = required_value(config_text, "Interface", "PrivateKey")?;
    let mtu = required_value(config_text, "Interface", "MTU")?
        .parse::<u16>()
        .map_err(|error| AppError::InvalidInput(format!("Invalid MTU in WireGuard config: {error}")))?;
    let server_public_key = required_value(config_text, "Peer", "PublicKey")?;
    let allowed_ips = required_value(config_text, "Peer", "AllowedIPs")?;
    let keepalive = required_value(config_text, "Peer", "PersistentKeepalive")?
        .parse::<u16>()
        .map_err(|error| {
            AppError::InvalidInput(format!(
                "Invalid PersistentKeepalive in WireGuard config: {error}"
            ))
        })?;
    let endpoint = required_value(config_text, "Peer", "Endpoint")?;
    let (endpoint_host, endpoint_port) = split_endpoint(&endpoint)?;
    let client_public_key = derive_public_key(&client_private_key)?;
    let server_tunnel_ip = strip_cidr(
        allowed_ips
            .split(',')
            .next()
            .map(str::trim)
            .unwrap_or_default(),
    );

    Ok(TunnelSession {
        tunnel_id: format!(
            "instance-{}-{}",
            instance_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            interface_name
        ),
        instance_id,
        interface_name: interface_name.to_string(),
        client_tunnel_ip: strip_cidr(&client_tunnel_ip),
        server_tunnel_ip,
        client_public_key,
        server_public_key,
        endpoint_host,
        endpoint_port,
        allowed_ips,
        mtu,
        persistent_keepalive_secs: keepalive,
        sunshine_host: sunshine_host.to_string(),
        sunshine_port,
        config_path: config_path.to_path_buf(),
        config_text: config_text.to_string(),
        client_private_key,
    })
}

fn required_value(config_text: &str, section: &str, key: &str) -> AppResult<String> {
    parse_config_value(config_text, section, key).ok_or_else(|| {
        AppError::InvalidInput(format!(
            "WireGuard config is missing {key} in [{section}]"
        ))
    })
}

fn parse_config_value(config_text: &str, section: &str, key: &str) -> Option<String> {
    let mut current_section = String::new();
    for line in config_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = trimmed[1..trimmed.len() - 1].to_string();
            continue;
        }
        if !current_section.eq_ignore_ascii_case(section) {
            continue;
        }
        let (line_key, line_value) = trimmed.split_once('=')?;
        if line_key.trim().eq_ignore_ascii_case(key) {
            return Some(line_value.trim().to_string());
        }
    }
    None
}

fn split_endpoint(endpoint: &str) -> AppResult<(String, u16)> {
    let (host, port) = endpoint.rsplit_once(':').ok_or_else(|| {
        AppError::InvalidInput(format!("Invalid WireGuard endpoint value: {endpoint}"))
    })?;
    let endpoint_port = port.parse::<u16>().map_err(|error| {
        AppError::InvalidInput(format!("Invalid WireGuard endpoint port: {error}"))
    })?;
    Ok((host.trim().to_string(), endpoint_port))
}

fn strip_cidr(value: &str) -> String {
    value.split('/').next().unwrap_or(value).trim().to_string()
}

fn derive_public_key(private_key: &str) -> AppResult<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("wg")
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::Command(format!("Failed to spawn wg pubkey: {error}")))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(private_key.as_bytes()).map_err(|error| {
            AppError::Command(format!("Failed writing to wg pubkey stdin: {error}"))
        })?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| AppError::Command(format!("Failed reading wg pubkey output: {error}")))?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "wg pubkey failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
