//! Local helper lifecycle primitives shared by the desktop app and privileged helper.
//! These paths must not be independently reconstructed by consumers.
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const STATUS_FILE: &str = "status.json";
pub const STOP_FILE: &str = "stop.request";
pub const LAUNCH_FILE: &str = "launch.json";
pub const STATUS_MAX_AGE_SECS: u64 = 5;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub engine: String,
    pub active: bool,
    pub pid: u32,
    pub process_start_ticks: u64,
    pub boot_id: String,
    pub process_start_id: String,
    pub interface_name: String,
    pub config_path: String,
    pub launch_id: String,
    pub peer_public_key: String,
    pub allowed_ips: Vec<String>,
    pub endpoint: String,
    pub control_endpoint: String,
    pub config_fingerprint: String,
    pub listen_port: u16,
    pub mtu: u16,
    pub latest_handshake_age_secs: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub updated_at_unix: u64,
    pub error: Option<String>,
}

impl RuntimeStatus {
    pub fn reports_healthy(&self, now: u64) -> bool {
        self.active && self.error.is_none() && status_is_fresh(self.updated_at_unix, now)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessIdentity {
    pub pid: u32,
    #[serde(default)]
    pub process_start_ticks: u64,
    #[serde(default)]
    pub boot_id: String,
    #[serde(default)]
    pub process_start_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerRecord {
    pub identity: ProcessIdentity,
    pub launch_id: String,
    pub state_dir: String,
    #[serde(default)]
    pub config_path: String,
}

/// Legacy records are discovery hints, not new lock locations. Check their
/// state_dir before trusting them: old Unix versions shared /tmp/owner.lock.
pub fn owner_record_paths(state_dir: &Path) -> [PathBuf; 2] {
    let parent = state_dir.parent().unwrap_or(state_dir);
    let root = if parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()))
    {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    };
    [state_dir.join("owner.lock"), root.join("owner.lock")]
}

pub fn config_root(config: &Path) -> &Path {
    let parent = config.parent().unwrap_or_else(|| Path::new("."));
    if parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            !name.is_empty() && name.chars().all(|character| character.is_ascii_digit())
        })
    {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    }
}

pub fn runtime_dir(root: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        // Keep the existing short socket address, but scope *all* locks inside it.
        let digest = Sha256::digest(root.as_os_str().to_string_lossy().as_bytes());
        PathBuf::from("/tmp").join(format!("noland-g-{}", hex::encode(&digest[..8])))
    }
    #[cfg(not(unix))]
    root.join("gotatun-runtime")
}

pub fn is_short_runtime(path: &Path) -> bool {
    cfg!(unix)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("noland-g-"))
}

pub fn runtime_directories(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![runtime_dir(root)];
    let legacy_global = root.join("gotatun-runtime");
    if !paths.contains(&legacy_global) {
        paths.push(legacy_global);
    }
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten().take(4096) {
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()))
            {
                let path = entry.path().join("gotatun-runtime");
                if path.is_dir() {
                    paths.push(path);
                }
            }
        }
    }
    paths
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchAuthorization {
    pub launch_id: String,
    pub config_path: String,
    pub expires_at_unix: u64,
}

impl LaunchAuthorization {
    pub fn load(state_dir: &Path) -> io::Result<Self> {
        serde_json::from_slice(&fs::read(state_dir.join(LAUNCH_FILE))?).map_err(io::Error::other)
    }

    pub fn authorizes(&self, launch_id: &str, config: &Path) -> bool {
        self.launch_id == launch_id && self.config_path == config.to_string_lossy()
    }

    pub fn authorizes_start(&self, launch_id: &str, config: &Path, now: u64) -> bool {
        self.authorizes(launch_id, config) && now < self.expires_at_unix
    }
}

pub fn status_is_fresh(updated_at: u64, now: u64) -> bool {
    // Do not consider a snapshot from the future healthy after a wall-clock adjustment.
    updated_at <= now.saturating_add(STATUS_MAX_AGE_SECS)
        && now.saturating_sub(updated_at) <= STATUS_MAX_AGE_SECS
}

/// Persist a live mutation only against the exact configuration the helper owns.
/// The returned digest becomes the helper's next control precondition.
pub fn update_config(
    path: &Path,
    expected: &str,
    section: &str,
    key: &str,
    value: &str,
) -> io::Result<String> {
    let original = fs::read(path)?;
    if format!("{:x}", Sha256::digest(&original)) != expected {
        return Err(io::Error::other(
            "configuration changed outside the running helper; reconnect before updating it",
        ));
    }
    let content = std::str::from_utf8(&original).map_err(io::Error::other)?;
    let mut current_section = "";
    let mut replaced = false;
    let mut output = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = &trimmed[1..trimmed.len() - 1];
        }
        if current_section == section
            && trimmed
                .split_once('=')
                .is_some_and(|(candidate, _)| candidate.trim().eq_ignore_ascii_case(key))
        {
            output.push_str(&format!("{key} = {value}\n"));
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !replaced {
        return Err(io::Error::other(format!(
            "configuration has no {section} {key} field"
        )));
    }
    atomic_write(path, output.as_bytes())?;
    Ok(format!("{:x}", Sha256::digest(output.as_bytes())))
}

/// Same-directory atomic publication, including Windows where rename does not
/// replace an existing destination. Readers see the old or new complete snapshot.
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    let temporary = parent.join(format!(".noland-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        use std::io::Write;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Configs contain a private key; snapshots and permits need to remain
            // readable by the desktop user even when published by the helper.
            options.mode(if path.extension().is_some_and(|ext| ext == "conf") {
                0o600
            } else {
                0o644
            });
        }
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        if let Ok(existing) = fs::metadata(path) {
            use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
            let created = file.metadata()?;
            if created.uid() != existing.uid() || created.gid() != existing.gid() {
                extern "C" {
                    fn fchown(fd: i32, owner: u32, group: u32) -> i32;
                }
                if unsafe { fchown(file.as_raw_fd(), existing.uid(), existing.gid()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            file.set_permissions(existing.permissions())?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            #[link(name = "kernel32")]
            extern "system" {
                fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
            }
            let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0x1 | 0x8) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(not(windows))]
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Extension used for OS tools. A stalled utility must not indefinitely block
/// helper heartbeats, shutdown, or the desktop's lifecycle lock.
pub trait BoundedCommand {
    fn bounded_output(&mut self, timeout: Duration) -> io::Result<Output>;
}

impl BoundedCommand for Command {
    fn bounded_output(&mut self, timeout: Duration) -> io::Result<Output> {
        use std::io::Read;
        let deadline = Instant::now() + timeout;
        let mut child = self.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let read_pipe = |pipe: Box<dyn Read + Send>| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let mut reader = pipe;
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            let keep = count.min((64 * 1024usize).saturating_sub(bytes.len()));
                            bytes.extend_from_slice(&chunk[..keep]);
                        }
                    }
                }
                let _ = tx.send(bytes);
            });
            rx
        };
        let stdout = read_pipe(Box::new(child.stdout.take().expect("piped stdout")));
        let stderr = read_pipe(Box::new(child.stderr.take().expect("piped stderr")));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(result.err().unwrap_or_else(|| {
                        io::Error::new(io::ErrorKind::TimedOut, "platform command timed out")
                    }));
                }
            }
        };
        let receive = |receiver: std::sync::mpsc::Receiver<Vec<u8>>| {
            receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "platform command output timed out")
                })
        };
        Ok(Output {
            status,
            stdout: receive(stdout)?,
            stderr: receive(stderr)?,
        })
    }
}

/// Identity survives PID reuse. macOS includes executable identity in the
/// bounded ps snapshot; Windows uses the kernel process creation timestamp.
#[cfg(target_os = "macos")]
pub fn process_start_id(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let output = Command::new("/bin/ps")
        .env("LC_ALL", "C")
        .args([
            "-ww",
            "-p",
            &pid.to_string(),
            "-o",
            "lstart=",
            "-o",
            "comm=",
        ])
        .bounded_output(Duration::from_secs(2))
        .ok()?;
    let identity = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && identity.contains("noland-net-helper")).then_some(identity)
}

#[cfg(windows)]
pub fn process_start_id(pid: u32) -> Option<String> {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetProcessTimes(
            handle: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn GetExitCodeProcess(handle: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let handle = unsafe { OpenProcess(0x1000, 0, pid) }; // PROCESS_QUERY_LIMITED_INFORMATION
    if handle.is_null() {
        return None;
    }
    let mut creation = FileTime::default();
    let mut exit = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    let mut code = 0;
    let ok = unsafe {
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) != 0
            && GetExitCodeProcess(handle, &mut code) != 0
            && code == 259
    };
    unsafe {
        CloseHandle(handle);
    }
    ok.then(|| format!("{:08x}{:08x}", creation.high, creation.low))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_configs_share_runtime_but_unrelated_roots_do_not() {
        let a = config_root(Path::new("app/wireguard/123/nolandwg0.conf"));
        let b = config_root(Path::new("app/wireguard/456/nolandwg0.conf"));
        assert_eq!(runtime_dir(a), runtime_dir(b));
        assert_ne!(runtime_dir(a), runtime_dir(Path::new("other/wireguard")));
        assert_ne!(
            runtime_dir(a).join("owner.lock"),
            runtime_dir(Path::new("other/wireguard")).join("owner.lock")
        );
    }

    #[test]
    fn launch_is_bound_to_generation_config_and_deadline() {
        let permit = LaunchAuthorization {
            launch_id: "one".into(),
            config_path: "a.conf".into(),
            expires_at_unix: 100,
        };
        assert!(permit.authorizes_start("one", Path::new("a.conf"), 99));
        assert!(!permit.authorizes_start("one", Path::new("a.conf"), 100));
        assert!(!permit.authorizes_start("two", Path::new("a.conf"), 99));
        assert!(!permit.authorizes_start("one", Path::new("b.conf"), 99));
    }

    #[test]
    fn freshness_rejects_stale_and_far_future_snapshots() {
        assert!(status_is_fresh(100, 105));
        assert!(!status_is_fresh(100, 106));
        assert!(!status_is_fresh(120, 100));
    }

    #[test]
    fn atomic_publication_replaces_existing_file() {
        let dir =
            std::env::temp_dir().join(format!("noland-runtime-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATUS_FILE);
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn live_updates_advance_fingerprint_and_reject_stale_writers() {
        let dir = std::env::temp_dir().join(format!("noland-config-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nolandwg0.conf");
        let initial =
            b"[Interface]\nMTU = 1280\nPrivateKey = secret\n\n[Peer]\nEndpoint = 192.0.2.1:51820\n";
        atomic_write(&path, initial).unwrap();
        let original = format!("{:x}", Sha256::digest(initial));
        let endpoint_revision =
            update_config(&path, &original, "Peer", "Endpoint", "192.0.2.2:51820").unwrap();
        assert_ne!(original, endpoint_revision);
        assert_eq!(
            format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
            endpoint_revision
        );
        assert!(update_config(&path, &original, "Interface", "MTU", "1200").is_err());
        let mtu_revision =
            update_config(&path, &endpoint_revision, "Interface", "MTU", "1200").unwrap();
        assert_ne!(endpoint_revision, mtu_revision);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Endpoint = 192.0.2.2:51820"));
        assert!(content.contains("MTU = 1200"));
        assert!(content.contains("PrivateKey = secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn discovery_includes_current_global_and_legacy_instance_runtime() {
        let root =
            std::env::temp_dir().join(format!("noland-discovery-test-{}", uuid::Uuid::new_v4()));
        let instance = root.join("123").join("gotatun-runtime");
        fs::create_dir_all(&instance).unwrap();
        let discovered = runtime_directories(&root);
        assert!(discovered.contains(&runtime_dir(&root)));
        assert!(discovered.contains(&root.join("gotatun-runtime")));
        assert!(discovered.contains(&instance));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stalled_platform_command_has_a_deadline() {
        let started = Instant::now();
        let error = Command::new("sleep")
            .arg("10")
            .bounded_output(Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
