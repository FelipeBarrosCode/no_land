//! Background desktop plumbing must not silently throttle foreground transfers.
use std::io;
use std::path::{Component, Path, PathBuf};

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

enum ProcessPresence {
    Absent,
    Unknown,
    Present { executable: PathBuf, cgroup: String },
}

struct WorkloadProbe<'a> {
    proc_root: &'a Path,
    cgroup_root: &'a Path,
}

impl WorkloadProbe<'_> {
    fn process(
        &self,
        pid: i32,
        read_file: &impl Fn(&Path) -> io::Result<String>,
    ) -> ProcessPresence {
        if pid <= 0 {
            return ProcessPresence::Unknown;
        }
        let process = self.proc_root.join(pid.to_string());
        let stat = match read_file(&process.join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A missing stat inside a still-present process directory is
                // not evidence of death. Nor is an unavailable /proc (e.g. on
                // non-Linux hosts) or an unreadable process directory.
                return match std::fs::metadata(&process) {
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            && std::fs::read_dir(self.proc_root).is_ok() =>
                    {
                        ProcessPresence::Absent
                    }
                    _ => ProcessPresence::Unknown,
                };
            }
            Err(_) => return ProcessPresence::Unknown,
        };
        let Some(generation) = process_generation(&stat, pid) else {
            return ProcessPresence::Unknown;
        };
        let Ok(executable) = std::fs::read_link(process.join("exe")) else {
            return ProcessPresence::Unknown;
        };
        let Ok(cgroup) = read_file(&process.join("cgroup")) else {
            return ProcessPresence::Unknown;
        };
        // Do not combine the executable/cgroup of two generations if a PID is
        // reused while probing. Persisted sessions do not have start ticks, so
        // this cannot prove that a present PID still belongs to the old session.
        if read_file(&process.join("stat"))
            .ok()
            .and_then(|stat| process_generation(&stat, pid))
            != Some(generation)
        {
            return ProcessPresence::Unknown;
        }
        ProcessPresence::Present { executable, cgroup }
    }

    fn cgroup_is_empty(
        &self,
        recorded: &str,
        read_file: &impl Fn(&Path) -> io::Result<String>,
    ) -> bool {
        let Some(path) = unified_cgroup_path(recorded) else {
            return false;
        };
        // v2 "populated" includes descendants, unlike an empty cgroup.procs.
        // Missing/unmounted/v1 groups and permission errors remain unknown.
        // In particular, AppSession's default path may never have been created.
        read_file(
            &self
                .cgroup_root
                .join(path.trim_start_matches('/'))
                .join("cgroup.events"),
        )
        .ok()
        .is_some_and(|events| {
            events.lines().any(|line| {
                let mut fields = line.split_whitespace();
                fields.next() == Some("populated")
                    && fields.next() == Some("0")
                    && fields.next().is_none()
            })
        })
    }

    fn session_is_latency_sensitive(
        &self,
        session: &AppSession,
        app: Option<&AppIdentity>,
        tracked_pids: &[i32],
        steam_root: Option<&Path>,
        read_file: &impl Fn(&Path) -> io::Result<String>,
    ) -> bool {
        let mut present = false;
        for pid in std::iter::once(session.root_pid).chain(
            tracked_pids
                .iter()
                .copied()
                .filter(|pid| *pid != session.root_pid),
        ) {
            let (executable, cgroup) = match self.process(pid, read_file) {
                ProcessPresence::Absent => continue,
                ProcessPresence::Unknown => return true,
                ProcessPresence::Present { executable, cgroup } => (executable, cgroup),
            };
            present = true;
            // A reused PID must not turn an old game's session into a support
            // or Steam-launcher exemption. Only a recorded, matching application
            // executable can qualify; all ambiguous/mismatched PIDs protect.
            if app.and_then(|app| app.canonical_executable.as_deref()) != Some(executable.as_path())
            {
                return true;
            }
            if steam_launcher_session(session, app, Some(&executable), steam_root) {
                continue;
            }
            // A new support-service process can reuse an interpreter PID. Its
            // cgroup is only exemption evidence if the session recorded it too.
            let matching_cgroup = unified_cgroup_path(&session.cgroup_path)
                .zip(unified_cgroup_path(&cgroup))
                .is_some_and(|(recorded, current)| recorded == current);
            if latency_sensitive_session(
                session,
                app,
                Some(&executable),
                if matching_cgroup { &cgroup } else { "" },
            ) {
                return true;
            }
        }
        if present {
            // Every observed member is narrowly identified support plumbing.
            return false;
        }
        // Root exit alone is insufficient: check all tracked descendants above
        // and require positive evidence that the recorded cgroup's whole
        // subtree is empty. Shared/populated or unverifiable groups stay safe;
        // resolving untracked, reparented children needs stronger persisted
        // process identities than the current session model provides.
        !self.cgroup_is_empty(&session.cgroup_path, read_file)
    }
}

fn process_generation(stat: &str, pid: i32) -> Option<u64> {
    if stat.split_once(' ')?.0.parse::<i32>().ok()? != pid {
        return None;
    }
    // comm may itself contain spaces and ')'; starttime is field 22.
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

fn unified_cgroup_path(cgroup: &str) -> Option<&str> {
    let path = if cgroup.starts_with('/') {
        cgroup.trim()
    } else {
        cgroup.lines().find_map(|line| line.strip_prefix("0::"))?
    };
    if !path.starts_with('/')
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return None;
    }
    Some(path)
}

impl StateAgent {
    pub(crate) fn has_latency_sensitive_sessions(&self) -> Result<bool> {
        let steam_root = self
            .roots
            .lock()
            .steam_root
            .clone()
            .and_then(|root| std::fs::canonicalize(root).ok());
        let probe = WorkloadProbe {
            proc_root: Path::new("/proc"),
            cgroup_root: Path::new("/sys/fs/cgroup"),
        };
        for session in self.db.open_sessions()? {
            let app = self.db.get_app(&session.app_id)?;
            let tracked_pids = self.db.session_pids(session.session_id)?;
            if probe.session_is_latency_sensitive(
                &session,
                app.as_ref(),
                &tracked_pids,
                steam_root.as_deref(),
                &|path| std::fs::read_to_string(path),
            ) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod active_transfer_tests;

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

#[cfg(all(test, unix))]
mod session_admission_tests {
    use super::*;
    use noland_state_core::AppId;
    use std::cell::Cell;
    use std::fs;

    const ROOT_PID: i32 = 101;
    const CHILD_PID: i32 = 202;
    const GROUP: &str = "/fixture/application";

    struct Fixture {
        root: PathBuf,
        proc_root: PathBuf,
        cgroup_root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("noland-transfer-workload-{}", uuid::Uuid::new_v4()));
            let proc_root = root.join("proc");
            let cgroup_root = root.join("cgroup");
            fs::create_dir_all(&proc_root).unwrap();
            fs::create_dir_all(&cgroup_root).unwrap();
            Self {
                root,
                proc_root,
                cgroup_root,
            }
        }

        fn probe(&self) -> WorkloadProbe<'_> {
            WorkloadProbe {
                proc_root: &self.proc_root,
                cgroup_root: &self.cgroup_root,
            }
        }

        fn process(&self, pid: i32, executable: &str, cgroup: &str) {
            let process = self.proc_root.join(pid.to_string());
            fs::create_dir_all(&process).unwrap();
            fs::write(process.join("stat"), stat(pid, 1234)).unwrap();
            fs::write(process.join("cgroup"), format!("0::{cgroup}\n")).unwrap();
            std::os::unix::fs::symlink(executable, process.join("exe")).unwrap();
        }

        fn cgroup(&self, path: &str, populated: bool) {
            let group = self.cgroup_root.join(path.trim_start_matches('/'));
            fs::create_dir_all(&group).unwrap();
            fs::write(
                group.join("cgroup.events"),
                format!("populated {}\nfrozen 0\n", u8::from(populated)),
            )
            .unwrap();
        }

        fn sensitive(
            &self,
            session: &AppSession,
            app: Option<&AppIdentity>,
            tracked_pids: &[i32],
            steam_root: Option<&Path>,
        ) -> bool {
            self.probe().session_is_latency_sensitive(
                session,
                app,
                tracked_pids,
                steam_root,
                &|path| fs::read_to_string(path),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn session(executable: &str) -> (AppSession, AppIdentity) {
        let id = AppId("exe:fixture".into());
        let mut session = AppSession::new(id.clone(), ROOT_PID, SessionSource::ExecutableDiscovery);
        session.cgroup_path = GROUP.into();
        let mut app = AppIdentity::new(id, "fixture");
        app.canonical_executable = Some(executable.into());
        (session, app)
    }

    fn stat(pid: i32, start_ticks: u64) -> String {
        // Fields 4..21 precede starttime; comm deliberately contains ')' and spaces.
        format!(
            "{pid} (fixture ) process) S {} {start_ticks} 0\n",
            ["0"; 18].join(" ")
        )
    }

    #[test]
    fn proven_empty_sessions_do_not_protect_even_explicit_launch_sources() {
        let fixture = Fixture::new();
        fixture.cgroup(GROUP, false);
        let (mut session, app) = session("/usr/bin/python3");
        for source in [
            SessionSource::DesktopEntry,
            SessionSource::Steam,
            SessionSource::NolandLaunch,
            SessionSource::ManualBinding,
            SessionSource::ExecutableDiscovery,
        ] {
            session.source = source;
            assert!(!fixture.sensitive(&session, Some(&app), &[ROOT_PID, CHILD_PID], None));
        }
    }

    #[test]
    fn a_missing_root_never_hides_a_live_tracked_child() {
        let fixture = Fixture::new();
        fixture.process(CHILD_PID, "/usr/bin/python3", GROUP);
        // Even contradictory/stale cgroup information cannot override a live PID.
        fixture.cgroup(GROUP, false);
        let (mut session, app) = session("/usr/bin/python3");
        for source in [
            SessionSource::DesktopEntry,
            SessionSource::Steam,
            SessionSource::ExecutableDiscovery,
        ] {
            session.source = source;
            assert!(fixture.sensitive(&session, Some(&app), &[ROOT_PID, CHILD_PID], None));
        }
    }

    #[test]
    fn populated_subtrees_protect_untracked_children_and_shared_desktop_sessions() {
        let fixture = Fixture::new();
        let (mut session, app) = session("/home/user/Games/game");
        for group in [GROUP, "/system.slice/noland-desktop.service"] {
            session.cgroup_path = format!("0::{group}\n");
            fixture.cgroup(group, true);
            assert!(fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));
        }
        // An empty direct PID list cannot establish subtree emptiness.
        let group = fixture.cgroup_root.join(GROUP.trim_start_matches('/'));
        fs::write(group.join("cgroup.procs"), "").unwrap();
        assert!(!fixture
            .probe()
            .cgroup_is_empty(GROUP, &|path| fs::read_to_string(path)));
    }

    #[test]
    fn absent_processes_differ_from_permission_denied_or_incomplete_procfs() {
        let fixture = Fixture::new();
        fixture.cgroup(GROUP, false);
        let (session, app) = session("/home/user/Games/game");
        let stat_path = fixture.proc_root.join(ROOT_PID.to_string()).join("stat");
        assert!(matches!(
            fixture
                .probe()
                .process(ROOT_PID, &|path| fs::read_to_string(path)),
            ProcessPresence::Absent
        ));
        let denied = |path: &Path| {
            if path == stat_path {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                fs::read_to_string(path)
            }
        };
        assert!(matches!(
            fixture.probe().process(ROOT_PID, &denied),
            ProcessPresence::Unknown
        ));
        assert!(fixture.probe().session_is_latency_sensitive(
            &session,
            Some(&app),
            &[ROOT_PID],
            None,
            &denied,
        ));
        // A directory without a readable stat must not look like a dead process.
        fs::create_dir_all(fixture.proc_root.join(ROOT_PID.to_string())).unwrap();
        assert!(fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));
        let unavailable = WorkloadProbe {
            proc_root: &fixture.root.join("unmounted-proc"),
            cgroup_root: &fixture.cgroup_root,
        };
        assert!(unavailable.session_is_latency_sensitive(
            &session,
            Some(&app),
            &[],
            None,
            &|path| fs::read_to_string(path),
        ));
    }

    #[test]
    fn unknown_or_unreadable_cgroups_cannot_prove_a_session_dead() {
        let fixture = Fixture::new();
        let (mut session, app) = session("/home/user/Games/game");
        assert!(fixture.sensitive(&session, Some(&app), &[], None));
        fixture.cgroup(GROUP, false);
        let denied = |_: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(!fixture.probe().cgroup_is_empty(GROUP, &denied));
        for group in [
            "",
            "/never-created-session-group",
            "1:cpu:/fixture/application",
            "0::/fixture/../application",
        ] {
            session.cgroup_path = group.into();
            assert!(fixture.sensitive(&session, Some(&app), &[], None));
        }
        fs::write(
            fixture
                .cgroup_root
                .join(GROUP.trim_start_matches('/'))
                .join("cgroup.events"),
            "frozen 0\n",
        )
        .unwrap();
        session.cgroup_path = GROUP.into();
        assert!(fixture.sensitive(&session, Some(&app), &[], None));
    }

    #[test]
    fn reused_or_changing_pids_do_not_grant_support_exemptions() {
        let fixture = Fixture::new();
        fixture.process(
            ROOT_PID,
            "/usr/bin/sunshine",
            "/system.slice/sunshine.service",
        );
        let (session, app) = session("/home/user/Games/game");
        assert!(fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));

        let (session, app) = self::session("/usr/bin/sunshine");
        let stat_path = fixture.proc_root.join(ROOT_PID.to_string()).join("stat");
        let reads = Cell::new(0);
        let changing_generation = |path: &Path| {
            if path == stat_path {
                let generation = reads.get();
                reads.set(generation + 1);
                Ok(stat(ROOT_PID, generation))
            } else {
                fs::read_to_string(path)
            }
        };
        assert!(fixture.probe().session_is_latency_sensitive(
            &session,
            Some(&app),
            &[ROOT_PID],
            None,
            &changing_generation,
        ));
    }

    #[test]
    fn reused_interpreter_pid_cannot_borrow_a_new_support_service_identity() {
        let fixture = Fixture::new();
        fixture.process(
            ROOT_PID,
            "/usr/bin/python3",
            "/system.slice/selkies.service",
        );
        let (session, app) = session("/usr/bin/python3");
        assert!(fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));
    }

    #[test]
    fn unknown_and_explicit_interpreter_applications_stay_protected() {
        let fixture = Fixture::new();
        fixture.process(ROOT_PID, "/usr/bin/python3", GROUP);
        let (mut session, app) = session("/usr/bin/python3");
        assert!(fixture.sensitive(&session, None, &[ROOT_PID], None));
        for source in [
            SessionSource::DesktopEntry,
            SessionSource::Steam,
            SessionSource::NolandLaunch,
            SessionSource::ManualBinding,
        ] {
            session.source = source;
            assert!(fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));
        }
    }

    #[test]
    fn unreadable_live_identity_never_falls_back_to_a_support_exemption() {
        let fixture = Fixture::new();
        let executable = "/opt/selkies-gstreamer/bin/python3";
        fixture.process(ROOT_PID, executable, GROUP);
        let (session, app) = session(executable);
        assert!(!fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));
        let denied = |path: &Path| {
            if path.file_name().is_some_and(|name| name == "cgroup") {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                fs::read_to_string(path)
            }
        };
        assert!(fixture.probe().session_is_latency_sensitive(
            &session,
            Some(&app),
            &[ROOT_PID],
            None,
            &denied,
        ));
        fs::remove_file(fixture.proc_root.join(ROOT_PID.to_string()).join("exe")).unwrap();
        assert!(fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));
    }

    #[test]
    fn only_recorded_support_services_and_registered_independent_steam_launchers_are_exempt() {
        let fixture = Fixture::new();
        let support_group = "/system.slice/selkies.service";
        fixture.process(ROOT_PID, "/usr/bin/python3", support_group);
        let (mut session, app) = session("/usr/bin/python3");
        session.cgroup_path = format!("0::{support_group}");
        assert!(!fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));
        session.source = SessionSource::DesktopEntry;
        assert!(fixture.sensitive(&session, Some(&app), &[ROOT_PID], None));

        let steam = Fixture::new();
        let root = Path::new("/home/user/.local/share/Steam");
        let executable = root.join("ubuntu12_32/steam");
        steam.process(ROOT_PID, executable.to_str().unwrap(), GROUP);
        let (mut session, mut app) = self::session(executable.to_str().unwrap());
        assert!(!steam.sensitive(&session, Some(&app), &[ROOT_PID], Some(root)));
        assert!(steam.sensitive(&session, Some(&app), &[ROOT_PID], None));
        session.source = SessionSource::Steam;
        assert!(steam.sensitive(&session, Some(&app), &[ROOT_PID], Some(root)));
        session.source = SessionSource::ExecutableDiscovery;
        app.steam_app_id = Some(553850);
        assert!(steam.sensitive(&session, Some(&app), &[ROOT_PID], Some(root)));
        app.steam_app_id = None;
        steam.process(CHILD_PID, "/home/user/Games/game", GROUP);
        assert!(steam.sensitive(&session, Some(&app), &[ROOT_PID, CHILD_PID], Some(root),));
    }

    #[test]
    fn process_generation_parser_handles_comm_parentheses_and_rejects_ambiguous_stat() {
        assert_eq!(
            process_generation(&stat(ROOT_PID, 5678), ROOT_PID),
            Some(5678)
        );
        assert_eq!(process_generation(&stat(ROOT_PID, 5678), CHILD_PID), None);
        assert_eq!(process_generation("101 (incomplete) S 0", ROOT_PID), None);
    }
}
