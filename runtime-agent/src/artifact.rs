use crate::{
    store::{atomic_json, Store},
    PROFILE,
};
use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, VerifyingKey};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Component, Path},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub profile: String,
    pub url: String,
    pub sha256: String,
    pub download_bytes: u64,
    pub extracted_bytes: u64,
}

pub fn verify(bytes: &[u8], signature: &[u8], public_key: &str) -> Result<Manifest> {
    anyhow::ensure!(bytes.len() <= 65536, "Manifest too large");
    let key: [u8; 32] = hex::decode(public_key.trim())
        .context("Invalid signing key")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Signing key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&key)?
        .verify_strict(bytes, &Signature::from_slice(signature)?)
        .context("Runtime manifest signature verification failed")?;
    let m: Manifest = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        m.schema == 1 && m.profile == PROFILE && crate::valid_id(&m.id),
        "Unsupported runtime manifest"
    );
    anyhow::ensure!(
        m.url.starts_with("https://") && !m.url.contains(['\n', '\r', '\0']),
        "Artifact URL must use HTTPS"
    );
    anyhow::ensure!(
        m.sha256.len() == 64 && hex::decode(&m.sha256)?.len() == 32,
        "Invalid artifact digest"
    );
    anyhow::ensure!(
        m.download_bytes > 0 && m.extracted_bytes > 0,
        "Missing measured artifact sizes"
    );
    Ok(m)
}

pub fn sha256(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub fn required_bytes(m: &Manifest) -> Result<u64> {
    m.download_bytes
        .checked_add(m.extracted_bytes)
        .and_then(|n| n.checked_add(m.extracted_bytes))
        .and_then(|n| n.checked_add(256 * 1024 * 1024))
        .context("Artifact size overflow")
}

pub fn install(store: &Store, m: &Manifest) -> Result<()> {
    let _lock = store.lock()?;
    let _ = fs::remove_file(store.state.join("cancel"));
    let result = install_inner(store, m);
    if let Err(e) = &result {
        store.progress(
            if store.state.join("cancel").exists() {
                "cancelled"
            } else {
                "failed"
            },
            "Runtime was not activated",
            Some(e.to_string()),
        )?;
    }
    result
}

fn install_inner(store: &Store, m: &Manifest) -> Result<()> {
    store.progress("checking", "Checking disk and existing installation", None)?;
    let mut activation = store.activation()?;
    if activation.current.as_deref() == Some(&m.id) {
        validate_root(&store.root()?)?;
        return store.progress("ready", "Runtime is already installed", None);
    }
    let existing = store.data.join("images").join(&m.id);
    if existing.exists() {
        let receipt: Manifest = crate::store::read_json(&existing.join("noland-manifest.json"))?;
        anyhow::ensure!(
            receipt.sha256 == m.sha256 && receipt.profile == m.profile,
            "Immutable runtime ID conflicts with existing image"
        );
        validate_root(&existing)?;
        activation.previous = activation.current.take();
        activation.current = Some(m.id.clone());
        atomic_json(&store.data.join("activation.json"), &activation)?;
        return store.progress("ready", "Existing verified image activated", None);
    }
    anyhow::ensure!(
        fs2::available_space(&store.data)? >= required_bytes(m)?,
        "Insufficient disk space for download, staging and rollback reserve"
    );
    let archive = store
        .data
        .join("cache")
        .join(format!("{}.tar.gz.part", m.id));
    store.progress(
        "downloading",
        "Downloading verified release (cancel/retry is resumable)",
        None,
    )?;
    if archive.metadata().map(|s| s.len()).unwrap_or(0) != m.download_bytes {
        let mut command = Command::new("curl");
        command
            .args([
                "--fail",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--connect-timeout",
                "20",
                "--max-time",
                "7200",
                "--max-filesize",
                &m.download_bytes.to_string(),
                "--continue-at",
                "-",
                "--output",
            ])
            .arg(&archive)
            .arg("--")
            .arg(&m.url)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_cancellable(store, command)?;
    }
    store.cancelled()?;
    store.progress("verifying", "Verifying release digest", None)?;
    if archive.metadata()?.len() != m.download_bytes || sha256(&archive)? != m.sha256.to_lowercase()
    {
        fs::remove_file(&archive)?;
        bail!("Artifact digest/size mismatch; corrupt download removed, retry required");
    }
    let staging = store
        .data
        .join("images")
        .join(format!("staging-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&staging)?;
    let extraction = (|| -> Result<()> {
        store.progress(
            "extracting",
            "Extracting into inactive staging directory",
            None,
        )?;
        extract(store, &archive, &staging, m.extracted_bytes)?;
        store.progress("validating", "Validating Ubuntu 24.04 image", None)?;
        validate_root(&staging)?;
        atomic_json(&staging.join("noland-manifest.json"), m)?;
        store.cancelled()?;
        let target = store.data.join("images").join(&m.id);
        anyhow::ensure!(
            !target.exists(),
            "Runtime ID already exists; immutable versions cannot be overwritten"
        );
        fs::rename(&staging, &target)?;
        activation.previous = activation.current.take();
        activation.current = Some(m.id.clone());
        atomic_json(&store.data.join("activation.json"), &activation)?;
        store.progress(
            "ready",
            "Image installed; run diagnostics before launching",
            None,
        )
    })();
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    extraction
}

pub fn validate_root(root: &Path) -> Result<()> {
    let os = fs::read_to_string(root.join("etc/os-release"))?;
    anyhow::ensure!(
        os.lines().any(|l| l == "ID=ubuntu") && os.lines().any(|l| l == "VERSION_ID=\"24.04\""),
        "Expected Ubuntu 24.04 image"
    );
    for path in [
        "usr/bin/env",
        "usr/bin/dbus-run-session",
        "usr/bin/glxinfo",
        "usr/bin/vulkaninfo",
        "usr/bin/glxgears",
        "usr/bin/pactl",
    ] {
        anyhow::ensure!(root.join(path).is_file(), "Image is missing {path}");
    }
    Ok(())
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn extract(store: &Store, archive: &Path, root: &Path, limit: u64) -> Result<()> {
    // Links are deferred until all regular files are unpacked. No archive entry can
    // traverse a previously-created symlink; absolute links are never accepted.
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    let mut links = Vec::new();
    let mut written = 0u64;
    for entry in archive.entries()? {
        store.cancelled()?;
        let mut e = entry?;
        let path = e.path()?.into_owned();
        anyhow::ensure!(safe_relative(&path), "Unsafe archive path");
        let kind = e.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            let link = e
                .link_name()?
                .context("Missing archive link target")?
                .into_owned();
            anyhow::ensure!(safe_relative(&link), "Unsafe archive link target");
            links.push((path, link, kind.is_hard_link()));
            continue;
        }
        anyhow::ensure!(
            kind.is_file() || kind.is_dir(),
            "Special archive files are forbidden"
        );
        written = written
            .checked_add(e.size())
            .context("Archive size overflow")?;
        anyhow::ensure!(written <= limit, "Archive exceeds signed extraction limit");
        e.set_preserve_permissions(false);
        anyhow::ensure!(e.unpack_in(root)?, "Archive path escaped destination");
    }
    for (path, link, hard) in links {
        store.cancelled()?;
        let destination = root.join(&path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
            anyhow::ensure!(
                parent.canonicalize()?.starts_with(root.canonicalize()?),
                "Link parent escaped destination"
            );
        }
        if hard {
            fs::hard_link(root.join(&link), &destination)?;
        } else {
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, destination)?;
            #[cfg(not(unix))]
            bail!("Image extraction requires Unix");
        }
    }
    Ok(())
}

pub fn run_cancellable(store: &Store, mut command: Command) -> Result<()> {
    let mut child = command.spawn()?;
    let started = Instant::now();
    loop {
        if store.cancelled().is_err() || started.elapsed() > Duration::from_secs(7200) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Operation cancelled or timed out");
        }
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(status.success(), "Download failed; retry to resume");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub fn rollback(store: &Store) -> Result<()> {
    let _lock = store.lock()?;
    let mut activation = store.activation()?;
    let previous = activation
        .previous
        .as_deref()
        .context("No previous runtime available")?;
    anyhow::ensure!(crate::valid_id(previous), "Invalid previous runtime ID");
    validate_root(&store.data.join("images").join(previous))?;
    std::mem::swap(&mut activation.current, &mut activation.previous);
    atomic_json(&store.data.join("activation.json"), &activation)?;
    store.progress(
        "ready",
        "Previous runtime activated; application data preserved",
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::io::Write;

    fn test_store() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store {
            data: temp.path().join("data"),
            state: temp.path().join("state"),
        };
        store.init().unwrap();
        (temp, store)
    }
    fn test_manifest() -> Manifest {
        Manifest {
            schema: 1,
            id: "test-v1".into(),
            profile: PROFILE.into(),
            url: "https://example.com/runtime.tar.gz".into(),
            sha256: "ab".repeat(32),
            download_bytes: 10,
            extracted_bytes: 1000,
        }
    }
    fn archive(path: &Path, entries: &[(&str, &[u8])]) {
        let gzip =
            flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gzip);
        for (name, bytes) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            builder.append_data(&mut h, name, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }
    fn valid_image(path: &Path) {
        archive(
            path,
            &[
                ("etc/os-release", b"ID=ubuntu\nVERSION_ID=\"24.04\"\n"),
                ("usr/bin/env", b"elf"),
                ("usr/bin/dbus-run-session", b"elf"),
                ("usr/bin/glxinfo", b"elf"),
                ("usr/bin/vulkaninfo", b"elf"),
                ("usr/bin/glxgears", b"elf"),
                ("usr/bin/pactl", b"elf"),
            ],
        );
    }

    #[test]
    fn rejects_traversal() {
        for p in ["/etc/passwd", "../x", "a/../../x"] {
            assert!(!safe_relative(Path::new(p)));
        }
        assert!(safe_relative(Path::new("usr/bin/env")));
    }
    #[test]
    fn disk_math_is_checked() {
        let m = Manifest {
            schema: 1,
            id: "v1".into(),
            profile: PROFILE.into(),
            url: "https://example.com/a".into(),
            sha256: "00".repeat(32),
            download_bytes: u64::MAX,
            extracted_bytes: 1,
        };
        assert!(required_bytes(&m).is_err());
    }
    #[test]
    fn rejects_unsigned_manifest() {
        assert!(verify(b"{}", &[0; 64], &"00".repeat(32)).is_err());
    }
    #[test]
    fn validates_signature_and_detects_tampering() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let public = hex::encode(key.verifying_key().as_bytes());
        let bytes = serde_json::to_vec(&test_manifest()).unwrap();
        let sig = key.sign(&bytes);
        assert_eq!(
            verify(&bytes, &sig.to_bytes(), &public).unwrap().id,
            "test-v1"
        );
        let mut tampered = bytes;
        tampered[0] ^= 1;
        assert!(verify(&tampered, &sig.to_bytes(), &public).is_err());
    }
    #[test]
    fn signed_wrong_profile_and_insecure_url_are_rejected() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let public = hex::encode(key.verifying_key().as_bytes());
        let mut m = test_manifest();
        m.profile = "ubuntu24-i386".into();
        let bytes = serde_json::to_vec(&m).unwrap();
        assert!(verify(&bytes, &key.sign(&bytes).to_bytes(), &public).is_err());
        m.profile = PROFILE.into();
        m.url = "http://example.com/a".into();
        let bytes = serde_json::to_vec(&m).unwrap();
        assert!(verify(&bytes, &key.sign(&bytes).to_bytes(), &public).is_err());
    }
    #[test]
    fn corrupt_download_keeps_healthy_activation() {
        let (_temp, store) = test_store();
        atomic_json(
            &store.data.join("activation.json"),
            &crate::store::Activation {
                current: Some("healthy".into()),
                previous: None,
            },
        )
        .unwrap();
        fs::write(store.data.join("cache/test-v1.tar.gz.part"), b"0123456789").unwrap();
        assert!(install(&store, &test_manifest()).is_err());
        assert_eq!(
            store.activation().unwrap().current.as_deref(),
            Some("healthy")
        );
        assert!(!store.data.join("images/test-v1").exists());
        assert!(!store.data.join("cache/test-v1.tar.gz.part").exists());
    }
    #[test]
    fn installation_activation_is_atomic_and_repeatable() {
        let (_temp, store) = test_store();
        let path = store.data.join("cache/test-v1.tar.gz.part");
        valid_image(&path);
        let mut m = test_manifest();
        m.download_bytes = path.metadata().unwrap().len();
        m.sha256 = sha256(&path).unwrap();
        install(&store, &m).unwrap();
        install(&store, &m).unwrap();
        assert_eq!(
            store.activation().unwrap().current.as_deref(),
            Some("test-v1")
        );
        assert!(store
            .data
            .join("images/test-v1/noland-manifest.json")
            .is_file());
    }
    #[test]
    fn extraction_limit_and_cancel_do_not_activate_image() {
        let (_temp, store) = test_store();
        let path = store.data.join("cache/test-v1.tar.gz.part");
        valid_image(&path);
        let target = store.data.join("images/staging-test");
        fs::create_dir(&target).unwrap();
        assert!(extract(&store, &path, &target, 1).is_err());
        fs::write(store.state.join("cancel"), b"cancel").unwrap();
        assert!(extract(&store, &path, &target, 1000).is_err());
        assert!(store.activation().unwrap().current.is_none());
    }
    #[test]
    fn rejects_archive_symlink_escape() {
        let (_temp, store) = test_store();
        let path = store.data.join("cache/escape.tar.gz");
        let gzip = flate2::write::GzEncoder::new(
            File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut builder = tar::Builder::new(gzip);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_link_name("../../outside").unwrap();
        h.set_cksum();
        builder
            .append_data(&mut h, "escape", std::io::empty())
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        let target = store.data.join("images/staging-test");
        fs::create_dir(&target).unwrap();
        assert!(extract(&store, &path, &target, 1000).is_err());
        assert!(!target.join("escape").exists());
    }
    #[test]
    fn install_lock_prevents_concurrent_activation() {
        let (_temp, store) = test_store();
        let _lock = store.lock().unwrap();
        assert!(store.lock().is_err());
    }
    #[test]
    fn rollback_preserves_app_data_and_running_image() {
        let (_temp, store) = test_store();
        for id in ["old", "new"] {
            let archive_path = store.data.join(format!("cache/{id}.tar.gz"));
            valid_image(&archive_path);
            let root = store.data.join("images").join(id);
            fs::create_dir(&root).unwrap();
            extract(&store, &archive_path, &root, 1000).unwrap();
        }
        let save = store.data.join("app-data/save");
        File::create(&save).unwrap().write_all(b"save").unwrap();
        atomic_json(
            &store.data.join("activation.json"),
            &crate::store::Activation {
                current: Some("new".into()),
                previous: Some("old".into()),
            },
        )
        .unwrap();
        rollback(&store).unwrap();
        assert_eq!(store.activation().unwrap().current.as_deref(), Some("old"));
        assert_eq!(fs::read(save).unwrap(), b"save");
        assert!(store.data.join("images/new").is_dir());
    }
}
