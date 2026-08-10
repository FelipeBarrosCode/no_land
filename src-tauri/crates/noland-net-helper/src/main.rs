use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use gotatun::{
    device::{DefaultDeviceTransports, Device, DeviceBuilder, Peer},
    tun::tun_async_device::TunDevice,
    x25519::{PublicKey, StaticSecret},
};
use ipnetwork::IpNetwork;
use serde::Serialize;
use tokio::time::sleep;
use tun::AbstractDevice;

#[cfg(not(target_os = "macos"))]
const INTERFACE_NAME: &str = "nolandwg0";
const STATUS_FILE_NAME: &str = "status.json";
const STOP_REQUEST_FILE_NAME: &str = "stop.request";
const STATUS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelperCommand {
    Run,
    Check,
}

#[derive(Debug)]
struct Args {
    command: HelperCommand,
    config_path: PathBuf,
    state_dir: PathBuf,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    wintun_path: Option<PathBuf>,
}

#[derive(Debug)]
struct TunnelConfig {
    private_key: [u8; 32],
    peer_public_key: [u8; 32],
    client_address: Ipv4Addr,
    client_prefix: u8,
    server_address: Ipv4Addr,
    allowed_ips: Vec<IpNetwork>,
    endpoint: SocketAddr,
    listen_port: u16,
    mtu: u16,
    keepalive: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus {
    engine: &'static str,
    active: bool,
    pid: u32,
    interface_name: String,
    config_path: String,
    peer_public_key: String,
    allowed_ips: Vec<String>,
    endpoint: String,
    latest_handshake_age_secs: Option<u64>,
    rx_bytes: u64,
    tx_bytes: u64,
    updated_at_unix: u64,
    error: Option<String>,
}

impl RuntimeStatus {
    fn starting(args: &Args) -> Self {
        Self {
            engine: "gotatun-embedded-0.7.1",
            active: false,
            pid: process::id(),
            interface_name: String::new(),
            config_path: args.config_path.display().to_string(),
            peer_public_key: String::new(),
            allowed_ips: Vec::new(),
            endpoint: String::new(),
            latest_handshake_age_secs: None,
            rx_bytes: 0,
            tx_bytes: 0,
            updated_at_unix: unix_timestamp(),
            error: None,
        }
    }
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("noland-net-helper: {error:#}");
            process::exit(2);
        }
    };

    if args.command == HelperCommand::Check {
        match parse_tunnel_config(&args.config_path) {
            Ok(config) => {
                println!(
                    "noland-net-helper: configuration valid (endpoint={}, allowed_ips={})",
                    config.endpoint,
                    config.allowed_ips.len()
                );
                return;
            }
            Err(error) => {
                eprintln!("noland-net-helper: invalid configuration: {error:#}");
                process::exit(1);
            }
        }
    }

    if let Err(error) = fs::create_dir_all(&args.state_dir) {
        eprintln!(
            "noland-net-helper: failed creating state directory {}: {error}",
            args.state_dir.display()
        );
        process::exit(1);
    }

    let stop_request_path = args.state_dir.join(STOP_REQUEST_FILE_NAME);
    let _ = fs::remove_file(&stop_request_path);

    if let Err(error) = run(args).await {
        eprintln!("noland-net-helper: {error:#}");
        process::exit(1);
    }
}

async fn run(args: Args) -> Result<()> {
    let status_path = args.state_dir.join(STATUS_FILE_NAME);
    let mut initial_status = RuntimeStatus::starting(&args);
    write_status(&status_path, &initial_status)?;

    let result = run_tunnel(&args, &status_path).await;
    if let Err(error) = &result {
        initial_status.error = Some(format!("{error:#}"));
        initial_status.updated_at_unix = unix_timestamp();
        let _ = write_status(&status_path, &initial_status);
    }
    result
}

async fn run_tunnel(args: &Args, status_path: &Path) -> Result<()> {
    let tunnel_config = parse_tunnel_config(&args.config_path)?;
    let mut tun_config = tun::Configuration::default();

    #[cfg(not(target_os = "macos"))]
    tun_config.tun_name(INTERFACE_NAME);

    tun_config
        .address(tunnel_config.client_address)
        .destination(tunnel_config.server_address)
        .netmask(prefix_to_netmask(tunnel_config.client_prefix))
        .mtu(tunnel_config.mtu)
        .up();

    #[cfg(target_os = "linux")]
    tun_config.platform_config(|platform| {
        platform.ensure_root_privileges(true);
    });

    #[cfg(target_os = "macos")]
    tun_config.platform_config(|platform| {
        platform.enable_routing(true);
    });

    #[cfg(target_os = "windows")]
    {
        let wintun_path = args.wintun_path.as_ref().ok_or_else(|| {
            anyhow!("the Windows GotaTun runtime requires the bundled wintun.dll path")
        })?;
        tun_config.platform_config(|platform| {
            platform.wintun_file(wintun_path.as_os_str());
        });
    }

    let async_tun =
        tun::create_as_async(&tun_config).context("failed creating managed TUN adapter")?;
    let interface_name = async_tun
        .tun_name()
        .context("failed resolving managed TUN adapter name")?;
    let gotatun_tun =
        TunDevice::from_tun_device(async_tun).context("failed attaching GotaTun to TUN adapter")?;

    let peer_key = PublicKey::from(tunnel_config.peer_public_key);
    let mut peer = Peer::new(peer_key)
        .with_endpoint(tunnel_config.endpoint)
        .with_allowed_ips(tunnel_config.allowed_ips.clone());
    peer.keepalive = tunnel_config.keepalive;

    let mut device = DeviceBuilder::new()
        .with_private_key(StaticSecret::from(tunnel_config.private_key))
        .with_default_udp()
        .udp_recv_buffer_size(7 * 1024 * 1024)
        .udp_send_buffer_size(7 * 1024 * 1024)
        .with_ip(gotatun_tun)
        .with_listen_port(tunnel_config.listen_port)
        .with_peer(peer)
        .build()
        .await
        .context("failed starting embedded GotaTun device")?;

    let mut status = RuntimeStatus {
        engine: "gotatun-embedded-0.7.1",
        active: true,
        pid: process::id(),
        interface_name,
        config_path: args.config_path.display().to_string(),
        peer_public_key: BASE64_STANDARD.encode(tunnel_config.peer_public_key),
        allowed_ips: tunnel_config
            .allowed_ips
            .iter()
            .map(ToString::to_string)
            .collect(),
        endpoint: tunnel_config.endpoint.to_string(),
        latest_handshake_age_secs: None,
        rx_bytes: 0,
        tx_bytes: 0,
        updated_at_unix: unix_timestamp(),
        error: None,
    };
    refresh_status_from_device(&device, &mut status).await;
    write_status(status_path, &status)?;

    let stop_request_path = args.state_dir.join(STOP_REQUEST_FILE_NAME);
    loop {
        tokio::select! {
            _ = device.wait() => {
                status.error = Some("embedded GotaTun device stopped after an unrecoverable runtime error".to_string());
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            _ = sleep(STATUS_INTERVAL) => {
                if stop_request_path.exists() {
                    break;
                }
                refresh_status_from_device(&device, &mut status).await;
                write_status(status_path, &status)?;
            }
        }
    }

    device.stop().await;
    status.active = false;
    status.updated_at_unix = unix_timestamp();
    write_status(status_path, &status)?;
    let _ = fs::remove_file(stop_request_path);
    Ok(())
}

async fn refresh_status_from_device(
    device: &Device<DefaultDeviceTransports>,
    status: &mut RuntimeStatus,
) {
    let peers = device.read(async |reader| reader.peers().await).await;
    if let Some(peer) = peers.first() {
        status.latest_handshake_age_secs = peer.stats.last_handshake.map(|age| age.as_secs());
        status.rx_bytes = peer.stats.rx_bytes as u64;
        status.tx_bytes = peer.stats.tx_bytes as u64;
    }
    status.updated_at_unix = unix_timestamp();
}

fn parse_args() -> Result<Args> {
    let mut values = std::env::args().skip(1);
    let command = match values.next().unwrap_or_default().as_str() {
        "run" => HelperCommand::Run,
        "check" => HelperCommand::Check,
        _ => bail!(
            "usage: noland-net-helper <run|check> --config <path> [--state-dir <path>] [--wintun <path>]"
        ),
    };

    let mut config_path = None;
    let mut state_dir = None;
    let mut wintun_path = None;
    while let Some(arg) = values.next() {
        match arg.as_str() {
            "--config" => config_path = values.next().map(PathBuf::from),
            "--state-dir" => state_dir = values.next().map(PathBuf::from),
            "--wintun" => wintun_path = values.next().map(PathBuf::from),
            other => bail!("unknown argument `{other}`"),
        }
    }

    let state_dir = match command {
        HelperCommand::Run => state_dir.ok_or_else(|| anyhow!("missing --state-dir path"))?,
        HelperCommand::Check => state_dir.unwrap_or_default(),
    };

    Ok(Args {
        command,
        config_path: config_path.ok_or_else(|| anyhow!("missing --config path"))?,
        state_dir,
        wintun_path,
    })
}

fn parse_tunnel_config(config_path: &Path) -> Result<TunnelConfig> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("failed reading {}", config_path.display()))?;

    let private_key = decode_key(required_value(&content, "Interface", "PrivateKey")?)?;
    let peer_public_key = decode_key(required_value(&content, "Peer", "PublicKey")?)?;
    let address = required_value(&content, "Interface", "Address")?;
    let (client_address, client_prefix) =
        parse_ipv4_cidr(address.split(',').next().unwrap_or(address))?;

    let allowed_ips_value = required_value(&content, "Peer", "AllowedIPs")?;
    let allowed_ips = allowed_ips_value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<IpNetwork>()
                .with_context(|| format!("invalid AllowedIPs entry `{value}`"))
        })
        .collect::<Result<Vec<_>>>()?;
    let server_address = allowed_ips
        .iter()
        .find_map(|network| match network.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .ok_or_else(|| anyhow!("WireGuard config has no IPv4 AllowedIPs entry"))?;

    let endpoint_value = required_value(&content, "Peer", "Endpoint")?;
    let endpoint = resolve_endpoint(endpoint_value)?;
    let listen_port = optional_value(&content, "Interface", "ListenPort")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let mtu = optional_value(&content, "Interface", "MTU")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(1280);
    let keepalive = optional_value(&content, "Peer", "PersistentKeepalive")
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value != 0);

    Ok(TunnelConfig {
        private_key,
        peer_public_key,
        client_address,
        client_prefix,
        server_address,
        allowed_ips,
        endpoint,
        listen_port,
        mtu,
        keepalive,
    })
}

fn required_value<'a>(content: &'a str, section: &str, key: &str) -> Result<&'a str> {
    optional_value(content, section, key)
        .ok_or_else(|| anyhow!("WireGuard config is missing [{section}] {key}"))
}

fn optional_value<'a>(content: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let target_section = format!("[{section}]");
    let mut in_section = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line.eq_ignore_ascii_case(&target_section);
            continue;
        }
        if !in_section {
            continue;
        }
        let (candidate, value) = line.split_once('=')?;
        if candidate.trim().eq_ignore_ascii_case(key) {
            return Some(value.trim());
        }
    }
    None
}

fn decode_key(value: &str) -> Result<[u8; 32]> {
    let decoded = BASE64_STANDARD
        .decode(value.trim())
        .context("WireGuard key is not valid base64")?;
    decoded
        .try_into()
        .map_err(|_| anyhow!("WireGuard keys must be exactly 32 bytes"))
}

fn parse_ipv4_cidr(value: &str) -> Result<(Ipv4Addr, u8)> {
    let (ip, prefix) = value
        .trim()
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid IPv4 CIDR `{value}`"))?;
    let ip = ip
        .parse::<Ipv4Addr>()
        .with_context(|| format!("invalid IPv4 address `{ip}`"))?;
    let prefix = prefix
        .parse::<u8>()
        .with_context(|| format!("invalid CIDR prefix `{prefix}`"))?;
    if prefix > 32 {
        bail!("invalid IPv4 CIDR prefix `{prefix}`");
    }
    Ok((ip, prefix))
}

fn resolve_endpoint(value: &str) -> Result<SocketAddr> {
    value
        .to_socket_addrs()
        .with_context(|| format!("failed resolving WireGuard endpoint `{value}`"))?
        .next()
        .ok_or_else(|| anyhow!("WireGuard endpoint `{value}` resolved to no addresses"))
}

fn prefix_to_netmask(prefix: u8) -> Ipv4Addr {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    };
    Ipv4Addr::from(mask)
}

fn write_status(path: &Path, status: &RuntimeStatus) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("status path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{STATUS_FILE_NAME}.tmp"));
    let serialized = serde_json::to_vec_pretty(status)?;
    fs::write(&temporary, serialized)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{parse_tunnel_config, prefix_to_netmask};
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use std::{fs, net::Ipv4Addr};

    #[test]
    fn parses_noland_wireguard_config() {
        let root =
            std::env::temp_dir().join(format!("noland-net-helper-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("nolandwg0.conf");
        let private_key = BASE64_STANDARD.encode([7u8; 32]);
        let peer_key = BASE64_STANDARD.encode([9u8; 32]);
        fs::write(
            &path,
            format!(
                "[Interface]\nAddress = 10.77.0.2/32\nPrivateKey = {private_key}\nListenPort = 51821\nMTU = 1280\n\n[Peer]\nPublicKey = {peer_key}\nEndpoint = 127.0.0.1:51820\nAllowedIPs = 10.77.0.1/32\nPersistentKeepalive = 25\n"
            ),
        )
        .unwrap();

        let config = parse_tunnel_config(&path).unwrap();
        assert_eq!(config.client_address, Ipv4Addr::new(10, 77, 0, 2));
        assert_eq!(config.client_prefix, 32);
        assert_eq!(config.server_address, Ipv4Addr::new(10, 77, 0, 1));
        assert_eq!(config.endpoint.to_string(), "127.0.0.1:51820");
        assert_eq!(config.listen_port, 51821);
        assert_eq!(config.mtu, 1280);
        assert_eq!(config.keepalive, Some(25));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn converts_cidr_prefix_to_ipv4_netmask() {
        assert_eq!(prefix_to_netmask(0), Ipv4Addr::new(0, 0, 0, 0));
        assert_eq!(prefix_to_netmask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(prefix_to_netmask(32), Ipv4Addr::new(255, 255, 255, 255));
    }
}
