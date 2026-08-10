use std::{env, path::PathBuf, process::Command};

use crate::utils::managed_binaries::locate_bundled_binary;

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

    pub fn managed_binary_target_triple(&self) -> &'static str {
        match (env::consts::OS, env::consts::ARCH) {
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
            ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
            ("windows", "x86_64") => "x86_64-pc-windows-msvc",
            ("windows", "aarch64") => "aarch64-pc-windows-msvc",
            _ => "",
        }
    }

    pub fn locate_app_managed_binary(
        &self,
        stem: &str,
        env_var: &str,
        uses_exe_suffix: bool,
    ) -> Option<PathBuf> {
        locate_bundled_binary(
            stem,
            env_var,
            uses_exe_suffix,
            self.managed_binary_target_triple(),
        )
    }

    pub fn install_hint_for_tool(&self, tool: &str) -> String {
        if matches!(tool, "noland-net-helper" | "ssh" | "scp" | "ssh-keygen") {
            return "This tool is expected to be bundled and managed by Noland Connect. Reinstall or rebuild the app so the managed sidecars are packaged correctly, or explicitly point the app at the binary with the matching `NOLAND_*_BIN` override.".to_string();
        }

        if self.is_linux() && tool == "xdg-open" {
            return "This Linux desktop session does not expose a default-browser opener. Copy the URL into a browser or install a desktop portal implementation."
                .to_string();
        }

        if self.is_macos() || self.is_windows() {
            return format!("The operating system does not expose its `{tool}` integration.");
        }

        if self.is_linux() {
            return format!("The Linux desktop session does not expose `{tool}`.");
        }

        format!("Install `{tool}` and ensure it is available in PATH.")
    }
}
