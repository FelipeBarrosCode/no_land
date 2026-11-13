//! Approved Ubuntu packages go into a new system image, never the parent OS.
use crate::{
    artifact, elf, engine,
    session::clean_command,
    store::{atomic_json, Store},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const HELPER: &str = "/usr/local/libexec/noland-runtime-maintain";
const MAX_FILES: usize = 8192;

#[derive(Deserialize)]
struct Catalog {
    schema: u32,
    libraries: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrerequisitePlan {
    pub app_id: String,
    pub image_id: String,
    pub missing_libraries: Vec<String>,
    pub packages: Vec<String>,
    pub unsupported_libraries: Vec<String>,
    pub warnings: Vec<String>,
    pub approval_digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateResult {
    pub image_id: String,
    pub changed: bool,
}

/// Resolve absolute links against the guest root, NOT the parent's /usr/lib.
fn guest_file(root: &Path, guest: &Path) -> Option<PathBuf> {
    let mut pending = VecDeque::from_iter(guest.components().map(|c| c.as_os_str().to_owned()));
    let mut relative = PathBuf::new();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == "/" {
            relative.clear();
            continue;
        }
        if part == "." {
            continue;
        }
        if part == ".." {
            relative.pop();
            continue;
        }
        let candidate = root.join(&relative).join(&part);
        let metadata = fs::symlink_metadata(&candidate).ok()?;
        if metadata.file_type().is_symlink() {
            links += 1;
            if links > 40 {
                return None;
            }
            let target = fs::read_link(candidate).ok()?;
            let mut prefix =
                VecDeque::from_iter(target.components().map(|c| c.as_os_str().to_owned()));
            prefix.append(&mut pending);
            pending = prefix;
        } else {
            relative.push(part);
        }
    }
    let path = root.join(relative);
    fs::metadata(&path).ok()?.is_file().then_some(path)
}

fn library(
    root: &Path,
    app: &Path,
    origin: &Path,
    paths: &[String],
    name: &str,
) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = paths
        .iter()
        .filter_map(|path| {
            let path = path
                .replace("${ORIGIN}", &origin.to_string_lossy())
                .replace("$ORIGIN", &origin.to_string_lossy());
            (!path.contains('$') && Path::new(&path).is_absolute()).then(|| PathBuf::from(path))
        })
        .collect();
    dirs.extend(
        [
            "/app",
            "/app/lib",
            "/app/lib64",
            "/app/usr/lib",
            "/app/usr/lib/x86_64-linux-gnu",
            "/lib/x86_64-linux-gnu",
            "/usr/lib/x86_64-linux-gnu",
            "/lib64",
            "/usr/lib64",
            "/lib",
            "/usr/lib",
        ]
        .map(PathBuf::from),
    );
    dirs.into_iter().find_map(|dir| {
        let guest = dir.join(name);
        if let Ok(relative) = guest.strip_prefix("/app") {
            guest_file(app, &Path::new("/").join(relative))
        } else {
            guest_file(root, &guest)
        }
    })
}

fn app_elfs(directory: &Path, entry: &Path, appimage: bool) -> Result<Vec<PathBuf>> {
    if !appimage {
        return Ok(vec![entry.to_owned()]);
    }
    let mut result = Vec::new();
    let mut queue = vec![directory.to_owned()];
    let mut seen = 0;
    while let Some(path) = queue.pop() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            seen += 1;
            anyhow::ensure!(seen <= MAX_FILES, "AppImage has too many entries for automatic inspection; no packages were installed");
            let kind = entry.file_type()?;
            if kind.is_dir() {
                queue.push(entry.path());
            } else if kind.is_file() {
                let mut magic = [0; 6];
                if fs::File::open(entry.path())?.read_exact(&mut magic).is_ok()
                    && &magic == b"\x7fELF\x02\x01"
                {
                    result.push(entry.path());
                }
            }
        }
    }
    anyhow::ensure!(
        !result.is_empty(),
        "No x86-64 ELF payload found in this AppImage"
    );
    Ok(result)
}

pub fn inspect(store: &Store, app_id: &str) -> Result<PrerequisitePlan> {
    let app = store.app(app_id)?;
    let root = store.root()?;
    let image_id = store
        .activation()?
        .current
        .context("Runtime is not installed")?;
    let (directory, entry) = engine::dependency_files(store, &app)?;
    let catalog: Catalog = serde_json::from_str(include_str!(
        "../../runtime-images/ubuntu24/prerequisites.json"
    ))?;
    anyhow::ensure!(catalog.schema == 1, "Unsupported prerequisite catalog");
    let mut queue = VecDeque::from(app_elfs(&directory, &entry, app.appimage)?);
    let mut seen = BTreeSet::new();
    let mut missing = BTreeSet::new();
    while let Some(path) = queue.pop_front() {
        if !seen.insert(path.clone()) {
            continue;
        }
        anyhow::ensure!(
            seen.len() <= MAX_FILES,
            "Dependency inspection exceeded its file limit; no packages were installed"
        );
        let required =
            elf::inspect(&path).with_context(|| format!("Cannot inspect {}", path.display()))?;
        let relative = path.strip_prefix(&directory).ok();
        let origin = relative
            .map(|p| Path::new("/app").join(p).parent().unwrap().to_owned())
            .or_else(|| {
                path.strip_prefix(&root)
                    .ok()
                    .map(|p| Path::new("/").join(p).parent().unwrap().to_owned())
            })
            .unwrap_or_else(|| PathBuf::from("/app"));
        for name in required.needed {
            if let Some(found) = library(&root, &directory, &origin, &required.search_paths, &name)
            {
                if !seen.contains(&found) {
                    anyhow::ensure!(queue.len() < MAX_FILES, "Dependency inspection queue exceeded its limit; no packages were installed");
                    queue.push_back(found);
                }
            } else {
                missing.insert(name);
            }
        }
    }
    let packages: BTreeSet<_> = missing
        .iter()
        .filter_map(|name| catalog.libraries.get(name).cloned())
        .collect();
    let unsupported = missing
        .iter()
        .filter(|name| !catalog.libraries.contains_key(*name))
        .cloned()
        .collect();
    let mut plan = PrerequisitePlan { app_id: app.id, image_id, missing_libraries: missing.into_iter().collect(), packages: packages.into_iter().collect(), unsupported_libraries: unsupported,
        warnings: vec!["Static library inspection cannot predict every plugin, dlopen dependency, launch wrapper or required ABI version. NVIDIA drivers are never downloaded by this installer.".into()], approval_digest: String::new() };
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(&plan)?);
    hash.update(artifact::sha256(&app.file)?.as_bytes());
    plan.approval_digest = hex::encode(hash.finalize());
    Ok(plan)
}

fn invoke(store: &Store, action: &str, image: &str, packages: &[String]) -> Result<UpdateResult> {
    anyhow::ensure!(
        cfg!(target_os = "linux"),
        "Runtime maintenance requires Linux"
    );
    anyhow::ensure!(
        crate::valid_id(image) && image.starts_with("ubuntu24-"),
        "Only provisioned Ubuntu runtime images can be updated"
    );
    // Capture the selected image before the helper advances the global default.
    // Otherwise a user with no activation.json would lose its rollback target.
    let mut activation = store.activation()?;
    anyhow::ensure!(
        activation.current.as_deref() == Some(image),
        "Runtime selection changed before maintenance"
    );
    let mut command = clean_command("sudo");
    command
        .args(["-n", "--", HELPER, action, image])
        .args(packages);
    let output = command
        .output()
        .context("Runtime maintenance helper unavailable; resume Noland Connect provisioning")?;
    // Package-manager diagnostics stay in the root-owned maintenance log.
    anyhow::ensure!(output.status.success(), "Runtime maintenance failed; the previous image and app data were preserved. See /var/log/noland-runtime-maintenance.log");
    let result: UpdateResult =
        serde_json::from_slice(&output.stdout).context("Invalid runtime maintenance response")?;
    anyhow::ensure!(
        crate::valid_id(&result.image_id) && result.image_id.starts_with("ubuntu24-"),
        "Invalid updated runtime ID"
    );
    artifact::validate_root(&store.root_for_id(&result.image_id)?)?;
    if activation.current.as_deref() != Some(&result.image_id) {
        activation.previous = activation.current.take();
        activation.current = Some(result.image_id.clone());
        atomic_json(&store.data.join("activation.json"), &activation)?;
    }
    store.progress(
        "ready",
        "Runtime updated; existing apps and saves preserved",
        None,
    )?;
    Ok(result)
}

pub fn update(store: &Store, approved: bool) -> Result<UpdateResult> {
    anyhow::ensure!(approved, "Runtime updates require --approve");
    let _lock = store.lock()?;
    let image = store
        .activation()?
        .current
        .context("Runtime is not installed; resume provisioning")?;
    invoke(store, "update", &image, &[])
}

pub fn install_prerequisites(
    store: &Store,
    app_id: &str,
    approved: bool,
    expected: Option<&str>,
) -> Result<PrerequisitePlan> {
    anyhow::ensure!(approved, "Installing app prerequisites requires --approve");
    let plan = inspect(store, app_id)?;
    anyhow::ensure!(
        expected.is_none_or(|digest| digest == plan.approval_digest),
        "Application or prerequisites changed since approval; inspect and approve again"
    );
    anyhow::ensure!(plan.unsupported_libraries.is_empty(), "Some dependencies are not supported by the approved Ubuntu catalog: {}. No packages were installed.", plan.unsupported_libraries.join(", "));
    if !plan.packages.is_empty() {
        let _lock = store.lock()?;
        anyhow::ensure!(
            store.activation()?.current.as_deref() == Some(&plan.image_id),
            "Runtime changed since inspection; inspect again"
        );
        invoke(store, "install", &plan.image_id, &plan.packages)?;
    }
    let verified = inspect(store, app_id)?;
    anyhow::ensure!(verified.missing_libraries.is_empty(), "Packages were installed but these dependencies remain unresolved: {}. The previous runtime image remains available for rollback.", verified.missing_libraries.join(", "));
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PROFILE;
    #[test]
    fn absolute_guest_symlinks_never_resolve_into_host_libraries() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("usr/lib")).unwrap();
        fs::write(temp.path().join("usr/lib/library.so"), b"guest").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/usr/lib", temp.path().join("lib")).unwrap();
            assert_eq!(
                guest_file(temp.path(), Path::new("/lib/library.so")),
                Some(temp.path().join("usr/lib/library.so"))
            );
            std::os::unix::fs::symlink("/etc/passwd", temp.path().join("bad")).unwrap();
            assert!(guest_file(temp.path(), Path::new("/bad")).is_none());
        }
    }
    #[test]
    fn catalog_contains_only_literal_library_names_and_package_ids() {
        let catalog: Catalog = serde_json::from_str(include_str!(
            "../../runtime-images/ubuntu24/prerequisites.json"
        ))
        .unwrap();
        assert_eq!(catalog.schema, 1);
        assert!(catalog.libraries.len() > 50);
        for (name, package) in catalog.libraries {
            assert!(name.starts_with("lib") && !name.contains('/'));
            assert!(
                !package.starts_with('-')
                    && package.bytes().all(|c| c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || b"+.-".contains(&c))
            );
            assert!(!package.contains("nvidia") && !package.starts_with("linux-"));
        }
    }

    #[test]
    fn detects_missing_libraries_but_reuses_bundled_dependencies_without_running_them() {
        use crate::store::{Activation, App};
        let temp = tempfile::tempdir().unwrap();
        let store = Store {
            data: temp.path().join("data"),
            state: temp.path().join("state"),
        };
        store.init().unwrap();
        fs::create_dir(store.data.join("images/fixture")).unwrap();
        atomic_json(
            &store.data.join("activation.json"),
            &Activation {
                current: Some("fixture".into()),
                previous: None,
            },
        )
        .unwrap();
        let app_dir = temp.path().join("app");
        fs::create_dir(&app_dir).unwrap();
        let app = App {
            id: "fixture-app".into(),
            name: "Fixture".into(),
            file: app_dir.join("main"),
            profile: PROFILE.into(),
            appimage: false,
        };
        fs::write(&app.file, crate::elf::tests::elf(b"libopenal.so.1\0")).unwrap();
        atomic_json(&store.data.join("apps/fixture-app.json"), &app).unwrap();
        let first = inspect(&store, &app.id).unwrap();
        assert_eq!(first.packages, ["libopenal1"]);
        assert!(first.unsupported_libraries.is_empty());
        assert!(install_prerequisites(&store, &app.id, false, None).is_err());
        assert!(install_prerequisites(&store, &app.id, true, Some("stale-approval")).is_err());
        // A static bundled ELF has no dependencies; no host execution is needed.
        let mut static_elf = vec![0; 64];
        static_elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        static_elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        fs::write(app_dir.join("libopenal.so.1"), static_elf).unwrap();
        let second = inspect(&store, &app.id).unwrap();
        assert!(second.missing_libraries.is_empty());
        assert!(second.packages.is_empty());
        assert_ne!(first.approval_digest, second.approval_digest);
        fs::write(
            &app.file,
            crate::elf::tests::elf(b"libunknown-vendor.so.99\0"),
        )
        .unwrap();
        let unknown = inspect(&store, &app.id).unwrap();
        assert_eq!(unknown.unsupported_libraries, ["libunknown-vendor.so.99"]);
        assert!(install_prerequisites(&store, &app.id, true, None).is_err());
    }
}
