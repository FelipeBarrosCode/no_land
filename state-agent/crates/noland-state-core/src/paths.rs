use std::path::{Path, PathBuf};

use crate::constants;

#[derive(Debug, Clone)]
pub struct AgentPaths {
    pub state_root: PathBuf,
    pub run_root: PathBuf,
    pub db_path: PathBuf,
    pub staging: PathBuf,
    pub snapshots: PathBuf,
    pub packs: PathBuf,
    pub restore: PathBuf,
    pub checkpoints: PathBuf,
    pub cache: PathBuf,
    pub rpc_socket: PathBuf,
}

impl AgentPaths {
    pub fn production() -> Self {
        Self::from_roots(
            PathBuf::from(constants::STATE_ROOT),
            PathBuf::from(constants::RUN_ROOT),
        )
    }

    pub fn from_roots(state_root: PathBuf, run_root: PathBuf) -> Self {
        Self {
            db_path: state_root.join("state.db"),
            staging: state_root.join("staging"),
            snapshots: state_root.join("snapshots"),
            packs: state_root.join("packs"),
            restore: state_root.join("restore"),
            checkpoints: state_root.join("checkpoints"),
            cache: state_root.join("cache"),
            rpc_socket: run_root.join("state-agent.sock"),
            state_root,
            run_root,
        }
    }

    pub fn ephemeral_storage(&self, operation_id: &str) -> PathBuf {
        self.run_root.join("storage").join(operation_id)
    }

    pub fn restore_dir(&self, restore_id: &str) -> PathBuf {
        self.restore.join(restore_id)
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            &self.state_root,
            &self.run_root,
            &self.staging,
            &self.snapshots,
            &self.packs,
            &self.restore,
            &self.checkpoints,
            &self.cache,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn is_internal(&self, path: &Path) -> bool {
        is_noland_internal(path)
            || path.starts_with(&self.state_root)
            || path.starts_with(&self.run_root)
    }
}

pub fn is_noland_internal(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with("/var/lib/noland")
        || text.starts_with("/run/noland")
        || text.contains("/noland/state/")
        || text.contains("/.noland/")
}

pub fn is_hard_volatile_root(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with("/tmp")
        || text.starts_with("/var/tmp")
        || text.starts_with("/run")
        || text.starts_with("/proc")
        || text.starts_with("/sys")
        || text.starts_with("/dev")
        || text.starts_with("/var/run")
}

pub fn looks_like_lock_or_socket(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some("lock" | "pid" | "sock" | "socket") => true,
        _ => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            name.ends_with(".lock")
                || name.ends_with(".pid")
                || name.starts_with('.') && name.ends_with(".swp")
        }
    }
}

pub fn looks_like_cache(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("/.cache/")
        || text.contains("/cache/")
        || text.contains("/shadercache/")
        || text.contains("/gpu-cache/")
        || text.contains("/code cache/")
        || text.contains("/tmp/")
        || text.ends_with(".tmp")
        || text.ends_with(".log")
}

pub fn looks_like_secret(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("/.ssh/")
        || text.contains("login.keychain")
        || text.contains("cookies")
        || text.contains("secret")
        || text.ends_with(".pem")
        || text.ends_with(".key")
        || text.contains("refresh_token")
        || text.contains("credentials")
}

pub fn looks_like_user_state(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("/saves/")
        || text.contains("/save/")
        || text.contains("/worlds/")
        || text.contains("/saves")
        || text.contains("/mods/")
        || text.contains("/config/")
        || text.contains("/.config/")
        || text.contains("/.local/share/")
        || text.ends_with(".sav")
        || text.ends_with(".save")
        || text.contains("userdata")
}

pub fn looks_like_os_or_lib(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with("/usr/")
        || text.starts_with("/lib")
        || text.starts_with("/bin")
        || text.starts_with("/sbin")
        || text.starts_with("/opt/nvidia")
        || text.starts_with("/etc/")
}
