use std::{fs, path::PathBuf, process::Command};

use anyhow::{bail, Context, Result};

pub trait LinkController: Send + Sync {
    fn get_mtu(&self, interface_name: &str) -> Result<u16>;
    fn set_mtu(&self, interface_name: &str, mtu: u16) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct SystemLinkController;

impl LinkController for SystemLinkController {
    fn get_mtu(&self, interface_name: &str) -> Result<u16> {
        validate_interface_name(interface_name)?;
        let path = PathBuf::from("/sys/class/net")
            .join(interface_name)
            .join("mtu");
        let value = fs::read_to_string(&path)
            .with_context(|| format!("failed reading MTU from {}", path.display()))?;
        value
            .trim()
            .parse::<u16>()
            .with_context(|| format!("invalid MTU reported by {}", path.display()))
    }

    fn set_mtu(&self, interface_name: &str, mtu: u16) -> Result<()> {
        validate_interface_name(interface_name)?;
        validate_mtu(mtu)?;
        // `ip` performs one bounded RTNETLINK mutation. Arguments are passed
        // directly (never through a shell), and the interface name is tightly
        // validated above.
        let output = Command::new("ip")
            .args(["link", "set", "dev", interface_name, "mtu"])
            .arg(mtu.to_string())
            .output()
            .context("failed launching ip for WireGuard MTU update")?;
        if !output.status.success() {
            bail!(
                "failed setting {interface_name} MTU to {mtu}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let observed = self.get_mtu(interface_name)?;
        if observed != mtu {
            bail!(
                "MTU readback mismatch for {interface_name}: requested {mtu}, observed {observed}"
            );
        }
        Ok(())
    }
}

pub fn validate_mtu(mtu: u16) -> Result<()> {
    if !(576..=1500).contains(&mtu) {
        bail!("WireGuard MTU must be between 576 and 1500 bytes");
    }
    Ok(())
}

fn validate_interface_name(interface_name: &str) -> Result<()> {
    if interface_name.is_empty()
        || interface_name.len() > 15
        || !interface_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("invalid network interface name");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shell_metacharacters_and_out_of_range_mtu() {
        assert!(validate_interface_name("wg0").is_ok());
        assert!(validate_interface_name("wg0;reboot").is_err());
        assert!(validate_interface_name("../../eth0").is_err());
        assert!(validate_mtu(1200).is_ok());
        assert!(validate_mtu(500).is_err());
    }
}
