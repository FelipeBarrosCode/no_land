use std::{
    fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    errors::{AppError, AppResult},
    moonlight::infrastructure::persistence::atomic_file::write_atomically,
};

use super::{remote_exec::RemoteExec, wireguard::reconnect_local_wireguard_client};

pub(super) const BOOTSTRAP_TUNNEL_MTU: u16 = 1440;

const MIN_TUNNEL_MTU: u16 = 1280;
const MAX_TUNNEL_MTU: u16 = 1420;
const SAFETY_MARGIN: u16 = 16;
const PROBE_COUNT: u8 = 4;
const MAX_ACCEPTABLE_LOSS_PERCENT: f32 = 5.0;
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const CACHE_SCHEMA_VERSION: u8 = 2;

#[derive(Debug, Clone)]
pub(super) struct TunnelMtuSelection {
    pub mtu: u16,
    pub path_mtu: Option<u16>,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
struct PendingSelection {
    selection: TunnelMtuSelection,
    fingerprint: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MtuCacheEntry {
    schema_version: u8,
    fingerprint: String,
    path_mtu: u16,
    tunnel_mtu: u16,
    tested_at_unix: u64,
}

#[derive(Debug)]
struct OuterPathIdentity {
    destination: IpAddr,
    source: IpAddr,
    interface: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn tune_connected_tunnel(
    config_path: PathBuf,
    remote: RemoteExec,
    remote_interface: String,
    endpoint_host: String,
    tunnel_host: String,
    fallback_mtu: u16,
) -> AppResult<TunnelMtuSelection> {
    let cache_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("mtu-selection.json");
    let selection_cache_path = cache_path.clone();
    let pending = tokio::task::spawn_blocking(move || {
        select_connected_tunnel_mtu(
            &selection_cache_path,
            &endpoint_host,
            &tunnel_host,
            fallback_mtu,
        )
    })
    .await
    .map_err(|error| AppError::Command(format!("WireGuard MTU probe task failed: {error}")))?;

    let current_mtu = read_config_mtu(&config_path).unwrap_or(BOOTSTRAP_TUNNEL_MTU);
    if current_mtu != pending.selection.mtu {
        apply_remote_mtu(&remote, &remote_interface, pending.selection.mtu).await?;
        write_local_config_mtu(&config_path, pending.selection.mtu)?;
        reconnect_local_wireguard_client(&config_path)?;
    }

    if let (Some(path_mtu), Some(fingerprint)) = (pending.selection.path_mtu, pending.fingerprint) {
        persist_cache(
            &cache_path,
            MtuCacheEntry {
                schema_version: CACHE_SCHEMA_VERSION,
                fingerprint,
                path_mtu,
                tunnel_mtu: pending.selection.mtu,
                tested_at_unix: unix_now(),
            },
        );
    }

    Ok(pending.selection)
}

fn select_connected_tunnel_mtu(
    cache_path: &Path,
    endpoint_host: &str,
    tunnel_host: &str,
    fallback_mtu: u16,
) -> PendingSelection {
    let outer_path = resolve_outer_path(endpoint_host);
    let fingerprint = outer_path
        .as_ref()
        .map(|path| path_fingerprint(endpoint_host, path));
    if let Some(cached) = fingerprint
        .as_deref()
        .and_then(|fingerprint| load_cache(cache_path, fingerprint, unix_now()))
    {
        return PendingSelection {
            selection: TunnelMtuSelection {
                mtu: cached.tunnel_mtu,
                path_mtu: Some(cached.path_mtu),
                source: "cache",
            },
            fingerprint,
        };
    }

    let Some(tunnel_destination) = resolve_ip(tunnel_host) else {
        tracing::warn!(
            host = tunnel_host,
            "could not resolve connected WireGuard probe target; selecting fallback MTU"
        );
        return fallback_selection(fallback_mtu);
    };
    let path_mtu = search_path_mtu(BOOTSTRAP_TUNNEL_MTU, |candidate| {
        probe_candidate(tunnel_destination, candidate)
    });
    let Some(path_mtu) = path_mtu else {
        tracing::warn!(
            host = %tunnel_destination,
            "connected WireGuard path did not produce reliable DF probe responses; selecting fallback MTU"
        );
        return fallback_selection(fallback_mtu);
    };

    PendingSelection {
        selection: TunnelMtuSelection {
            mtu: operational_mtu(path_mtu, fallback_mtu),
            path_mtu: Some(path_mtu),
            source: "connected-probe",
        },
        fingerprint,
    }
}

fn fallback_selection(fallback_mtu: u16) -> PendingSelection {
    PendingSelection {
        selection: TunnelMtuSelection {
            mtu: fallback_mtu,
            path_mtu: None,
            source: "fallback",
        },
        fingerprint: None,
    }
}

fn operational_mtu(path_mtu: u16, fallback_mtu: u16) -> u16 {
    let bounded = path_mtu
        .saturating_sub(SAFETY_MARGIN)
        .clamp(fallback_mtu, MAX_TUNNEL_MTU);
    bounded - (bounded % 4)
}

async fn apply_remote_mtu(remote: &RemoteExec, remote_interface: &str, mtu: u16) -> AppResult<()> {
    if !remote_interface
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(AppError::Command(
            "WireGuard interface name is invalid during MTU update".to_string(),
        ));
    }
    let script = format!(
        "sudo sed -i -E 's/^[[:space:]]*MTU[[:space:]]*=.*/MTU = {mtu}/' /etc/wireguard/{remote_interface}.conf && sudo ip link set dev {remote_interface} mtu {mtu}"
    );
    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || remote.ssh(&script, Duration::from_secs(30)))
        .await
        .map_err(|error| AppError::Command(format!("Remote MTU update task failed: {error}")))??;
    if output.status_code != 0 {
        return Err(AppError::Command(format!(
            "Could not apply WireGuard MTU {mtu} on the instance: {}",
            output.stderr.trim()
        )));
    }
    Ok(())
}

fn write_local_config_mtu(config_path: &Path, mtu: u16) -> AppResult<()> {
    let config = fs::read_to_string(config_path)?;
    let updated = replace_interface_mtu(&config, mtu).ok_or_else(|| {
        AppError::Command(format!(
            "WireGuard config {} has no Interface MTU field",
            config_path.display()
        ))
    })?;
    write_atomically(config_path, updated.as_bytes()).map_err(|error| {
        AppError::Command(format!(
            "Could not persist selected WireGuard MTU in {}: {error}",
            config_path.display()
        ))
    })
}

fn replace_interface_mtu(config: &str, mtu: u16) -> Option<String> {
    let mut in_interface = false;
    let mut replaced = false;
    let mut lines = Vec::new();
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_interface = trimmed[1..trimmed.len() - 1].eq_ignore_ascii_case("interface");
        }
        if in_interface
            && trimmed
                .split_once('=')
                .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("mtu"))
        {
            lines.push(format!("MTU = {mtu}"));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    replaced.then(|| format!("{}\n", lines.join("\n")))
}

fn read_config_mtu(config_path: &Path) -> Option<u16> {
    let config = fs::read_to_string(config_path).ok()?;
    let mut in_interface = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_interface = trimmed[1..trimmed.len() - 1].eq_ignore_ascii_case("interface");
            continue;
        }
        if !in_interface {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("mtu") {
            return value.trim().parse().ok();
        }
    }
    None
}

fn resolve_outer_path(endpoint_host: &str) -> Option<OuterPathIdentity> {
    let destination = resolve_ip(endpoint_host)?;
    let bind = match destination {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(SocketAddr::new(destination, 9)).ok()?;
    let source = socket.local_addr().ok()?.ip();
    Some(OuterPathIdentity {
        destination,
        source,
        interface: interface_for_ip(source),
    })
}

fn resolve_ip(host: &str) -> Option<IpAddr> {
    let host = host
        .trim()
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host.trim());
    host.parse::<IpAddr>().ok().or_else(|| {
        (host, 9)
            .to_socket_addrs()
            .ok()?
            .next()
            .map(|address| address.ip())
    })
}

fn search_path_mtu(mut upper_bound: u16, mut probe: impl FnMut(u16) -> bool) -> Option<u16> {
    upper_bound -= upper_bound % 4;
    if upper_bound < MIN_TUNNEL_MTU || !probe(MIN_TUNNEL_MTU) {
        return None;
    }
    let candidates = (MIN_TUNNEL_MTU..=upper_bound)
        .step_by(4)
        .collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = candidates.len().saturating_sub(1);
    while low < high {
        let midpoint = low + (high - low).div_ceil(2);
        if probe(candidates[midpoint]) {
            low = midpoint;
        } else {
            high = midpoint - 1;
        }
    }
    Some(candidates[low])
}

fn probe_candidate(destination: IpAddr, packet_size: u16) -> bool {
    let header_size = if destination.is_ipv6() { 48 } else { 28 };
    let Some(payload_size) = packet_size.checked_sub(header_size) else {
        return false;
    };
    let mut command = ping_command(destination, payload_size);
    command.stdin(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let Ok(output) = command.output() else {
        return false;
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.status.success()
        && parse_loss_percent(&combined).is_some_and(|loss| loss <= MAX_ACCEPTABLE_LOSS_PERCENT)
}

fn ping_command(destination: IpAddr, payload_size: u16) -> Command {
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("ping.exe");
        command.args([
            if destination.is_ipv6() { "-6" } else { "-4" },
            "-n",
            &PROBE_COUNT.to_string(),
            "-w",
            "1000",
        ]);
        if destination.is_ipv4() {
            command.arg("-f");
        }
        command.args(["-l", &payload_size.to_string(), &destination.to_string()]);
        return command;
    }

    #[cfg(target_os = "macos")]
    {
        let executable = if destination.is_ipv6() {
            "/sbin/ping6"
        } else {
            "/sbin/ping"
        };
        let mut command = Command::new(executable);
        command.args(["-c", &PROBE_COUNT.to_string(), "-i", "0.2", "-W", "1000"]);
        if destination.is_ipv4() {
            command.arg("-D");
        }
        command.args(["-s", &payload_size.to_string(), &destination.to_string()]);
        return command;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("ping");
        command.args([if destination.is_ipv6() { "-6" } else { "-4" }, "-M", "do"]);
        command.args([
            "-c",
            &PROBE_COUNT.to_string(),
            "-i",
            "0.2",
            "-W",
            "1",
            "-s",
            &payload_size.to_string(),
            &destination.to_string(),
        ]);
        return command;
    }

    #[allow(unreachable_code)]
    Command::new("ping")
}

fn parse_loss_percent(output: &str) -> Option<f32> {
    for (percent_index, _) in output.match_indices('%') {
        let prefix = &output[..percent_index];
        let number = prefix
            .chars()
            .rev()
            .take_while(|character| character.is_ascii_digit() || *character == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        if let Ok(value) = number.parse() {
            return Some(value);
        }
    }
    None
}

fn path_fingerprint(endpoint_host: &str, path: &OuterPathIdentity) -> String {
    let mut hasher = Sha256::new();
    for value in [
        endpoint_host.trim().to_ascii_lowercase(),
        path.destination.to_string(),
        path.source.to_string(),
        path.interface.clone().unwrap_or_default(),
    ] {
        hasher.update(value.len().to_le_bytes());
        hasher.update(value.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn load_cache(path: &Path, fingerprint: &str, now: u64) -> Option<MtuCacheEntry> {
    let contents = fs::read(path).ok()?;
    if contents.len() > 64 * 1024 {
        return None;
    }
    let entry: MtuCacheEntry = serde_json::from_slice(&contents).ok()?;
    if entry.schema_version != CACHE_SCHEMA_VERSION
        || entry.fingerprint != fingerprint
        || now.saturating_sub(entry.tested_at_unix) > CACHE_TTL.as_secs()
        || !(MIN_TUNNEL_MTU..=BOOTSTRAP_TUNNEL_MTU).contains(&entry.path_mtu)
        || !(MIN_TUNNEL_MTU..=MAX_TUNNEL_MTU).contains(&entry.tunnel_mtu)
    {
        return None;
    }
    Some(entry)
}

fn persist_cache(path: &Path, entry: MtuCacheEntry) {
    if let Ok(contents) = serde_json::to_vec_pretty(&entry) {
        if let Err(error) = write_atomically(path, &contents) {
            tracing::warn!(path = %path.display(), %error, "failed to cache connected WireGuard MTU selection");
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(unix)]
fn interface_for_ip(source_ip: IpAddr) -> Option<String> {
    use std::{ffi::CStr, ptr};

    let mut interfaces: *mut libc::ifaddrs = ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut interfaces) } != 0 {
        return None;
    }
    let mut current = interfaces;
    let mut result = None;
    while !current.is_null() {
        let interface = unsafe { &*current };
        if !interface.ifa_addr.is_null() && !interface.ifa_name.is_null() {
            let family = unsafe { (*interface.ifa_addr).sa_family as i32 };
            let address = match family {
                libc::AF_INET => {
                    let address = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_in) };
                    Some(IpAddr::V4(Ipv4Addr::from(
                        address.sin_addr.s_addr.to_ne_bytes(),
                    )))
                }
                libc::AF_INET6 => {
                    let address = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_in6) };
                    Some(IpAddr::V6(Ipv6Addr::from(address.sin6_addr.s6_addr)))
                }
                _ => None,
            };
            if address == Some(source_ip) {
                result = unsafe { CStr::from_ptr(interface.ifa_name) }
                    .to_str()
                    .ok()
                    .map(str::to_owned);
                break;
            }
        }
        current = unsafe { (*current).ifa_next };
    }
    unsafe { libc::freeifaddrs(interfaces) };
    result
}

#[cfg(not(unix))]
fn interface_for_ip(_source_ip: IpAddr) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_search_finds_largest_reliable_inner_mtu() {
        let mut probes = Vec::new();
        let selected = search_path_mtu(BOOTSTRAP_TUNNEL_MTU, |candidate| {
            probes.push(candidate);
            candidate <= 1436
        });
        assert_eq!(selected, Some(1436));
        assert!(probes.len() <= 8);
    }

    #[test]
    fn connected_path_mtu_gets_safety_margin() {
        assert_eq!(operational_mtu(1440, 1280), 1420);
        assert_eq!(operational_mtu(1400, 1280), 1384);
        assert_eq!(operational_mtu(1280, 1280), 1280);
    }

    #[test]
    fn rewrites_only_interface_mtu() {
        let config = "[Interface]\nAddress = 10.77.0.2/32\nMTU = 1440\n\n[Peer]\nMTU = 999\n";
        let updated = replace_interface_mtu(config, 1384).unwrap();
        assert!(updated.contains("[Interface]\nAddress = 10.77.0.2/32\nMTU = 1384"));
        assert!(updated.contains("[Peer]\nMTU = 999"));
    }

    #[test]
    fn parses_unix_and_windows_packet_loss() {
        assert_eq!(
            parse_loss_percent("4 packets transmitted, 4 received, 0.0% packet loss"),
            Some(0.0)
        );
        assert_eq!(parse_loss_percent("Lost = 1 (25% loss)"), Some(25.0));
    }
}
