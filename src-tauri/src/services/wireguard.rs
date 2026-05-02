use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use serde::Serialize;
use tokio::fs;
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use super::{app_config::WireGuardDefaults, remote_exec::RemoteExec};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireGuardProvisionResult {
    pub server_ip: String,
    pub client_ip: String,
    pub server_public_key: String,
    pub client_public_key: String,
    pub client_config_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct WireGuardService {
    pub defaults: WireGuardDefaults,
}

const REQUIRED_REMOTE_WIREGUARD_PACKAGES: &[&str] = &["wireguard-tools", "iproute2", "ufw"];
const APT_UPDATE_TIMEOUT_SECS: u64 = 180;
const APT_INSTALL_TIMEOUT_SECS: u64 = 300;

impl WireGuardService {
    async fn wait_for_dpkg_lock_with_message(
        &self,
        remote: &RemoteExec,
        max_wait_secs: u64,
    ) -> AppResult<bool> {
        // Option C: Surgical approach
        // 1. Quick check first
        // 2. If locked, aggressive kill
        // 3. Check again
        // 4. Only then wait with timeout
        let surgical_script = format!(
            r#"#!/bin/bash
set -uo pipefail

LOCK_FILES="/var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/cache/apt/archives/lock /var/lib/apt/lists/lock"
MAX_WAIT={max_wait_secs}

check_lock() {{
    for lock in $LOCK_FILES; do
        if sudo fuser "$lock" >/dev/null 2>&1; then
            return 1
        fi
    done
    return 0
}}

# Phase 1: Quick check (0-3 seconds)
if check_lock; then
    echo "LOCK_FREE"
    exit 0
fi

# Phase 2: Aggressive kill (unattended-upgrades often auto-restarts)
echo "LOCK_HELD: killing competing apt processes..."
sudo systemctl stop unattended-upgrades 2>/dev/null || true
sudo systemctl mask unattended-upgrades 2>/dev/null || true
sudo pkill -9 -f unattended-upgrades 2>/dev/null || true
sudo pkill -9 -f apt.systemd.daily 2>/dev/null || true
sudo pkill -9 -f "[a]pt-get" 2>/dev/null || true
sudo pkill -9 -f "[d]pkg" 2>/dev/null || true
sleep 2

# Phase 3: Fix broken dpkg state and remove stale locks
echo "Fixing dpkg state..."
sudo dpkg --configure -a 2>/dev/null || true
for lock in $LOCK_FILES; do
    if [ -f "$lock" ]; then
        sudo rm -f "$lock" 2>/dev/null || true
    fi
done

# Phase 4: Check again after cleanup
sleep 1
if check_lock; then
    echo "LOCK_FREE_AFTER_KILL"
    exit 0
fi

# Phase 5: Patient wait with timeout
echo "Still locked after cleanup, waiting up to ${{MAX_WAIT}}s..."
check_count=0
while ! check_lock; do
    check_count=$((check_count + 1))
    if [ $((check_count % 15)) -eq 0 ]; then
        echo "Still waiting for package manager lock... ${{check_count}}s elapsed"
    fi
    sleep 1
    if [ $check_count -ge $MAX_WAIT ]; then
        echo "TIMEOUT: Package manager lock not released after ${{MAX_WAIT}} seconds"
        # Unmask so future boots are not broken
        sudo systemctl unmask unattended-upgrades 2>/dev/null || true
        exit 1
    fi
done

# Unmask so future boots are not broken
sudo systemctl unmask unattended-upgrades 2>/dev/null || true
echo "LOCK_RELEASED_AFTER_WAIT ${{check_count}}"
exit 0"#
        );

        let remote = remote.clone();
        let result = tokio::task::spawn_blocking(move || {
            remote.ssh(&surgical_script, Duration::from_secs(max_wait_secs + 30))
        })
        .await
        .map_err(|error| AppError::Command(format!("join failure: {error}")))??;

        if result.status_code != 0 {
            info!(
                "dpkg lock wait returned {}: stdout={} stderr={}",
                result.status_code,
                result.stdout.trim(),
                result.stderr.trim()
            );
            return Ok(false);
        }

        let stdout = result.stdout.trim();
        if stdout.contains("LOCK_FREE") || stdout.contains("LOCK_RELEASED") {
            info!("dpkg lock acquired: {}", stdout);
            Ok(true)
        } else {
            info!("dpkg lock check returned unexpected output: {}", stdout);
            Ok(false)
        }
    }

    pub async fn configure(
        &self,
        remote: &RemoteExec,
        local_app_data_dir: &Path,
        endpoint_host: &str,
        endpoint_port: u16,
    ) -> AppResult<WireGuardProvisionResult> {
        ensure_local_wireguard_tools()?;

        let (server_private, server_public) = generate_keypair()?;
        let (client_private, client_public) = generate_keypair()?;

        self.cleanup_existing_wireguard(remote).await?;
        self.setup_cpu_governor(remote).await?;
        let primary_interface = self.detect_primary_interface(remote).await?;
        let server_config = self.render_server_config(&server_private, &client_public);
        let server_tunnel_host = strip_cidr(&self.defaults.server_tunnel_ip);
        let client_config = self.render_client_config(
            &client_private,
            &server_public,
            endpoint_host,
            endpoint_port,
            &format!("{server_tunnel_host}/32"),
        );

        self.setup_queue_management_persistent(remote).await?;
        self.setup_network_tuning_persistent(remote).await?;

        let packages_needed = self.check_wireguard_packages_needed(remote).await?;

        let escaped_server_config = shell_single_quote_escape(&server_config);

        // Write config file first (doesn't need dpkg lock)
        let config_script = format!(
            "sudo mkdir -p /etc/wireguard && sudo bash -lc 'cat > /etc/wireguard/{}.conf <<\"EOF\"\n{}\nEOF'",
            self.defaults.server_interface_name,
            escaped_server_config
        );

        let remote_write_config = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&config_script, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if remote_write_config.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "WireGuard config write failed: {}",
                remote_write_config.stderr
            )));
        }

        if !packages_needed.is_empty() {
            info!(
                "Missing WireGuard packages on remote, attempting install: {}",
                packages_needed.join(", ")
            );
            self.install_wireguard_packages(remote, &packages_needed)
                .await?;
        }

        // Set up firewall rules only after ufw is guaranteed to be installed
        self.setup_firewall_rules(remote, &primary_interface)
            .await?;

        let bring_up = {
            let remote = remote.clone();
            let iface = self.defaults.server_interface_name.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &format!(
                        "sudo systemctl enable wg-quick@{iface} && (sudo wg-quick down {iface} 2>/dev/null || true) && sudo wg-quick up {iface} && ip a show {iface} && sudo wg show"
                    ),
                    Duration::from_secs(120),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if bring_up.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "WireGuard interface did not start: {}",
                bring_up.stderr
            )));
        }

        self.setup_wireguard_routing(remote, &primary_interface)
            .await?;

        self.apply_network_tuning(remote, &primary_interface)
            .await?;
        self.validate_network_tuning(remote, &primary_interface)
            .await?;

        let local_config_dir = local_app_data_dir.join("wireguard");
        fs::create_dir_all(&local_config_dir).await?;
        let local_config_path = local_config_dir.join("noland-connect-client.conf");
        fs::write(&local_config_path, client_config).await?;

        Ok(WireGuardProvisionResult {
            server_ip: strip_cidr(&self.defaults.server_tunnel_ip),
            client_ip: strip_cidr(&self.defaults.client_tunnel_ip),
            server_public_key: server_public,
            client_public_key: client_public,
            client_config_path: local_config_path,
        })
    }

    async fn check_wireguard_packages_needed(&self, remote: &RemoteExec) -> AppResult<Vec<String>> {
        let query = REQUIRED_REMOTE_WIREGUARD_PACKAGES.join(" ");
        let check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &format!("dpkg-query -W -f='${{Package}}\\n' {}", query),
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if check.status_code != 0 {
            info!(
                "Package check returned {}, assuming all packages need installation",
                check.status_code
            );
            return Ok(REQUIRED_REMOTE_WIREGUARD_PACKAGES
                .iter()
                .map(|s| s.to_string())
                .collect());
        }

        let installed: std::collections::HashSet<_> = check
            .stdout
            .lines()
            .map(|l| l.trim().to_lowercase())
            .collect();

        let missing: Vec<String> = REQUIRED_REMOTE_WIREGUARD_PACKAGES
            .iter()
            .filter(|p| !installed.contains(&p.to_lowercase()))
            .map(|s| s.to_string())
            .collect();

        if missing.is_empty() {
            info!("All WireGuard packages already installed, skipping apt-get");
        } else {
            info!("Missing WireGuard packages: {}", missing.join(", "));
        }

        Ok(missing)
    }

    async fn install_wireguard_packages(
        &self,
        remote: &RemoteExec,
        packages_needed: &[String],
    ) -> AppResult<()> {
        // Permanently neuter unattended-upgrades so it can't re-acquire the lock
        let disable_auto_upgrades = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "sudo systemctl stop unattended-upgrades 2>/dev/null || true; sudo systemctl disable --now unattended-upgrades 2>/dev/null || true; sudo systemctl mask unattended-upgrades 2>/dev/null || true; sudo apt-get remove -y unattended-upgrades 2>/dev/null || true; sudo rm -f /etc/apt/apt.conf.d/20auto-upgrades /etc/apt/apt.conf.d/50unattended-upgrades 2>/dev/null || true; echo 'AUTO_UPGRADES_DISABLED'",
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        if disable_auto_upgrades.status_code != 0 {
            warn!(
                "Failed to disable auto-upgrades (continuing): stdout: {} | stderr: {}",
                disable_auto_upgrades.stdout.trim(),
                disable_auto_upgrades.stderr.trim()
            );
        } else {
            info!(
                "Auto-upgrades disabled: {}",
                disable_auto_upgrades.stdout.trim()
            );
        }

        // wait_for_dpkg_lock_with_message handles all cleanup internally (Option C)
        let lock_acquired = self.wait_for_dpkg_lock_with_message(remote, 120).await?;
        if !lock_acquired {
            return Err(AppError::Provisioning(
                "Package manager is locked by another process (likely unattended-upgrades). \
                Waiting timed out after 60 seconds. Please try again in a few minutes when \
                system updates have finished. Alternatively, you can SSH into the instance and \
                run: sudo systemctl stop unattended-upgrades && sudo dpkg --configure -a"
                    .to_string(),
            ));
        }

        let update = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "sudo DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=60 update",
                    Duration::from_secs(APT_UPDATE_TIMEOUT_SECS),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if update.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed apt-get update for WireGuard dependencies (exit {}): stdout: {} | stderr: {}",
                update.status_code,
                update.stdout.trim(),
                update.stderr.trim()
            )));
        }

        let install_command = format!(
            "sudo DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=60 -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 install -y {}",
            packages_needed.join(" ")
        );

        let install = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &install_command,
                    Duration::from_secs(APT_INSTALL_TIMEOUT_SECS),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if install.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed installing WireGuard dependencies (exit {}): stdout: {} | stderr: {}",
                install.status_code,
                install.stdout.trim(),
                install.stderr.trim()
            )));
        }

        let remaining = self.check_wireguard_packages_needed(remote).await?;
        if !remaining.is_empty() {
            return Err(AppError::Provisioning(format!(
                "WireGuard package install completed but required packages are still missing: {}",
                remaining.join(", ")
            )));
        }

        Ok(())
    }

    async fn setup_cpu_governor(&self, remote: &RemoteExec) -> AppResult<()> {
        let cpu_governor_service = r#"[Unit]
Description=Set CPU governor to performance
After=multi-user.target
ConditionPathExistsGlob=/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor

[Service]
Type=oneshot
ExecStart=/bin/bash -lc "for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > \"$cpu\" 2>/dev/null || true; done"

[Install]
WantedBy=multi-user.target
"#;

        let escaped = shell_single_quote_escape(cpu_governor_service);
        let command = format!(
            "sudo bash -lc 'cat > /etc/systemd/system/set-cpu-governor.service <<\"EOF\"\n{}\nEOF'",
            escaped
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to create CPU governor service: {}",
                output.stderr
            )));
        }

        let enable = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("sudo systemctl daemon-reload && sudo systemctl enable set-cpu-governor && sudo systemctl start set-cpu-governor", Duration::from_secs(30))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if enable.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to enable CPU governor: {}",
                enable.stderr
            )));
        }

        Ok(())
    }

    async fn setup_firewall_rules(
        &self,
        remote: &RemoteExec,
        primary_interface: &str,
    ) -> AppResult<()> {
        let firewall_setup = format!(
            r#"#!/bin/bash
set -euo pipefail

# Enable UFW if needed but avoid destructive reset of existing policy
ufw --force enable >/dev/null 2>&1 || true

# Allow SSH
ufw status | grep -q "22/tcp" || ufw allow 22/tcp comment 'SSH'

# Allow WireGuard
ufw status | grep -q "{}/udp" || ufw allow {}/udp comment 'WireGuard'

# Ensure WireGuard response traffic can always exit
ufw status | grep -q "{}/udp (out)" || ufw allow out {}/udp comment 'WireGuard outbound'

# Allow forwarding between public NIC and WireGuard interface
ufw route allow in on {} out on {} comment 'WG ingress forward' >/dev/null 2>&1 || true
ufw route allow in on {} out on {} comment 'WG egress forward' >/dev/null 2>&1 || true

# Allow ICMP (ping) via iptables directly — UFW ICMP syntax is inconsistent across versions
iptables -C INPUT -p icmp --icmp-type echo-request -j ACCEPT 2>/dev/null || iptables -A INPUT -p icmp --icmp-type echo-request -j ACCEPT
iptables -C INPUT -p icmp --icmp-type echo-reply -j ACCEPT 2>/dev/null || iptables -A INPUT -p icmp --icmp-type echo-reply -j ACCEPT
iptables -C OUTPUT -p icmp --icmp-type echo-request -j ACCEPT 2>/dev/null || iptables -A OUTPUT -p icmp --icmp-type echo-request -j ACCEPT
iptables -C OUTPUT -p icmp --icmp-type echo-reply -j ACCEPT 2>/dev/null || iptables -A OUTPUT -p icmp --icmp-type echo-reply -j ACCEPT

# Sunshine ports via WireGuard only
ufw status | grep -q "on {}" || ufw allow in on {} to any port 47984,47989,48010 comment 'Sunshine streaming'
"#,
            self.defaults.listen_port,
            self.defaults.listen_port,
            self.defaults.listen_port,
            self.defaults.listen_port,
            self.defaults.server_interface_name,
            primary_interface,
            primary_interface,
            self.defaults.server_interface_name,
            self.defaults.server_interface_name,
            self.defaults.server_interface_name
        );

        let escaped = shell_single_quote_escape(&firewall_setup);
        let command = format!(
            "sudo bash -lc 'cat > /tmp/setup-firewall.sh <<\"EOF\"\n{}\nEOF\nchmod +x /tmp/setup-firewall.sh\n/tmp/setup-firewall.sh'",
            escaped
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(90)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup firewall: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn setup_network_tuning_persistent(&self, remote: &RemoteExec) -> AppResult<()> {
        let wg_iface = self.defaults.server_interface_name.clone();
        let sysctl_config = format!(
            "# Network tuning for low-latency streaming
net.core.rmem_max=134217728
net.core.wmem_max=134217728
net.ipv4.tcp_rmem=4096 87380 134217728
net.ipv4.tcp_wmem=4096 65536 134217728
net.core.netdev_max_backlog=5000
net.ipv4.tcp_fastopen=3
net.ipv4.tcp_timestamps=0
net.ipv4.tcp_sack=1
net.ipv4.ip_forward=1
net.ipv4.conf.all.rp_filter=0
net.ipv4.conf.default.rp_filter=0
net.ipv4.conf.{wg_iface}.rp_filter=0
"
        );

        let escaped = shell_single_quote_escape(&sysctl_config);
        let command = format!(
            "sudo bash -lc 'cat > /etc/sysctl.d/99-noland-network.conf <<\"EOF\"\n{}\nEOF\nsudo sysctl --system >/dev/null'",
            escaped
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup network tuning: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    fn render_server_config(&self, server_private: &str, client_public: &str) -> String {
        format!(
            "[Interface]\nAddress = {}\nListenPort = {}\nPrivateKey = {}\nMTU = {}\n\n[Peer]\nPublicKey = {}\nAllowedIPs = {}\n",
            self.defaults.server_tunnel_ip,
            self.defaults.listen_port,
            server_private,
            self.defaults.tunnel_mtu,
            client_public,
            self.defaults.client_tunnel_ip,
        )
    }

    async fn setup_wireguard_routing(
        &self,
        remote: &RemoteExec,
        primary_interface: &str,
    ) -> AppResult<()> {
        let iface = self.defaults.server_interface_name.clone();
        let nic = primary_interface.to_string();
        // Note: FORWARD rules are handled by ufw route allow in setup_firewall_rules.
        // Only NAT/MASQUERADE remains here because UFW cannot configure it.
        let command = format!(
            "sudo sysctl -w net.ipv4.ip_forward=1 >/dev/null && sudo iptables -t nat -C POSTROUTING -o {nic} -j MASQUERADE 2>/dev/null || sudo iptables -t nat -A POSTROUTING -o {nic} -j MASQUERADE"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(90)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup WireGuard routing/NAT: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    fn render_client_config(
        &self,
        client_private: &str,
        server_public: &str,
        endpoint_host: &str,
        listen_port: u16,
        allowed_ips: &str,
    ) -> String {
        format!(
            "[Interface]\nAddress = {}\nPrivateKey = {}\nMTU = {}\n\n[Peer]\nPublicKey = {}\nEndpoint = {}:{}\nAllowedIPs = {}\nPersistentKeepalive = {}\n",
            self.defaults.client_tunnel_ip,
            client_private,
            self.defaults.tunnel_mtu,
            server_public,
            endpoint_host,
            listen_port,
            allowed_ips,
            self.defaults.persistent_keepalive_secs,
        )
    }

    async fn cleanup_existing_wireguard(&self, remote: &RemoteExec) -> AppResult<()> {
        let iface = self.defaults.server_interface_name.clone();
        let command = format!(
            "sudo bash -lc 'target=\"{iface}\"; for dir in /sys/class/net/wg*; do [ -e \"$dir\" ] || continue; dev=$(basename \"$dir\"); if [ \"$dev\" != \"$target\" ]; then systemctl stop \"wg-quick@$dev\" >/dev/null 2>&1 || true; systemctl disable \"wg-quick@$dev\" >/dev/null 2>&1 || true; wg-quick down \"$dev\" >/dev/null 2>&1 || true; ip link delete \"$dev\" >/dev/null 2>&1 || true; fi; done'"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed clearing existing WireGuard interfaces: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn detect_primary_interface(&self, remote: &RemoteExec) -> AppResult<String> {
        let route_get = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("ip route get 1.1.1.1", Duration::from_secs(20))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if route_get.status_code == 0 {
            if let Some(iface) = route_get.stdout.lines().find_map(parse_default_route_dev) {
                if !iface.is_empty() {
                    return Ok(iface);
                }
            }
        }

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("ip -o route show default", Duration::from_secs(40))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to detect primary egress interface: {}",
                output.stderr.trim()
            )));
        }

        let iface = output
            .stdout
            .lines()
            .find_map(parse_default_route_dev)
            .ok_or_else(|| {
                AppError::Provisioning(
                    "Could not detect primary egress interface from default route".to_string(),
                )
            })?;

        Ok(iface)
    }

    async fn setup_queue_management_persistent(&self, remote: &RemoteExec) -> AppResult<()> {
        let script = r#"#!/usr/bin/env bash
set -uo pipefail

EGRESS_IF="$(ip route get 1.1.1.1 | awk '{for (i=1;i<=NF;i++) if ($i=="dev") {print $(i+1); exit}}')"
[ -n "$EGRESS_IF" ] || { echo "No egress interface detected"; exit 1; }

tc qdisc replace dev "$EGRESS_IF" root fq_codel || true
tc -s qdisc show dev "$EGRESS_IF"
"#;

        let service = r#"[Unit]
Description=Apply FQ_Codel to default-route interface
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/noland-apply-qdisc.sh
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
"#;

        let rollback = r#"#!/usr/bin/env bash
set -euo pipefail

EGRESS_IF="$(ip route get 1.1.1.1 | awk '{for (i=1;i<=NF;i++) if ($i=="dev") {print $(i+1); exit}}')"
[ -n "$EGRESS_IF" ] || { echo "No egress interface detected"; exit 1; }

tc qdisc del dev "$EGRESS_IF" root 2>/dev/null || true
tc qdisc show dev "$EGRESS_IF"
"#;

        let command = format!(
            "sudo bash -lc 'cat > /usr/local/bin/noland-apply-qdisc.sh <<\"EOF\"\n{}\nEOF\nchmod +x /usr/local/bin/noland-apply-qdisc.sh\ncat > /usr/local/bin/noland-rollback-qdisc.sh <<\"EOF\"\n{}\nEOF\nchmod +x /usr/local/bin/noland-rollback-qdisc.sh\ncat > /etc/systemd/system/noland-qdisc.service <<\"EOF\"\n{}\nEOF\nsystemctl daemon-reload\nsystemctl enable --now noland-qdisc.service\n/usr/local/bin/noland-apply-qdisc.sh'",
            shell_single_quote_escape(script),
            shell_single_quote_escape(rollback),
            shell_single_quote_escape(service)
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(120)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup persistent queue management: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn apply_network_tuning(
        &self,
        remote: &RemoteExec,
        primary_interface: &str,
    ) -> AppResult<()> {
        let iface = self.defaults.server_interface_name.clone();
        let nic = primary_interface.to_string();

        let command = format!(
            "sudo tc qdisc replace dev {nic} root fq_codel && (sudo ethtool -C {nic} rx-usecs 0 tx-usecs 0 || true) && sudo sysctl -w net.ipv4.ip_forward=1 >/dev/null && sudo sysctl -w net.ipv4.conf.all.rp_filter=0 >/dev/null && sudo sysctl -w net.ipv4.conf.default.rp_filter=0 >/dev/null && sudo sysctl -w net.ipv4.conf.{nic}.rp_filter=0 >/dev/null && sudo sysctl -w net.ipv4.conf.{iface}.rp_filter=0 >/dev/null && (sudo systemctl stop tailscaled 2>/dev/null || true)"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(120)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed applying network pacing/tuning: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn validate_network_tuning(
        &self,
        remote: &RemoteExec,
        primary_interface: &str,
    ) -> AppResult<()> {
        let iface = self.defaults.server_interface_name.clone();
        let nic = primary_interface.to_string();
        let command = format!(
            "ip a show {iface} && ip route && tc -s qdisc show dev {iface} && tc -s qdisc show dev {nic} && if pgrep -x sunshine >/dev/null; then taskset -p $(pgrep -x sunshine | head -n 1); fi"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(120)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Network tuning verification failed: {}",
                output.stderr
            )));
        }

        Ok(())
    }
}

pub fn setup_local_wireguard_client(config_path: &Path) -> AppResult<String> {
    if !config_path.exists() {
        return Err(AppError::NotFound(format!(
            "WireGuard client config not found at {}",
            config_path.display()
        )));
    }

    ensure_local_wireguard_tools()?;

    normalize_wireguard_client_allowed_ips(config_path)?;

    #[cfg(target_os = "macos")]
    {
        setup_local_wireguard_client_macos(config_path)
    }

    #[cfg(target_os = "linux")]
    {
        setup_local_wireguard_client_linux(config_path)
    }

    #[cfg(target_os = "windows")]
    {
        setup_local_wireguard_client_windows(config_path)
    }
}

#[cfg(target_os = "macos")]
fn ensure_local_wireguard_tools() -> AppResult<()> {
    if command_exists("wg") && command_exists("wg-quick") {
        return Ok(());
    }

    if !command_exists("brew") {
        return Err(AppError::Command(
            "WireGuard tools are missing (wg/wg-quick). Install Homebrew and run `brew install wireguard-tools`, then retry."
                .to_string(),
        ));
    }

    let install = Command::new("bash")
        .arg("-lc")
        .arg("brew install wireguard-tools")
        .output()
        .map_err(|error| {
            AppError::Command(format!(
                "Failed to run Homebrew install for wireguard-tools: {error}"
            ))
        })?;

    if !install.status.success() {
        return Err(AppError::Command(format!(
            "Failed to auto-install wireguard-tools with Homebrew (exit {}): stdout: {} | stderr: {}",
            install.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&install.stdout).trim(),
            String::from_utf8_lossy(&install.stderr).trim()
        )));
    }

    if !command_exists("wg") || !command_exists("wg-quick") {
        return Err(AppError::Command(
            "wireguard-tools installation completed, but wg/wg-quick are still unavailable in PATH. Open a new terminal session and retry."
                .to_string(),
        ));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_local_wireguard_tools() -> AppResult<()> {
    if command_exists("wg") && command_exists("wg-quick") {
        return Ok(());
    }

    let install = Command::new("sudo")
        .args(["apt-get", "install", "-y", "wireguard", "wireguard-tools"])
        .output()
        .map_err(|error| {
            AppError::Command(format!(
                "Failed to run apt install for WireGuard tools: {error}"
            ))
        })?;

    if !install.status.success() {
        return Err(AppError::Command(format!(
            "Failed to auto-install WireGuard tools with apt (exit {}): stdout: {} | stderr: {}",
            install.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&install.stdout).trim(),
            String::from_utf8_lossy(&install.stderr).trim()
        )));
    }

    if !command_exists("wg") || !command_exists("wg-quick") {
        return Err(AppError::Command(
            "WireGuard packages installed, but wg/wg-quick are still unavailable in PATH. Open a new terminal session and retry."
                .to_string(),
        ));
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn ensure_local_wireguard_tools() -> AppResult<()> {
    let wg_status = Command::new("where").arg("wg").status();
    let wireguard_exe_status = Command::new("where").arg("wireguard.exe").status();

    let has_wg = wg_status.map(|status| status.success()).unwrap_or(false);
    let has_wireguard_exe = wireguard_exe_status
        .map(|status| status.success())
        .unwrap_or(false);

    if has_wg && has_wireguard_exe {
        return Ok(());
    }

    Err(AppError::Command(
        "WireGuard tools are not installed on Windows. Please install WireGuard from https://wireguard.com/install and retry."
            .to_string(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn command_exists(command: &str) -> bool {
    Command::new("bash")
        .arg("-lc")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn normalize_wireguard_client_allowed_ips(config_path: &Path) -> AppResult<()> {
    const SCOPED_ALLOWED_IPS: &str = "10.77.0.1/32";

    let original = std::fs::read_to_string(config_path).map_err(|error| {
        AppError::Command(format!(
            "Failed reading WireGuard client config {}: {error}",
            config_path.display()
        ))
    })?;

    let mut in_peer_section = false;
    let mut in_interface_section = false;
    let mut replaced = false;
    let mut normalized_lines = Vec::with_capacity(original.lines().count() + 2);

    for line in original.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_peer_section = trimmed.eq_ignore_ascii_case("[Peer]");
            in_interface_section = trimmed.eq_ignore_ascii_case("[Interface]");
        }

        if in_interface_section && trimmed.to_ascii_lowercase().starts_with("dns") {
            continue;
        }

        if in_peer_section && trimmed.to_ascii_lowercase().starts_with("allowedips") {
            normalized_lines.push(format!("AllowedIPs = {SCOPED_ALLOWED_IPS}"));
            replaced = true;
        } else {
            normalized_lines.push(line.to_string());
        }
    }

    if !replaced {
        return Err(AppError::InvalidInput(format!(
            "WireGuard client config {} is missing AllowedIPs in [Peer] section",
            config_path.display()
        )));
    }

    let mut normalized = normalized_lines.join("\n");
    if original.ends_with('\n') {
        normalized.push('\n');
    }

    if normalized != original {
        std::fs::write(config_path, normalized).map_err(|error| {
            AppError::Command(format!(
                "Failed writing normalized WireGuard client config {}: {error}",
                config_path.display()
            ))
        })?;
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn setup_local_wireguard_client_macos(config_path: &Path) -> AppResult<String> {
    const LOCAL_TUNNEL_NAME: &str = "nolandwg0";
    const LOCAL_CONF_PATH: &str = "/usr/local/etc/wireguard/nolandwg0.conf";
    const HOMEBREW_CONF_PATH: &str = "/opt/homebrew/etc/wireguard/nolandwg0.conf";
    const LEGACY_TUNNEL_NAME: &str = "noland-connect-client";

    let path = config_path.display().to_string().replace('"', "\\\"");
    let shell_script = format!(
        "set -euo pipefail; cd /; export PATH=\"/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH\"; if ! command -v wg-quick >/dev/null 2>&1; then echo 'wg-quick not found. Install wireguard-tools first.' >&2; exit 1; fi; mkdir -p /usr/local/etc/wireguard /opt/homebrew/etc/wireguard; wg-quick down {LOCAL_TUNNEL_NAME} >/dev/null 2>&1 || true; wg-quick down {LEGACY_TUNNEL_NAME} >/dev/null 2>&1 || true; for iface in $(wg show interfaces 2>/dev/null || true); do [ \"$iface\" = \"{LOCAL_TUNNEL_NAME}\" ] || ifconfig \"$iface\" down >/dev/null 2>&1 || true; done; install -m 600 \"{path}\" {LOCAL_CONF_PATH}; install -m 600 \"{path}\" {HOMEBREW_CONF_PATH}; wg-quick up {LOCAL_CONF_PATH}; for _ in 1 2 3 4 5; do wg show > /tmp/noland-wg-show.txt 2>/dev/null || true; if grep -qi 'latest handshake:' /tmp/noland-wg-show.txt && ! grep -qi 'latest handshake: never' /tmp/noland-wg-show.txt; then break; fi; sleep 1; done; if grep -qi 'allowed ips: 0.0.0.0/0' /tmp/noland-wg-show.txt; then echo 'WireGuard came up in full-tunnel mode (0.0.0.0/0), refusing configuration' >&2; cat /tmp/noland-wg-show.txt >&2; exit 1; fi; if ! grep -qi 'allowed ips: 10.77.0.1/32' /tmp/noland-wg-show.txt; then echo 'WireGuard came up but allowed ips are not scoped to 10.77.0.1/32' >&2; cat /tmp/noland-wg-show.txt >&2; exit 1; fi"
    );
    let applescript = format!(
        "do shell script \"{}\" with administrator privileges",
        shell_script.replace('\\', "\\\\").replace('"', "\\\"")
    );

    let output = Command::new("osascript")
        .current_dir("/")
        .arg("-e")
        .arg(applescript)
        .output()
        .map_err(|error| AppError::Command(format!("Failed to run osascript: {error}")))?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "Failed to setup local WireGuard client (exit {}): stdout: {} | stderr: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok("WireGuard client tunnel configured and activated on this Mac".to_string())
}

#[cfg(target_os = "linux")]
fn setup_local_wireguard_client_linux(config_path: &Path) -> AppResult<String> {
    const LOCAL_TUNNEL_NAME: &str = "nolandwg0";
    const LEGACY_TUNNEL_NAME: &str = "noland-connect-client";

    let destination = "/etc/wireguard/nolandwg0.conf";
    let copy = Command::new("sudo")
        .args([
            "install",
            "-m",
            "600",
            config_path.to_string_lossy().as_ref(),
            destination,
        ])
        .output()
        .map_err(|error| AppError::Command(format!("Failed to copy WireGuard config: {error}")))?;

    if !copy.status.success() {
        return Err(AppError::Command(
            "Failed to copy WireGuard config with sudo. Approve sudo prompt and retry.".to_string(),
        ));
    }

    let _ = Command::new("sudo")
        .args(["wg-quick", "down", LOCAL_TUNNEL_NAME])
        .status();
    let _ = Command::new("sudo")
        .args(["wg-quick", "down", LEGACY_TUNNEL_NAME])
        .status();

    let up = Command::new("sudo")
        .args(["wg-quick", "up", destination])
        .output()
        .map_err(|error| AppError::Command(format!("Failed to start local WireGuard: {error}")))?;

    if !up.status.success() {
        return Err(AppError::Command(
            "Failed to start local WireGuard with sudo. Approve sudo prompt and retry.".to_string(),
        ));
    }

    Ok("WireGuard client tunnel configured and activated on this Linux machine".to_string())
}

#[cfg(target_os = "windows")]
fn setup_local_wireguard_client_windows(config_path: &Path) -> AppResult<String> {
    const LOCAL_TUNNEL_NAME: &str = "nolandwg0";
    const LEGACY_TUNNEL_NAME: &str = "noland-connect-client";
    let config = config_path.display().to_string();
    let _ = Command::new("wireguard.exe")
        .args(["/uninstalltunnelservice", LOCAL_TUNNEL_NAME])
        .status();
    let _ = Command::new("wireguard.exe")
        .args(["/uninstalltunnelservice", LEGACY_TUNNEL_NAME])
        .status();

    let output = Command::new("wireguard.exe")
        .args(["/installtunnelservice", &config])
        .output()
        .map_err(|error| AppError::Command(format!("Failed to run wireguard.exe: {error}")))?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "Failed to setup local WireGuard client: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok("WireGuard client tunnel installed as Windows service".to_string())
}

fn parse_default_route_dev(line: &str) -> Option<String> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    let dev_index = parts.iter().position(|part| *part == "dev")?;
    let iface = parts.get(dev_index + 1)?;
    Some((*iface).to_string())
}

fn generate_keypair() -> AppResult<(String, String)> {
    let private_result = RemoteExec::run_local("wg", &["genkey"], Duration::from_secs(15))?;
    if private_result.status_code != 0 {
        return Err(AppError::Command(format!(
            "wg genkey failed: {}",
            private_result.stderr
        )));
    }
    let private = private_result.stdout;

    let mut child = Command::new("wg")
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::Command(format!("Failed to spawn wg pubkey: {error}")))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(private.as_bytes()).map_err(|error| {
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

    let public = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if public.is_empty() {
        return Err(AppError::Command(
            "wg pubkey returned an empty key".to_string(),
        ));
    }

    Ok((private.trim().to_string(), public))
}

fn strip_cidr(ip: &str) -> String {
    ip.split('/').next().unwrap_or(ip).to_string()
}

fn shell_single_quote_escape(content: &str) -> String {
    content.replace('\'', "'\"'\"'")
}
