use std::fs;
use std::path::{Path, PathBuf};

use noland_state_core::AppId;

#[derive(Debug, Clone)]
pub struct PrefixDiscovery {
    pub id: String,
    pub path: PathBuf,
    pub kind: PrefixKind,
    pub associated_app: Option<AppId>,
}

#[derive(Debug, Clone, Copy)]
pub enum PrefixKind {
    Wine,
    Bottles,
    Proton,
}

pub fn discover_wine_prefixes(home: &Path) -> Vec<PrefixDiscovery> {
    let mut out = Vec::new();
    let default = home.join(".wine");
    if default.exists() {
        out.push(PrefixDiscovery {
            id: "default".into(),
            path: default,
            kind: PrefixKind::Wine,
            associated_app: None,
        });
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
            if path.join("system.reg").exists() || path.join("drive_c").exists() {
                out.push(PrefixDiscovery {
                    id: entry.file_name().to_string_lossy().into_owned(),
                    path,
                    kind: PrefixKind::Wine,
                    associated_app: None,
                });
            }
        }
    }
    out
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
