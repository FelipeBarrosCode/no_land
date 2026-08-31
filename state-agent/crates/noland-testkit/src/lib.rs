//! Fixtures for attribution, classification, commit, and restore tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use noland_state_core::*;
use uuid::Uuid;

pub struct Harness {
    pub root: PathBuf,
    pub home: PathBuf,
}

impl Harness {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!("noland-harness-{}", Uuid::new_v4()));
        let home = root.join("home");
        fs::create_dir_all(home.join(".local/share/applications")).unwrap();
        fs::create_dir_all(home.join(".config")).unwrap();
        fs::create_dir_all(home.join(".local/share")).unwrap();
        Self { root, home }
    }

    pub fn write_desktop(&self, id: &str, name: &str, exec: &Path) {
        let path = self
            .home
            .join(".local/share/applications")
            .join(format!("{id}.desktop"));
        fs::write(
            path,
            format!(
                "[Desktop Entry]\nName={name}\nExec={}\nType=Application\n",
                exec.display()
            ),
        )
        .unwrap();
    }

    pub fn write_minecraft_like(&self) -> PathBuf {
        let data = self.home.join(".local/share/example-game");
        fs::create_dir_all(data.join("saves/world")).unwrap();
        fs::create_dir_all(data.join("mods")).unwrap();
        fs::create_dir_all(self.home.join(".config/example-game")).unwrap();
        fs::write(data.join("saves/world/level.dat"), b"world-v1").unwrap();
        fs::write(data.join("mods/cool.jar"), b"mod-bytes").unwrap();
        fs::write(
            self.home.join(".config/example-game/options.txt"),
            b"render=fancy",
        )
        .unwrap();
        data
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Launch a helper that mutates application state. Returns the child pid.
pub fn launch_mutator(save: &Path, payload: &[u8]) -> i32 {
    fs::create_dir_all(save.parent().unwrap()).unwrap();
    #[cfg(unix)]
    {
        let script = save.parent().unwrap().join("mutate.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s' '{data}' > '{path}'\n",
                data = String::from_utf8_lossy(payload).replace('\'', ""),
                path = save.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o755));
        let child = Command::new(&script).spawn().expect("spawn mutator");
        let pid = child.id() as i32;
        let _ = child.wait_with_output();
        pid
    }
    #[cfg(not(unix))]
    {
        fs::write(save, payload).unwrap();
        1
    }
}

pub fn agent_paths(root: &Path) -> AgentPaths {
    AgentPaths::from_roots(root.join("state"), root.join("run"))
}
