use std::{env, process::Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Macos,
    Linux,
    Windows,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchKind {
    X64,
    Arm64,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub struct OsDetection {
    os: OsKind,
}

impl Default for OsDetection {
    fn default() -> Self {
        Self::new()
    }
}

impl OsDetection {
    pub fn new() -> Self {
        let os = match std::env::consts::OS {
            "macos" => OsKind::Macos,
            "linux" => OsKind::Linux,
            "windows" => OsKind::Windows,
            _ => OsKind::Unknown,
        };

        Self { os }
    }

    pub fn is_macos(&self) -> bool {
        self.os == OsKind::Macos
    }

    pub fn is_windows(&self) -> bool {
        self.os == OsKind::Windows
    }

    pub fn is_linux(&self) -> bool {
        self.os == OsKind::Linux
    }

    pub fn arch(&self) -> ArchKind {
        match env::consts::ARCH {
            "x86_64" => ArchKind::X64,
            "aarch64" => ArchKind::Arm64,
            _ => ArchKind::Unknown,
        }
    }

    pub fn platform_display_name(&self) -> &'static str {
        match self.os {
            OsKind::Macos => "Mac",
            OsKind::Linux => "Linux machine",
            OsKind::Windows => "Windows machine",
            OsKind::Unknown => "client machine",
        }
    }

    pub fn ssh_known_hosts_null_file(&self) -> &'static str {
        if self.is_windows() {
            "NUL"
        } else {
            "/dev/null"
        }
    }

    pub fn ping_args(&self, host: &str) -> Vec<String> {
        if self.is_windows() {
            vec!["-n".to_string(), "3".to_string(), host.to_string()]
        } else {
            vec![
                "-c".to_string(),
                "3".to_string(),
                "-W".to_string(),
                "2".to_string(),
                host.to_string(),
            ]
        }
    }

    pub fn ssh_add_args_for_key(&self, key_path: &str) -> Vec<String> {
        if self.is_macos() {
            vec!["--apple-use-keychain".to_string(), key_path.to_string()]
        } else {
            vec![key_path.to_string()]
        }
    }

    pub fn ssh_add_stdin_args(&self) -> Vec<String> {
        if self.is_macos() {
            vec!["--apple-use-keychain".to_string(), "--stdin".to_string()]
        } else {
            vec!["--stdin".to_string()]
        }
    }

    pub fn command_exists(&self, command: &str) -> bool {
        if self.is_windows() {
            return Command::new("where")
                .arg(command)
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
        }

        Command::new("sh")
            .arg("-lc")
            .arg(format!("command -v {command} >/dev/null 2>&1"))
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    pub fn resolve_command_path(&self, command: &str) -> Option<String> {
        if self.is_windows() {
            let output = Command::new("where").arg(command).output().ok()?;
            if !output.status.success() {
                return None;
            }
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let first = stdout
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())?;
            return Some(first.to_string());
        }

        let output = Command::new("sh")
            .arg("-lc")
            .arg(format!("command -v {command} 2>/dev/null"))
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let first = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if first.is_empty() {
            None
        } else {
            Some(first)
        }
    }

    pub fn default_path_prefixes(&self) -> &'static [&'static str] {
        if self.is_macos() {
            &[
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
            ]
        } else if self.is_linux() {
            &[
                "/usr/local/sbin",
                "/usr/local/bin",
                "/usr/sbin",
                "/usr/bin",
                "/sbin",
                "/bin",
            ]
        } else {
            &[]
        }
    }

    pub fn with_augmented_path(&self, command: &mut Command) {
        if self.is_windows() {
            return;
        }

        let current = env::var("PATH").unwrap_or_default();
        let mut segments = self.default_path_prefixes().join(":");
        if !current.trim().is_empty() {
            segments.push(':');
            segments.push_str(&current);
        }
        command.env("PATH", segments);
    }

    pub fn install_hint_for_tool(&self, tool: &str) -> String {
        if matches!(
            tool,
            "gotatun"
                | "wg"
                | "wg.exe"
                | "wg-quick"
                | "wg-quick.exe"
                | "wireguard.exe"
                | "wireguard"
        ) {
            return "This tool is expected to be bundled and managed by Noland Connect. Reinstall or rebuild the app so the managed sidecars are packaged correctly, or explicitly point the app at the binary with the matching `NOLAND_*_BIN` override.".to_string();
        }

        if self.is_macos() {
            return match tool {
                "ssh" | "ssh-keygen" | "ssh-add" => {
                    "Install Xcode Command Line Tools or OpenSSH client tools.".to_string()
                }
                _ => format!("Install `{tool}` and ensure it is available in PATH."),
            };
        }

        if self.is_linux() {
            return match tool {
                "xdg-open" => "Install xdg-utils (`sudo apt-get install -y xdg-utils`).".to_string(),
                "ssh" | "ssh-keygen" | "ssh-add" => {
                    "Install OpenSSH client tools (example: `sudo apt-get install -y openssh-client`)."
                        .to_string()
                }
                _ => format!("Install `{tool}` with your package manager and ensure it is in PATH."),
            };
        }

        if self.is_windows() {
            return match tool {
                "ssh" | "ssh-keygen" | "ssh-add" => {
                    "Install or enable OpenSSH Client in Windows optional features.".to_string()
                }
                _ => format!("Install `{tool}` and add it to your PATH."),
            };
        }

        format!("Install `{tool}` and ensure it is available in PATH.")
    }

    pub fn install_command_for_tool(&self, tool: &str) -> Option<&'static str> {
        if matches!(
            tool,
            "gotatun"
                | "wg"
                | "wg.exe"
                | "wg-quick"
                | "wg-quick.exe"
                | "wireguard.exe"
                | "wireguard"
        ) {
            return None;
        }

        if self.is_macos() {
            return match tool {
                "ssh" | "ssh-keygen" | "ssh-add" => Some("xcode-select --install"),
                _ => None,
            };
        }

        if self.is_linux() {
            return match tool {
                "xdg-open" => Some(
                    "if command -v apt-get >/dev/null 2>&1; then sudo DEBIAN_FRONTEND=noninteractive apt-get update && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y xdg-utils; elif command -v dnf >/dev/null 2>&1; then sudo dnf install -y xdg-utils; elif command -v yum >/dev/null 2>&1; then sudo yum install -y xdg-utils; elif command -v pacman >/dev/null 2>&1; then sudo pacman -Sy --noconfirm xdg-utils; elif command -v zypper >/dev/null 2>&1; then sudo zypper --non-interactive install xdg-utils; else exit 127; fi",
                ),
                "ssh" | "ssh-keygen" | "ssh-add" => Some(
                    "if command -v apt-get >/dev/null 2>&1; then sudo DEBIAN_FRONTEND=noninteractive apt-get update && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y openssh-client; elif command -v dnf >/dev/null 2>&1; then sudo dnf install -y openssh-clients; elif command -v yum >/dev/null 2>&1; then sudo yum install -y openssh-clients; elif command -v pacman >/dev/null 2>&1; then sudo pacman -Sy --noconfirm openssh; elif command -v zypper >/dev/null 2>&1; then sudo zypper --non-interactive install openssh; else exit 127; fi",
                ),
                _ => None,
            };
        }

        if self.is_windows() {
            return match tool {
                "ssh" | "ssh-keygen" | "ssh-add" => Some(
                    "powershell -NoProfile -ExecutionPolicy Bypass -Command \"Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0\"",
                ),
                _ => None,
            };
        }

        None
    }

    pub fn try_install_tool(&self, tool: &str) -> Result<bool, String> {
        let Some(install_cmd) = self.install_command_for_tool(tool) else {
            return Ok(false);
        };

        if self.command_exists(tool) {
            return Ok(true);
        }

        let output = if self.is_windows() {
            Command::new("cmd").args(["/C", install_cmd]).output()
        } else {
            let mut command = Command::new("sh");
            command.arg("-lc").arg(install_cmd);
            self.with_augmented_path(&mut command);
            command.output()
        }
        .map_err(|error| format!("Failed to run installer for `{tool}`: {error}"))?;

        if !output.status.success() {
            return Err(format!(
                "Auto-install failed for `{tool}` (exit {}): stdout: {} | stderr: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Ok(self.command_exists(tool))
    }
}
