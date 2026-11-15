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
    // Do not inherit launcher/provider environment hints that force software
    // graphics, restrict GPU visibility or clamp application worker counts.
    // CPU scheduling/affinity and device permissions remain parent-owned.
    c.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LANG", "C.UTF-8");
    c
}

fn create_workspace(store: &Store) -> Result<PathBuf> {
    // Driver copies and probe homes can be large. /run/user/<uid> is a
    // systemd-managed tmpfs with its own quota, even when RAM and the main disk
    // have plenty of room. Share the runtime state filesystem instead; Noland
    // imposes no separate workspace size limit and does not remount system tmpfs.
    let base = store.state.join("sessions");
    private_dir(&base)?;
    let dir = base.join(uuid::Uuid::new_v4().to_string());
    private_dir(&dir)?;
    Ok(dir)
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
        let dir = create_workspace(store)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_do_not_add_cpu_gpu_or_thread_caps_to_the_clean_environment() {
        let command = clean_command("bwrap");
        let environment = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(environment.len(), 2);
        assert_eq!(
            environment["PATH"],
            "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        );
        assert_eq!(environment["LANG"], "C.UTF-8");
        assert_eq!(command.get_program(), "bwrap");
        assert_eq!(command.get_args().count(), 0);
        #[cfg(unix)]
        {
            let output = clean_command("/usr/bin/env").output().unwrap();
            assert!(output.status.success());
            let environment = String::from_utf8(output.stdout).unwrap();
            let lines = environment.lines().collect::<Vec<_>>();
            // Do not print inherited environment values if this ever regresses.
            assert!(lines.len() == 2);
            assert!(lines
                .iter()
                .all(|line| line.starts_with("PATH=") || line.starts_with("LANG=")));
        }
    }

    #[test]
    fn workspaces_share_state_storage_without_using_user_runtime_tmpfs() {
        let root = tempfile::tempdir().unwrap();
        let store = Store {
            data: root.path().join("data"),
            state: root.path().join("state"),
        };
        let first = create_workspace(&store).unwrap();
        let second = create_workspace(&store).unwrap();
        assert_eq!(first.parent(), Some(store.state.join("sessions").as_path()));
        assert_eq!(second.parent(), first.parent());
        assert_ne!(first, second);
        assert!(first.is_dir() && second.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!(
                fs::metadata(&first).unwrap().dev(),
                fs::metadata(&store.state).unwrap().dev()
            );
            for path in [&first, &second, &store.state.join("sessions")] {
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
        // Creating a later workspace never cleans or changes another session.
        fs::write(first.join("active-session"), b"keep").unwrap();
        create_workspace(&store).unwrap();
        assert_eq!(fs::read(first.join("active-session")).unwrap(), b"keep");
    }
}
