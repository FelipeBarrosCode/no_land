//! Background desktop plumbing must not silently throttle foreground transfers.
use std::path::Path;

use noland_state_core::{AppIdentity, AppSession, Result, SessionSource};

use crate::StateAgent;

const SUPPORT_ROOTS: &[&str] = &["/opt/selkies", "/opt/selkies-gstreamer"];
const SUPPORT_EXECUTABLES: &[&str] = &[
    "selkies",
    "selkies-gstreamer",
    "sunshine",
    "noland-state-agent",
    "noland-lifecycle-agent",
    "noland-network-agent",
    "noland-mic-receiver",
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
    "pulseaudio",
    "guacd",
];
const SUPPORT_SERVICES: &[&str] = &[
    "selkies.service",
    "selkies-gstreamer.service",
    "sunshine.service",
    "noland-state-agent.service",
    "noland-lifecycle-agent.service",
    "noland-network-agent.service",
    "noland-mic-receiver.service",
    "pipewire.service",
    "pipewire-pulse.service",
    "wireplumber.service",
    "pulseaudio.service",
];

fn latency_sensitive_session(
    session: &AppSession,
    app: Option<&AppIdentity>,
    live_executable: Option<&Path>,
    cgroup: &str,
) -> bool {
    // Explicit application launches/bindings remain protected even if they use
    // an interpreter. Never exempt python, /opt, or system.slice as a whole.
    if !matches!(
        session.source,
        SessionSource::ExecutableDiscovery | SessionSource::Learned
    ) {
        return true;
    }
    let executable =
        live_executable.or_else(|| app.and_then(|app| app.canonical_executable.as_deref()));
    let support_executable = executable.is_some_and(|path| {
        SUPPORT_ROOTS.iter().any(|root| path.starts_with(root))
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SUPPORT_EXECUTABLES.contains(&name))
    });
    // The general noland-desktop service also contains user games/browsers;
    // only narrowly identified support services may bypass this safeguard.
    let support_service = cgroup
        .lines()
        .any(|line| line.split('/').any(|part| SUPPORT_SERVICES.contains(&part)));
    !(support_executable || support_service)
}

fn steam_launcher_session(
    session: &AppSession,
    app: Option<&AppIdentity>,
    executable: Option<&Path>,
    steam_root: Option<&Path>,
) -> bool {
    // A Steam game session can initially point at the launcher. Keep it
    // protected; exemption requires a live, app-independent launcher identity.
    if session.app_id.as_str().starts_with("steam:")
        || app.is_some_and(|app| app.steam_app_id.is_some())
        || !matches!(
            session.source,
            SessionSource::ExecutableDiscovery | SessionSource::Learned
        )
    {
        return false;
    }
    executable
        .zip(steam_root)
        .and_then(|(exe, root)| exe.strip_prefix(root).ok())
        .and_then(Path::to_str)
        .is_some_and(|relative| {
            matches!(
                relative,
                "ubuntu12_32/steam"
                    | "linux32/steam"
                    | "ubuntu12_64/steamwebhelper"
                    | "linux64/steamwebhelper"
            )
        })
}

impl StateAgent {
    pub(crate) fn has_latency_sensitive_sessions(&self) -> Result<bool> {
        let steam_root = self
            .roots
            .lock()
            .steam_root
            .clone()
            .and_then(|root| std::fs::canonicalize(root).ok());
        for session in self.db.open_sessions()? {
            let app = self.db.get_app(&session.app_id)?;
            let process = std::path::PathBuf::from(format!("/proc/{}", session.root_pid));
            let executable = std::fs::read_link(process.join("exe")).ok();
            if steam_launcher_session(
                &session,
                app.as_ref(),
                executable.as_deref(),
                steam_root.as_deref(),
            ) {
                continue;
            }
            let cgroup = std::fs::read_to_string(process.join("cgroup"))
                .unwrap_or_else(|_| session.cgroup_path.clone());
            if latency_sensitive_session(&session, app.as_ref(), executable.as_deref(), &cgroup) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_state_core::AppId;

    #[test]
    fn only_live_registered_steam_launcher_processes_are_exempt() {
        let root = Path::new("/home/user/.local/share/Steam");
        let executable = root.join("ubuntu12_32/steam");
        let (mut session, app) = discovered(executable.to_str().unwrap());
        assert!(steam_launcher_session(
            &session,
            Some(&app),
            Some(&executable),
            Some(root)
        ));
        assert!(!steam_launcher_session(
            &session,
            Some(&app),
            None,
            Some(root)
        ));
        assert!(!steam_launcher_session(
            &session,
            Some(&app),
            Some(Path::new("/home/user/game/steam")),
            Some(root)
        ));
        session.app_id = AppId::steam(553850);
        assert!(!steam_launcher_session(
            &session,
            Some(&app),
            Some(&executable),
            Some(root)
        ));
    }

    fn discovered(executable: &str) -> (AppSession, AppIdentity) {
        let id = AppId("exe:fixture".into());
        let session = AppSession::new(id.clone(), 0, SessionSource::ExecutableDiscovery);
        let mut app = AppIdentity::new(id, "fixture");
        app.canonical_executable = Some(executable.into());
        (session, app)
    }

    #[test]
    fn selkies_python_is_not_gameplay_but_user_interpreters_are() {
        let (session, app) = discovered("/opt/selkies-gstreamer/bin/python3.12");
        assert!(!latency_sensitive_session(&session, Some(&app), None, ""));
        for executable in [
            "/usr/bin/python3",
            "/home/user/game/.venv/bin/python3",
            "/opt/selkies-game/game",
        ] {
            let (session, app) = discovered(executable);
            assert!(latency_sensitive_session(&session, Some(&app), None, ""));
        }
    }

    #[test]
    fn dedicated_support_cgroups_do_not_exempt_the_entire_desktop() {
        let (session, app) = discovered("/usr/bin/python3");
        assert!(!latency_sensitive_session(
            &session,
            Some(&app),
            None,
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/selkies.service"
        ));
        for group in [
            "0::/system.slice/noland-desktop.service",
            "0::/system.slice/my-game.service",
            "",
        ] {
            assert!(latency_sensitive_session(&session, Some(&app), None, group));
        }
    }

    #[test]
    fn explicit_and_unknown_application_sessions_remain_protected() {
        let (mut session, app) = discovered("/opt/selkies-gstreamer/bin/python3.12");
        session.source = SessionSource::NolandLaunch;
        assert!(latency_sensitive_session(
            &session,
            Some(&app),
            None,
            "selkies.service"
        ));
        session.source = SessionSource::ExecutableDiscovery;
        assert!(latency_sensitive_session(&session, None, None, ""));
        assert!(latency_sensitive_session(
            &session,
            Some(&app),
            Some(Path::new("/home/user/Games/Hookshot/hookshot")),
            ""
        ));
    }
}
