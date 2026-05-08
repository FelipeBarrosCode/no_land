#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Macos,
    Linux,
    Windows,
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
}
