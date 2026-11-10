use crate::store::{private_dir, private_file, Store};
use anyhow::{Context, Result};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use noland_runtime_contracts::SessionDescriptor as Descriptor;

pub struct Session {
    pub descriptor: Descriptor,
    pub dir: PathBuf,
    pub authority: PathBuf,
}

pub fn clean_command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LANG", "C.UTF-8");
    c
}

impl Session {
    pub fn discover(store: &Store) -> Result<Self> {
        anyhow::ensure!(
            cfg!(target_os = "linux"),
            "Runtime execution requires a Linux parent"
        );
        #[cfg(unix)]
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        #[cfg(not(unix))]
        let (uid, gid) = (0, 0);
        anyhow::ensure!(
            uid != 0,
            "Run the companion as the desktop user, never root"
        );
        let home = PathBuf::from(std::env::var("HOME")?);
        anyhow::ensure!(home.is_absolute(), "HOME must be absolute");
        let user_output = clean_command("id").arg("-un").output()?;
        anyhow::ensure!(
            user_output.status.success(),
            "Cannot determine desktop identity"
        );
        let user = String::from_utf8(user_output.stdout)?.trim().to_string();
        let base = PathBuf::from(format!("/run/user/{uid}"));
        // A private user-state fallback is useful in container images without pam_systemd.
        let base = if base.is_dir() {
            base.join("noland-runtime")
        } else {
            store.state.join("sessions")
        };
        private_dir(&base)?;
        let dir = base.join(uuid::Uuid::new_v4().to_string());
        private_dir(&dir)?;
        let pulse_socket = [
            PathBuf::from(format!("/run/user/{uid}/pulse/native")),
            home.join(".config/pulse/native"),
        ]
        .into_iter()
        .find(|p| p.exists());
        let driver = clean_command("timeout")
            .args([
                "5",
                "nvidia-smi",
                "--query-gpu=driver_version",
                "--format=csv,noheader",
            ])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let descriptor = Descriptor {
            display: crate::DISPLAY.into(),
            authority_source: crate::XAUTHORITY.into(),
            user,
            uid,
            gid,
            home,
            pulse_socket,
            driver_identity: driver,
        };
        let authority = dir.join("Xauthority");
        Ok(Self {
            descriptor,
            dir,
            authority,
        })
    }
    pub fn refresh_authority(&self) -> Result<()> {
        let output = clean_command("sudo")
            .args(["-n", "/usr/local/libexec/noland-runtime-session"])
            .stderr(Stdio::null())
            .output()
            .context("No Land session credential helper is unavailable")?;
        anyhow::ensure!(
            output.status.success() && !output.stdout.is_empty() && output.stdout.len() < 65536,
            "Cannot refresh display credentials; repair the companion installation"
        );
        let mut file = private_file(&self.authority)?;
        file.set_len(0)?;
        file.write_all(&output.stdout)?;
        // Cookie bytes stay in this process/private file, never structured output.
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
