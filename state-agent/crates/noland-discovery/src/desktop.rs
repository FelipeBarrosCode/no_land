use std::fs;
use std::path::{Path, PathBuf};

use noland_state_core::*;

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub id: String,
    pub name: String,
    pub exec: Option<PathBuf>,
    pub icon: Option<PathBuf>,
    pub path: PathBuf,
}

impl DesktopEntry {
    pub fn to_identity(&self) -> AppIdentity {
        AppIdentity {
            desktop_entry_id: Some(self.id.clone()),
            canonical_executable: self.exec.clone(),
            launcher: Some(LauncherKind::Native),
            identity_confidence: 0.85,
            icon_path: self.icon.clone(),
            ..AppIdentity::new(AppId::desktop(&self.id), self.name.clone())
        }
    }
}

pub fn discover_desktop_apps(home: &Path) -> Vec<AppIdentity> {
    let mut dirs = vec![
        home.join(".local/share/applications"),
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Ok(xdg) = std::env::var("XDG_DATA_DIRS") {
        for part in xdg.split(':') {
            dirs.push(PathBuf::from(part).join("applications"));
        }
    }
    let mut apps = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(parsed) = parse_desktop_entry(&path) {
                let identity = parsed.to_identity();
                let user_installed = path.starts_with(home);
                if user_installed || crate::portable::is_backup_candidate(&identity) {
                    apps.push(identity);
                }
            }
        }
    }
    apps
}

pub fn parse_desktop_entry(path: &Path) -> Option<DesktopEntry> {
    let text = fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut in_desktop_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "Name" => name = Some(v.trim().to_string()),
                "Exec" => exec = executable_from_exec(v.trim()),
                "Icon" => icon = Some(PathBuf::from(v.trim())),
                "NoDisplay" if v.trim().eq_ignore_ascii_case("true") => return None,
                "Hidden" if v.trim().eq_ignore_ascii_case("true") => return None,
                _ => {}
            }
        }
    }
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());
    Some(DesktopEntry {
        name: name.unwrap_or_else(|| id.clone()),
        id,
        exec,
        icon,
        path: path.to_path_buf(),
    })
}

fn executable_from_exec(exec: &str) -> Option<PathBuf> {
    let tokens = exec_tokens(exec)?;
    let mut index = 0;

    if tokens
        .first()
        .is_some_and(|token| command_name(token) == "env")
    {
        index += 1;
        while let Some(token) = tokens.get(index) {
            if token == "--" {
                index += 1;
                break;
            }
            if matches!(token.as_str(), "-u" | "--unset" | "-C" | "--chdir") {
                index += 2;
            } else if token.starts_with('-') || is_env_assignment(token) {
                index += 1;
            } else {
                break;
            }
        }
    }

    let command = tokens.get(index)?;
    if command.starts_with('%') || is_generic_wrapper(command) {
        return None;
    }
    Some(PathBuf::from(command))
}

fn exec_tokens(exec: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in exec.chars() {
        if escaped {
            token.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if quote.is_none() && ch.is_whitespace() {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(ch);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn command_name(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

fn is_env_assignment(token: &str) -> bool {
    token
        .split_once('=')
        .is_some_and(|(name, _)| !name.is_empty() && !name.contains('/'))
}

fn is_generic_wrapper(command: &str) -> bool {
    matches!(
        command_name(command),
        "flatpak" | "gtk-launch" | "sh" | "bash" | "dash" | "zsh"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn recovers_direct_and_env_executables() {
        assert_eq!(
            executable_from_exec("\"/opt/Example App/bin/app\" --open %U"),
            Some(PathBuf::from("/opt/Example App/bin/app"))
        );
        assert_eq!(
            executable_from_exec("/usr/bin/env FOO=bar -- /opt/example/bin/app %f"),
            Some(PathBuf::from("/opt/example/bin/app"))
        );
        assert_eq!(
            executable_from_exec("env -u OLD_SETTING example-app --new-window"),
            Some(PathBuf::from("example-app"))
        );
    }

    #[test]
    fn rejects_generic_wrappers_and_unsafe_exec_lines() {
        for exec in [
            "flatpak run org.example.App",
            "/bin/sh -c /opt/example/app",
            "gtk-launch org.example.App",
            "env FOO=bar /usr/bin/flatpak run org.example.App",
            "\"/opt/unterminated app",
        ] {
            assert_eq!(
                executable_from_exec(exec),
                None,
                "unexpected target for {exec}"
            );
        }
    }

    #[test]
    fn parses_desktop_file() {
        let dir = std::env::temp_dir().join(format!("noland-desktop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("example-game.desktop");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "[Desktop Entry]\nName=Example Game\nExec=/opt/example-game/bin/game %u\nIcon=example\n"
        )
        .unwrap();
        let parsed = parse_desktop_entry(&path).unwrap();
        assert_eq!(parsed.name, "Example Game");
        assert_eq!(
            parsed.exec.unwrap(),
            PathBuf::from("/opt/example-game/bin/game")
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
