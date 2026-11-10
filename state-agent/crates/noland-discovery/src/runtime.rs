use std::path::{Component, Path, PathBuf};

/// Translate the runtime's private-home namespace to a durable host path.
/// Refuse ambiguous matches and symlink escapes rather than borrowing another app's files.
pub fn resolve_runtime_executable(home: &Path, executable: &Path) -> Option<PathBuf> {
    let relative = executable.strip_prefix("/home/runtime").ok()?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let base = std::fs::canonicalize(home.join(".local/share/noland-runtime/app-data")).ok()?;
    let mut found = None;
    for entry in std::fs::read_dir(&base).ok()?.flatten() {
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let private_home = entry.path();
        let Ok(candidate) = std::fs::canonicalize(private_home.join(relative)) else {
            continue;
        };
        if !candidate.starts_with(&private_home) || !candidate.is_file() {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(candidate);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{derive_owned_install_root, fallback_exe_identity};

    struct TestHome(PathBuf);

    impl TestHome {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("noland-runtime-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn maps_private_executable_and_derives_only_its_game_root() {
        let temp = TestHome::new();
        let home = temp.path();
        let game = home.join(".local/share/noland-runtime/app-data/one/Games/Hookshot");
        std::fs::create_dir_all(game.join("bin")).unwrap();
        let host_exe = game.join("bin/hookshot");
        std::fs::write(&host_exe, b"game").unwrap();
        let namespace_exe = Path::new("/home/runtime/Games/Hookshot/bin/hookshot");
        assert_eq!(
            resolve_runtime_executable(home, namespace_exe),
            Some(host_exe.clone())
        );
        let mut app = fallback_exe_identity(namespace_exe);
        let original_id = app.app_id.clone();
        app.canonical_executable = Some(host_exe);
        assert_eq!(derive_owned_install_root(home, &app), Some(game));
        assert_eq!(app.app_id, original_id);
    }

    #[test]
    fn rejects_ambiguous_matches_and_parent_traversal() {
        let temp = TestHome::new();
        for id in ["one", "two"] {
            let root = temp
                .path()
                .join(".local/share/noland-runtime/app-data")
                .join(id);
            std::fs::create_dir_all(root.join("Games/Hookshot/bin")).unwrap();
            std::fs::write(root.join("Games/Hookshot/bin/hookshot"), b"game").unwrap();
        }
        assert_eq!(
            resolve_runtime_executable(
                temp.path(),
                Path::new("/home/runtime/Games/Hookshot/bin/hookshot")
            ),
            None
        );
        assert_eq!(
            resolve_runtime_executable(temp.path(), Path::new("/home/runtime/../secret")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_into_other_app() {
        let temp = TestHome::new();
        let root = temp.path().join(".local/share/noland-runtime/app-data/one");
        std::fs::create_dir_all(&root).unwrap();
        let outside = temp.path().join("outside");
        std::fs::write(&outside, b"private").unwrap();
        std::os::unix::fs::symlink(outside, root.join("game")).unwrap();
        assert_eq!(
            resolve_runtime_executable(temp.path(), Path::new("/home/runtime/game")),
            None
        );
    }
}
