use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Store {
    pub data: PathBuf,
    pub state: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct App {
    pub id: String,
    pub name: String,
    pub file: PathBuf,
    pub profile: String,
    pub appimage: bool,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activation {
    pub current: Option<String>,
    pub previous: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: String,
    pub message: String,
    pub error: Option<String>,
}

impl Store {
    pub fn open() -> Result<Self> {
        let home = PathBuf::from(std::env::var("HOME").context("HOME is required")?);
        anyhow::ensure!(home.is_absolute(), "HOME must be absolute");
        let store = Self {
            data: home.join(".local/share/noland-runtime"),
            state: home.join(".local/state/noland-runtime"),
        };
        store.init()?;
        Ok(store)
    }
    pub fn init(&self) -> Result<()> {
        for dir in [
            self.data.clone(),
            self.state.clone(),
            self.data.join("images"),
            self.data.join("apps"),
            self.data.join("cache"),
            self.data.join("app-data"),
        ] {
            private_dir(&dir)?;
        }
        Ok(())
    }
    pub fn lock(&self) -> Result<File> {
        let f = private_file(&self.state.join("install.lock"))?;
        f.try_lock_exclusive()
            .context("Another runtime operation is active")?;
        Ok(f)
    }
    pub fn activation(&self) -> Result<Activation> {
        let mut activation: Activation = read_or_default(&self.data.join("activation.json"))?;
        if activation.current.is_none() {
            activation.current = system_image()?.map(|(id, _)| id);
        }
        Ok(activation)
    }
    pub fn progress(&self, phase: &str, message: &str, error: Option<String>) -> Result<()> {
        atomic_json(
            &self.state.join("progress.json"),
            &Progress {
                phase: phase.into(),
                message: message.into(),
                error,
            },
        )
    }
    pub fn cancelled(&self) -> Result<()> {
        if self.state.join("cancel").exists() {
            bail!("Installation cancelled; retry will reuse the partial download");
        }
        Ok(())
    }
    pub fn apps(&self) -> Result<Vec<App>> {
        let mut apps = Vec::new();
        for entry in fs::read_dir(self.data.join("apps"))? {
            let path = entry?.path();
            if path.extension().and_then(|v| v.to_str()) == Some("json") {
                apps.push(read_json(&path)?);
            }
        }
        apps.sort_by(|a: &App, b| a.name.cmp(&b.name));
        Ok(apps)
    }
    pub fn app(&self, id: &str) -> Result<App> {
        anyhow::ensure!(crate::valid_id(id), "Invalid app ID");
        let app: App = read_json(&self.data.join("apps").join(format!("{id}.json")))?;
        anyhow::ensure!(
            app.id == id && app.profile == crate::PROFILE,
            "Invalid app record"
        );
        Ok(app)
    }
    pub fn root(&self) -> Result<PathBuf> {
        let id = self
            .activation()?
            .current
            .context("Runtime is not installed")?;
        anyhow::ensure!(crate::valid_id(&id), "Invalid runtime ID");
        let user_image = self.data.join("images").join(&id);
        if user_image.is_dir() {
            return Ok(user_image);
        }
        if let Some((system_id, root)) = system_image()? {
            if id == system_id {
                return Ok(root);
            }
        }
        anyhow::bail!("Runtime image is unavailable; resume Provisioning to repair it")
    }
}

// The image is built on the cloud PC during provisioning, not downloaded from
// a separately signed No Land release. Apps still run with the desktop UID.
fn system_image() -> Result<Option<(String, PathBuf)>> {
    let path = Path::new("/etc/noland-runtime/image.json");
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            meta.uid() == 0 && meta.mode() & 0o022 == 0 && meta.is_file(),
            "Provisioned runtime configuration must be root-owned and not writable by applications"
        );
    }
    let value: serde_json::Value = read_json(path)?;
    anyhow::ensure!(
        value.get("profile").and_then(|v| v.as_str()) == Some(crate::PROFILE),
        "Unsupported provisioned runtime profile"
    );
    let id = value
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing runtime image ID")?;
    anyhow::ensure!(crate::valid_id(id), "Invalid provisioned runtime ID");
    let root = Path::new("/opt/noland/runtime/images").join(id);
    crate::artifact::validate_root(&root)?;
    Ok(Some((id.into(), root)))
}

pub fn private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;
    anyhow::ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "Private directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        anyhow::ensure!(
            fs::metadata(path)?.uid() == unsafe { libc::geteuid() },
            "Private directory has a different owner"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    Ok(options.open(path)?)
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut f = private_file(&temp)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?;
    f.sync_all()?;
    fs::rename(temp, path)?;
    File::open(path.parent().context("Missing parent")?)?.sync_all()?;
    Ok(())
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub fn read_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}
