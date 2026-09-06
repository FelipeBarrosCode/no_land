#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    fs::{self, File, OpenOptions},
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(all(unix, not(target_os = "linux")))]
use std::process::Command;
#[cfg(target_os = "windows")]
use std::{
    io::Write,
    process::{Command, Stdio},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use fs2::FileExt;
use gotatun::{
    device::{DefaultDeviceTransports, Device, DeviceBuilder, Peer},
    tun::tun_async_device::TunDevice,
    x25519::{PublicKey, StaticSecret},
};
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::sleep;
use tun::AbstractDevice;

#[cfg(not(target_os = "macos"))]
const INTERFACE_NAME: &str = "nolandwg0";
const STATUS_FILE_NAME: &str = "status.json";
const STOP_REQUEST_FILE_NAME: &str = "stop.request";
const OWNER_LOCK_FILE_NAME: &str = "owner.lock";
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
    launch_id: String,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    wintun_path: Option<PathBuf>,
}

#[derive(Debug)]
struct TunnelConfig {
    private_key: [u8; 32],
    peer_public_key: [u8; 32],
    client_address: Ipv4Addr,
    client_prefix: u8,
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    server_address: Ipv4Addr,
    allowed_ips: Vec<IpNetwork>,
    endpoint: SocketAddr,
    listen_port: u16,
    mtu: u16,
    keepalive: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus {
    engine: String,
    active: bool,
    pid: u32,
    interface_name: String,
    config_path: String,
    #[serde(default)]
    launch_id: String,
    peer_public_key: String,
    allowed_ips: Vec<String>,
    endpoint: String,
    #[serde(default)]
    config_fingerprint: String,
    #[serde(default)]
    listen_port: u16,
    latest_handshake_age_secs: Option<u64>,
    rx_bytes: u64,
    tx_bytes: u64,
    updated_at_unix: u64,
    error: Option<String>,
}

impl RuntimeStatus {
    fn starting(args: &Args, config_fingerprint: String) -> Self {
        Self {
            engine: "gotatun-embedded-0.7.1".to_string(),
            active: false,
            pid: process::id(),
            interface_name: String::new(),
            config_path: args.config_path.display().to_string(),
            launch_id: args.launch_id.clone(),
            peer_public_key: String::new(),
            allowed_ips: Vec::new(),
            endpoint: String::new(),
            config_fingerprint,
            listen_port: 0,
            latest_handshake_age_secs: None,
            rx_bytes: 0,
            tx_bytes: 0,
            updated_at_unix: unix_timestamp(),
            error: None,
        }
    }
}

/// Probe whether a UDP port can be bound on both wildcard IPv4 and IPv6 the way
/// GotaTun does (IPv4 first, then IPv6). Windows refuses binds inside
/// Hyper-V/WinNAT excluded port ranges with WSAEACCES even for administrators,
/// so probing is the only reliable availability check.
#[cfg(target_os = "windows")]
fn udp_port_probe_ok(port: u16) -> bool {
    let v4 = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port));
    if v4.is_err() {
        return false;
    }
    drop(v4);
    std::net::UdpSocket::bind((std::net::Ipv6Addr::UNSPECIFIED, port)).is_ok()
}

/// Resolve the listen port the embedded GotaTun engine should actually use on
/// Windows.
///
/// The configured `ListenPort` may sit inside a Windows excluded port range
/// (Hyper-V/WinNAT reserves ranges dynamically, e.g. for Docker Desktop), or be
/// held by another process. In those cases Windows fails the bind with
/// WSAEACCES (os error 10013) even for elevated processes, so GotaTun would
/// abort. If no `ListenPort` is configured, keep `0` so GotaTun asks the OS
/// for an ephemeral UDP source port. Otherwise walk upward from the configured
/// port for the first bindable candidate; as a last resort return 0 so GotaTun
/// picks a random port with its own IPv6-conflict retries. The WireGuard server
/// learns the client port from the handshake, so a different local port is
/// harmless.
///
/// On non-Windows platforms the configured port is used unchanged.
#[cfg(target_os = "windows")]
fn resolve_effective_listen_port(configured: u16) -> u16 {
    if configured == 0 {
        return 0;
    }

    if udp_port_probe_ok(configured) {
        return configured;
    }

    let start = u32::from(configured);
    for offset in 0..64u32 {
        let candidate = (start + offset) % 65536;
        if candidate == 0 {
            continue;
        }
        let port = candidate as u16;
        if port != configured && udp_port_probe_ok(port) {
            return port;
        }
    }

    0
}

#[cfg(target_os = "windows")]
struct WindowsAddressGuard {
    interface_name: String,
    address: Ipv4Addr,
}

#[cfg(target_os = "windows")]
impl Drop for WindowsAddressGuard {
    fn drop(&mut self) {
        remove_windows_adapter_address(&self.interface_name, self.address);
    }
}

#[cfg(target_os = "windows")]
struct WindowsRouteGuard {
    interface_name: String,
    routes: Vec<String>,
}

#[cfg(target_os = "windows")]
impl Drop for WindowsRouteGuard {
    fn drop(&mut self) {
        for route in self.routes.iter().rev() {
            remove_windows_route(&self.interface_name, route);
        }
    }
}

#[cfg(target_os = "windows")]
fn configure_hidden_windows_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(target_os = "windows")]
fn ensure_windows_gotatun_firewall_rule(listen_port: u16) -> Result<()> {
    const RULE_NAME: &str = "Noland GotaTun UDP";

    let mut delete = Command::new("netsh.exe");
    configure_hidden_windows_command(&mut delete);
    let _ = delete
        .args(["advfirewall", "firewall", "delete", "rule"])
        .arg(format!("name={RULE_NAME}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let mut add = Command::new("netsh.exe");
    configure_hidden_windows_command(&mut add);
    let output = add
        .args(["advfirewall", "firewall", "add", "rule"])
        .arg(format!("name={RULE_NAME}"))
        .args(["dir=in", "action=allow", "protocol=UDP"])
        .arg(format!("localport={listen_port}"))
        .args(["profile=any", "edge=yes", "enable=yes"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed launching netsh to configure the Noland GotaTun firewall rule")?;

    if !output.status.success() {
        bail!(
            "failed allowing inbound UDP port {listen_port} for the managed GotaTun tunnel in Windows Firewall: {} {}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn remove_windows_adapter_address(interface_name: &str, address: Ipv4Addr) {
    let mut command = Command::new("netsh.exe");
    configure_hidden_windows_command(&mut command);
    let _ = command
        .args(["interface", "ipv4", "delete", "address"])
        .arg(format!("name={interface_name}"))
        .arg(format!("address={address}"))
        .arg("gateway=all")
        .arg("store=active")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "windows")]
fn windows_adapter_has_address(interface_name: &str, address: Ipv4Addr) -> bool {
    let mut command = Command::new("netsh.exe");
    configure_hidden_windows_command(&mut command);
    command
        .args(["interface", "ipv4", "show", "addresses"])
        .arg(format!("name={interface_name}"))
        .stdin(Stdio::null())
        .output()
        .ok()
        .is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&address.to_string())
        })
}

#[cfg(target_os = "windows")]
fn windows_interfaces_with_address(address: Ipv4Addr) -> Vec<String> {
    let script = format!(
        "Get-NetIPAddress -IPAddress '{}' -AddressFamily IPv4 -ErrorAction SilentlyContinue | Select-Object -ExpandProperty InterfaceAlias",
        address
    );
    let mut command = Command::new("powershell.exe");
    configure_hidden_windows_command(&mut command);
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn stop_and_disable_legacy_wireguard_service(interface_name: &str) {
    let service_name = format!("WireGuardTunnel${interface_name}");

    let mut stop = Command::new("sc.exe");
    configure_hidden_windows_command(&mut stop);
    let _ = stop
        .args(["stop", &service_name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let mut disable = Command::new("sc.exe");
    configure_hidden_windows_command(&mut disable);
    let _ = disable
        .args(["config", &service_name, "start=", "disabled"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "windows")]
fn remove_windows_address_conflicts(expected_interface: &str, address: Ipv4Addr) {
    for interface_name in windows_interfaces_with_address(address) {
        if !interface_name.eq_ignore_ascii_case(expected_interface) {
            eprintln!(
                "noland-net-helper: removing reserved tunnel address {address} from conflicting Windows interface {interface_name}"
            );
            stop_and_disable_legacy_wireguard_service(&interface_name);
        }

        remove_windows_adapter_address(&interface_name, address);
    }

    for _ in 0..20 {
        if windows_interfaces_with_address(address)
            .iter()
            .all(|interface_name| interface_name.eq_ignore_ascii_case(expected_interface))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "windows")]
fn reset_windows_adapter_address(interface_name: &str) {
    let mut command = Command::new("netsh.exe");
    configure_hidden_windows_command(&mut command);
    let _ = command
        .args(["interface", "ipv4", "set", "address"])
        .arg(format!("name={interface_name}"))
        .arg("source=dhcp")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "windows")]
fn configure_windows_adapter_address(
    interface_name: &str,
    address: Ipv4Addr,
    prefix: u8,
) -> Result<WindowsAddressGuard> {
    // tun 0.8.6 applies the address twice on Windows. Creating the adapter
    // without address fields and configuring it here avoids ERROR_OBJECT_EXISTS.
    // A legacy WireGuard service may own the reserved address on a differently
    // named adapter, so release that cross-interface conflict before assigning it.
    remove_windows_address_conflicts(interface_name, address);
    reset_windows_adapter_address(interface_name);
    remove_windows_adapter_address(interface_name, address);

    let mut last_stdout = String::new();
    let mut last_stderr = String::new();
    for attempt in 0..3 {
        let mut command = Command::new("netsh.exe");
        configure_hidden_windows_command(&mut command);
        let output = command
            .args(["interface", "ipv4", "set", "address"])
            .arg(format!("name={interface_name}"))
            .arg("source=static")
            .arg(format!("address={address}"))
            .arg(format!("mask={}", prefix_to_netmask(prefix)))
            .arg("gateway=none")
            .arg("store=active")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("failed launching netsh to configure the Windows tunnel address")?;

        last_stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        last_stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if output.status.success() {
            return Ok(WindowsAddressGuard {
                interface_name: interface_name.to_string(),
                address,
            });
        }

        for _ in 0..10 {
            if windows_adapter_has_address(interface_name, address) {
                return Ok(WindowsAddressGuard {
                    interface_name: interface_name.to_string(),
                    address,
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        if attempt == 0 {
            reset_windows_adapter_address(interface_name);
            remove_windows_adapter_address(interface_name, address);
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    bail!(
        "failed configuring Windows tunnel address {address}/{prefix} on {interface_name}: {} {}",
        last_stdout,
        last_stderr
    );
}

#[cfg(target_os = "windows")]
fn remove_windows_route(interface_name: &str, route: &str) {
    let mut command = Command::new("netsh.exe");
    configure_hidden_windows_command(&mut command);
    let _ = command
        .args(["interface", "ipv4", "delete", "route"])
        .arg(format!("prefix={route}"))
        .arg(format!("interface={interface_name}"))
        .arg("store=active")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "windows")]
fn windows_adapter_has_route(interface_name: &str, route: &str) -> bool {
    let mut command = Command::new("netsh.exe");
    configure_hidden_windows_command(&mut command);
    command
        .args(["interface", "ipv4", "show", "route"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .is_some_and(|output| {
            let output = String::from_utf8_lossy(&output.stdout);
            output.contains(route) && output.contains(interface_name)
        })
}

#[cfg(target_os = "windows")]
fn install_windows_allowed_ip_routes(
    interface_name: &str,
    allowed_ips: &[IpNetwork],
) -> Result<WindowsRouteGuard> {
    let mut installed: Vec<String> = Vec::new();
    for network in allowed_ips {
        let IpNetwork::V4(network) = network else {
            continue;
        };
        let route = network.to_string();
        remove_windows_route(interface_name, &route);

        let mut command = Command::new("netsh.exe");
        configure_hidden_windows_command(&mut command);
        let output = command
            .args(["interface", "ipv4", "add", "route"])
            .arg(format!("prefix={route}"))
            .arg(format!("interface={interface_name}"))
            .arg("nexthop=0.0.0.0")
            .arg("store=active")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("failed launching netsh for route {route}"))?;

        if !output.status.success() && !windows_adapter_has_route(interface_name, &route) {
            for installed_route in installed.iter().rev() {
                remove_windows_route(interface_name, installed_route);
            }
            bail!(
                "failed adding Windows route {route} through {interface_name}: {} {}",
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        installed.push(route);
    }

    if installed.is_empty() {
        bail!("WireGuard config has no IPv4 AllowedIPs routes for the Windows adapter");
    }

    Ok(WindowsRouteGuard {
        interface_name: interface_name.to_string(),
        routes: installed,
    })
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

    if let Err(error) = run(args).await {
        eprintln!("noland-net-helper: {error:#}");
        process::exit(1);
    }
}

async fn run(args: Args) -> Result<()> {
    cleanup_stale_runtime_owners(&args.state_dir).await?;
    let owner_lock = acquire_owner_lock(&args.state_dir)?;
    cleanup_platform_network_state(&args)?;
    let stop_request_path = args.state_dir.join(STOP_REQUEST_FILE_NAME);
    let _ = fs::remove_file(&stop_request_path);
    let status_path = args.state_dir.join(STATUS_FILE_NAME);
    let config_fingerprint = config_fingerprint(&args.config_path)?;
    let mut initial_status = RuntimeStatus::starting(&args, config_fingerprint.clone());
    write_status(&status_path, &initial_status)?;

    let result = run_tunnel(&args, &status_path, &config_fingerprint).await;
    if let Err(error) = &result {
        initial_status.error = Some(format!("{error:#}"));
        initial_status.updated_at_unix = unix_timestamp();
        let _ = write_status(&status_path, &initial_status);
    }
    drop(owner_lock);
    result
}

async fn run_tunnel(args: &Args, status_path: &Path, config_fingerprint: &str) -> Result<()> {
    let tunnel_config = parse_tunnel_config(&args.config_path)?;
    #[cfg(target_os = "windows")]
    let effective_listen_port = resolve_effective_listen_port(tunnel_config.listen_port);
    #[cfg(not(target_os = "windows"))]
    let effective_listen_port = tunnel_config.listen_port;
    #[cfg(target_os = "windows")]
    if effective_listen_port != tunnel_config.listen_port {
        eprintln!(
            "noland-net-helper: configured ListenPort {} is unavailable (excluded port range or held by another process), using {} instead",
            tunnel_config.listen_port, effective_listen_port
        );
    }
    let mut tun_config = tun::Configuration::default();

    #[cfg(not(target_os = "macos"))]
    tun_config.tun_name(INTERFACE_NAME);

    tun_config.mtu(tunnel_config.mtu).up();

    #[cfg(not(target_os = "windows"))]
    tun_config
        .address(tunnel_config.client_address)
        .netmask(prefix_to_netmask(tunnel_config.client_prefix))
        .destination(tunnel_config.server_address);

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
        if effective_listen_port != 0 {
            ensure_windows_gotatun_firewall_rule(effective_listen_port)?;
        }
        tun_config.platform_config(|platform| {
            platform.wintun_file(wintun_path.as_os_str());
        });
    }

    let async_tun =
        tun::create_as_async(&tun_config).context("failed creating managed TUN adapter")?;
    let interface_name = async_tun
        .tun_name()
        .context("failed resolving managed TUN adapter name")?;

    #[cfg(target_os = "windows")]
    let _windows_address = configure_windows_adapter_address(
        &interface_name,
        tunnel_config.client_address,
        tunnel_config.client_prefix.min(24),
    )?;

    #[cfg(target_os = "windows")]
    let _windows_routes =
        install_windows_allowed_ip_routes(&interface_name, &tunnel_config.allowed_ips)?;

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
        .with_listen_port(effective_listen_port)
        .with_peer(peer)
        .build()
        .await
        .context("failed starting embedded GotaTun device")?;

    let mut status = RuntimeStatus {
        engine: "gotatun-embedded-0.7.1".to_string(),
        active: true,
        pid: process::id(),
        interface_name,
        config_path: args.config_path.display().to_string(),
        launch_id: args.launch_id.clone(),
        peer_public_key: BASE64_STANDARD.encode(tunnel_config.peer_public_key),
        allowed_ips: tunnel_config
            .allowed_ips
            .iter()
            .map(ToString::to_string)
            .collect(),
        endpoint: tunnel_config.endpoint.to_string(),
        config_fingerprint: config_fingerprint.to_string(),
        listen_port: effective_listen_port,
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
                if let Err(error) = write_status_with_retry(status_path, &status, 5) {
                    eprintln!(
                        "noland-net-helper: status heartbeat write failed; keeping the tunnel active: {error:#}"
                    );
                }
            }
        }
    }

    device.stop().await;
    status.active = false;
    status.updated_at_unix = unix_timestamp();
    if let Err(error) = write_status_with_retry(status_path, &status, 5) {
        eprintln!("noland-net-helper: failed writing final tunnel status: {error:#}");
    }
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
    let mut launch_id = None;
    let mut wintun_path = None;
    while let Some(arg) = values.next() {
        match arg.as_str() {
            "--config" => config_path = values.next().map(PathBuf::from),
            "--state-dir" => state_dir = values.next().map(PathBuf::from),
            "--launch-id" => launch_id = values.next(),
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
        launch_id: match command {
            HelperCommand::Run => launch_id.ok_or_else(|| anyhow!("missing --launch-id value"))?,
            HelperCommand::Check => launch_id.unwrap_or_else(|| "check".to_string()),
        },
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

fn runtime_root(state_dir: &Path) -> PathBuf {
    let runtime_parent = state_dir.parent().unwrap_or(state_dir);
    if runtime_parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.chars().all(|character| character.is_ascii_digit()))
    {
        runtime_parent
            .parent()
            .unwrap_or(runtime_parent)
            .to_path_buf()
    } else {
        runtime_parent.to_path_buf()
    }
}

fn owner_lock_path(state_dir: &Path) -> PathBuf {
    runtime_root(state_dir).join(OWNER_LOCK_FILE_NAME)
}

fn runtime_directories(state_dir: &Path) -> Vec<PathBuf> {
    let root = runtime_root(state_dir);
    let mut runtimes = vec![root.join("gotatun-runtime")];
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("gotatun-runtime");
            if candidate.is_dir() && !runtimes.contains(&candidate) {
                runtimes.push(candidate);
            }
        }
    }
    runtimes
}

fn load_runtime_status(path: &Path) -> Option<RuntimeStatus> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

#[cfg(target_os = "linux")]
fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let executable_matches = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_owned()))
        .and_then(|name| name.to_str().map(str::to_string))
        .is_some_and(|name| name.starts_with("noland-net-helper"));
    if !executable_matches {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()
        .is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("noland-net-helper")
        })
}

#[cfg(target_os = "windows")]
fn process_exists(pid: u32) -> bool {
    let mut command = Command::new("tasklist.exe");
    configure_hidden_windows_command(&mut command);
    command
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .is_some_and(|output| {
            let output = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            output.contains("noland-net-helper") && output.contains(&format!("\"{pid}\""))
        })
}

#[cfg(unix)]
fn terminate_stale_helper(pid: u32) {
    if pid == 0 || pid == process::id() {
        return;
    }
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(target_os = "linux")]
fn discover_helper_pids() -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| *pid != process::id() && process_exists(*pid))
        .collect()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn discover_helper_pids() -> Vec<u32> {
    Command::new("pgrep")
        .args(["-x", "noland-net-helper"])
        .output()
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.trim().parse::<u32>().ok())
                .filter(|pid| *pid != process::id())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn discover_helper_pids() -> Vec<u32> {
    let mut command = Command::new("tasklist.exe");
    configure_hidden_windows_command(&mut command);
    command
        .args([
            "/FI",
            "IMAGENAME eq noland-net-helper.exe",
            "/FO",
            "CSV",
            "/NH",
        ])
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let fields = line.split(',').collect::<Vec<_>>();
                    fields
                        .get(1)
                        .map(|value| value.trim().trim_matches('"'))
                        .and_then(|value| value.parse::<u32>().ok())
                })
                .filter(|pid| *pid != process::id())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn terminate_stale_helper(pid: u32) {
    if pid == 0 || pid == process::id() {
        return;
    }
    let mut command = Command::new("taskkill.exe");
    configure_hidden_windows_command(&mut command);
    let _ = command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

async fn cleanup_stale_runtime_owners(state_dir: &Path) -> Result<()> {
    let runtimes = runtime_directories(state_dir);
    let mut owner_pids = discover_helper_pids();
    for runtime in &runtimes {
        if let Some(status) = load_runtime_status(&runtime.join(STATUS_FILE_NAME)) {
            if status.pid != process::id() && !owner_pids.contains(&status.pid) {
                owner_pids.push(status.pid);
            }
        }
        if runtime.exists() {
            fs::write(runtime.join(STOP_REQUEST_FILE_NAME), b"stop\n").with_context(|| {
                format!(
                    "failed requesting stale helper shutdown in {}",
                    runtime.display()
                )
            })?;
        }
    }

    for _ in 0..20 {
        if owner_pids.iter().all(|pid| !process_exists(*pid)) {
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    for pid in owner_pids
        .iter()
        .copied()
        .filter(|pid| process_exists(*pid))
    {
        terminate_stale_helper(pid);
    }
    for _ in 0..20 {
        if owner_pids.iter().all(|pid| !process_exists(*pid)) {
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    if let Some(pid) = owner_pids.into_iter().find(|pid| process_exists(*pid)) {
        bail!(
            "stale Noland GotaTun helper PID {pid} could not be terminated during elevated cleanup"
        );
    }

    for runtime in runtimes {
        let _ = fs::remove_file(runtime.join(STOP_REQUEST_FILE_NAME));
        let _ = fs::remove_file(runtime.join(STATUS_FILE_NAME));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn cleanup_platform_network_state(_args: &Args) -> Result<()> {
    if let Ok(config) = parse_tunnel_config(&_args.config_path) {
        for network in &config.allowed_ips {
            if let IpNetwork::V4(network) = network {
                remove_windows_route(INTERFACE_NAME, &network.to_string());
            }
        }
        remove_windows_address_conflicts(INTERFACE_NAME, config.client_address);
        reset_windows_adapter_address(INTERFACE_NAME);
        remove_windows_adapter_address(INTERFACE_NAME, config.client_address);
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn cleanup_platform_network_state(_args: &Args) -> Result<()> {
    Ok(())
}

fn acquire_owner_lock(state_dir: &Path) -> Result<File> {
    let lock_path = owner_lock_path(state_dir);
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "failed opening tunnel ownership lock {}",
                lock_path.display()
            )
        })?;
    lock_file.try_lock_exclusive().with_context(|| {
        format!(
            "another Noland GotaTun helper already owns the managed tunnel ({})",
            lock_path.display()
        )
    })?;
    Ok(lock_file)
}

fn config_fingerprint(config_path: &Path) -> Result<String> {
    let content = fs::read(config_path).with_context(|| {
        format!(
            "failed reading {} for fingerprinting",
            config_path.display()
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(content)))
}

fn write_status_with_retry(path: &Path, status: &RuntimeStatus, attempts: usize) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..attempts.max(1) {
        match write_status(path, status) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < attempts {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("status write failed")))
}

fn write_status(path: &Path, status: &RuntimeStatus) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("status path has no parent: {}", path.display()))?;
    // The helper runs elevated on macOS, while this directory belongs to the desktop user.
    // Recreating a removed parent hierarchy here would make the app data directory root-owned
    // and prevent the desktop app from loading or saving its state on the next launch.
    #[cfg(not(target_os = "macos"))]
    fs::create_dir_all(parent)?;
    #[cfg(target_os = "macos")]
    if !parent.is_dir() {
        bail!(
            "status directory disappeared while the elevated helper was running: {}",
            parent.display()
        );
    }

    let serialized = serde_json::to_vec_pretty(status)?;

    #[cfg(target_os = "windows")]
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        file.write_all(&serialized)?;
        file.sync_data()?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        let temporary = parent.join(format!(".{STATUS_FILE_NAME}.tmp"));
        fs::write(&temporary, serialized)?;
        fs::rename(&temporary, path)?;
        Ok(())
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{config_fingerprint, owner_lock_path, parse_tunnel_config, prefix_to_netmask};
    #[cfg(target_os = "windows")]
    use super::{resolve_effective_listen_port, udp_port_probe_ok};
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
        let fingerprint = config_fingerprint(&path).unwrap();
        assert_eq!(config.client_address, Ipv4Addr::new(10, 77, 0, 2));
        assert_eq!(config.client_prefix, 32);
        assert_eq!(config.server_address, Ipv4Addr::new(10, 77, 0, 1));
        assert_eq!(config.endpoint.to_string(), "127.0.0.1:51820");
        assert_eq!(config.listen_port, 51821);
        assert_eq!(config.mtu, 1280);
        assert_eq!(config.keepalive, Some(25));
        assert_eq!(fingerprint.len(), 64);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_fingerprint_changes_with_endpoint_generation() {
        let root = std::env::temp_dir().join(format!(
            "noland-net-helper-fingerprint-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("nolandwg0.conf");
        fs::write(&path, "Endpoint = 192.0.2.1:51820\n").unwrap();
        let first = config_fingerprint(&path).unwrap();
        fs::write(&path, "Endpoint = 192.0.2.2:51820\n").unwrap();
        let second = config_fingerprint(&path).unwrap();
        assert_ne!(first, second);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_and_global_runtime_dirs_share_one_owner_lock() {
        let root = std::env::temp_dir().join(format!(
            "noland-net-helper-owner-test-{}",
            std::process::id()
        ));
        let global_runtime = root.join("gotatun-runtime");
        let legacy_runtime = root.join("47458589").join("gotatun-runtime");
        assert_eq!(owner_lock_path(&global_runtime), root.join("owner.lock"));
        assert_eq!(owner_lock_path(&legacy_runtime), root.join("owner.lock"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn listen_port_resolver_skips_ports_held_by_other_sockets() {
        // Hold a UDP port without SO_REUSEADDR, mirroring the exclusive holder
        // case that makes Windows fail GotaTun binds with WSAEACCES.
        let blocker = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let held_port = blocker.local_addr().unwrap().port();
        assert!(!udp_port_probe_ok(held_port));

        let resolved = resolve_effective_listen_port(held_port);
        assert_ne!(resolved, held_port);
        assert!(udp_port_probe_ok(resolved));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn listen_port_resolver_keeps_available_configured_port() {
        let resolved = resolve_effective_listen_port(51820);
        if udp_port_probe_ok(51820) {
            assert_eq!(resolved, 51820);
        } else {
            assert_ne!(resolved, 51820);
            assert!(udp_port_probe_ok(resolved));
        }
    }

    #[test]
    fn converts_cidr_prefix_to_ipv4_netmask() {
        assert_eq!(prefix_to_netmask(0), Ipv4Addr::new(0, 0, 0, 0));
        assert_eq!(prefix_to_netmask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(prefix_to_netmask(32), Ipv4Addr::new(255, 255, 255, 255));
    }
}
