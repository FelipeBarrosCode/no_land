use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use noland_state_core::{dedicated_cgroup_path, AppId};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct DedicatedCgroup {
    pub path: String,
    pub created: bool,
}

pub struct CgroupResolver {
    sysfs_root: PathBuf,
}

impl Default for CgroupResolver {
    fn default() -> Self {
        Self {
            sysfs_root: PathBuf::from("/sys/fs/cgroup"),
        }
    }
}

impl CgroupResolver {
    pub fn new(sysfs_root: PathBuf) -> Self {
        Self { sysfs_root }
    }

    pub fn preferred_path(app_id: &AppId, session_id: Uuid) -> String {
        dedicated_cgroup_path(app_id, session_id)
    }

    pub fn try_create(&self, app_id: &AppId, session_id: Uuid) -> DedicatedCgroup {
        let logical = Self::preferred_path(app_id, session_id);
        let fs_path = self.sysfs_root.join(logical.trim_start_matches('/'));
        let created = fs::create_dir_all(&fs_path).is_ok();
        DedicatedCgroup {
            path: logical,
            created,
        }
    }

    pub fn try_attach(&self, cgroup_path: &str, pid: i32) -> io::Result<()> {
        let procs = self
            .sysfs_root
            .join(cgroup_path.trim_start_matches('/'))
            .join("cgroup.procs");
        let mut file = fs::OpenOptions::new().append(true).open(procs)?;
        writeln!(file, "{pid}")?;
        Ok(())
    }

    pub fn read_proc_cgroup(pid: i32) -> Option<String> {
        let text = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
        // cgroup v2: "0::/path"
        for line in text.lines() {
            if let Some(rest) = line.split("::").nth(1) {
                return Some(rest.to_string());
            }
            if let Some((_, path)) = line.split_once(':') {
                if let Some((_, path)) = path.split_once(':') {
                    return Some(path.to_string());
                }
            }
        }
        None
    }
}
