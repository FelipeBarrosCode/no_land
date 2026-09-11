use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use noland_state_core::{AppId, AppIdentity, LauncherKind};

#[derive(Debug, Clone)]
pub struct PrefixDiscovery {
    pub id: String,
    pub path: PathBuf,
    pub kind: PrefixKind,
    pub associated_app: Option<AppId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixKind {
    Wine,
    Bottles,
    Proton,
}

impl PrefixDiscovery {
    pub fn to_identity(&self) -> Option<AppIdentity> {
        let app_id = self.associated_app.clone()?;
        let launcher = match self.kind {
            PrefixKind::Wine => LauncherKind::Wine,
            PrefixKind::Bottles => LauncherKind::Bottles,
            PrefixKind::Proton => LauncherKind::Proton,
        };
        Some(AppIdentity {
            launcher: Some(launcher),
            identity_confidence: 0.95,
            ..AppIdentity::new(app_id, self.id.clone())
        })
    }
}

pub fn discover_wine_prefixes(home: &Path) -> Vec<PrefixDiscovery> {
    let mut out = Vec::new();
    let default = home.join(".wine");
    if is_wine_prefix(&default) {
        out.push(wine_prefix("default".into(), default));
    }
    for dir in [
        home.join(".local/share/wineprefixes"),
        home.join(".wine-prefixes"),
    ] {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_wine_prefix(&path) {
                out.push(wine_prefix(
                    entry.file_name().to_string_lossy().into_owned(),
                    path,
                ));
            }
        }
    }

    // A prefix name is a durable identity within the user's Wine prefix collection only
    // while it is unambiguous. Do not merge same-named prefixes from different roots.
    let mut counts = HashMap::new();
    for prefix in &out {
        *counts.entry(prefix.id.clone()).or_insert(0usize) += 1;
    }
    for prefix in &mut out {
        if counts.get(&prefix.id).copied().unwrap_or_default() > 1 {
            prefix.associated_app = None;
        }
    }
    out
}

fn is_wine_prefix(path: &Path) -> bool {
    path.is_dir() && (path.join("system.reg").exists() || path.join("drive_c").is_dir())
}

fn wine_prefix(id: String, path: PathBuf) -> PrefixDiscovery {
    PrefixDiscovery {
        associated_app: Some(AppId::launcher("wine", &id)),
        id,
        path,
        kind: PrefixKind::Wine,
    }
}

pub fn discover_bottles(home: &Path) -> Vec<PrefixDiscovery> {
    let mut roots = vec![
        home.join(".local/share/bottles/bottles"),
        home.join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
    ];
    if let Ok(custom) = std::env::var("NOLAND_BOTTLES_ROOT") {
        roots.push(PathBuf::from(custom));
    }
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let id = entry.file_name().to_string_lossy().into_owned();
                out.push(PrefixDiscovery {
                    associated_app: Some(AppId::launcher("bottles", &id)),
                    id,
                    path,
                    kind: PrefixKind::Bottles,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "noland-wine-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn discovered_prefixes_produce_launcher_identities() {
        let home = test_home("identities");
        let wine = home.join(".wine/drive_c");
        let bottle = home.join(".local/share/bottles/bottles/arcade");
        fs::create_dir_all(&wine).unwrap();
        fs::create_dir_all(&bottle).unwrap();

        let wine = discover_wine_prefixes(&home).pop().unwrap();
        assert_eq!(
            wine.associated_app,
            Some(AppId::launcher("wine", "default"))
        );
        assert_eq!(
            wine.to_identity().unwrap().launcher,
            Some(LauncherKind::Wine)
        );

        let bottle = discover_bottles(&home)
            .into_iter()
            .find(|prefix| prefix.id == "arcade")
            .unwrap();
        assert_eq!(
            bottle.associated_app,
            Some(AppId::launcher("bottles", "arcade"))
        );
        assert_eq!(
            bottle.to_identity().unwrap().launcher,
            Some(LauncherKind::Bottles)
        );

        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn same_named_wine_prefixes_are_not_given_an_ambiguous_app_identity() {
        let home = test_home("duplicates");
        for root in [
            home.join(".local/share/wineprefixes/game/drive_c"),
            home.join(".wine-prefixes/game/drive_c"),
        ] {
            fs::create_dir_all(root).unwrap();
        }

        let prefixes = discover_wine_prefixes(&home);
        assert_eq!(prefixes.len(), 2);
        assert!(prefixes
            .iter()
            .all(|prefix| prefix.associated_app.is_none()));

        fs::remove_dir_all(home).ok();
    }
}
