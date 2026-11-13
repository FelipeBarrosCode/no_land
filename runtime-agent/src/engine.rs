use crate::{
    artifact,
    session::{clean_command, Session},
    store::{atomic_json, private_dir, read_json, App, Progress, Store},
    PROFILE,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub installed: bool,
    pub profile: String,
    pub runtime_id: Option<String>,
    pub previous_id: Option<String>,
    pub compatibility: String,
    pub progress: Option<Progress>,
    pub apps: Vec<App>,
    pub checks: Vec<Check>,
}

pub fn status(store: &Store) -> Result<Status> {
    let activation = store.activation()?;
    Ok(Status {
        installed: activation.current.is_some(),
        profile: PROFILE.into(),
        runtime_id: activation.current,
        previous_id: activation.previous,
        compatibility: "unvalidated".into(),
        progress: if store.state.join("progress.json").exists() {
            Some(read_json(&store.state.join("progress.json"))?)
        } else {
            None
        },
        apps: store.apps()?,
        checks: Vec::new(),
    })
}

pub fn register(store: &Store, file: &Path) -> Result<App> {
    register_selected(store, file, false)
}

pub fn register_selected(store: &Store, file: &Path, make_executable: bool) -> Result<App> {
    let _lock = store.lock()?;
    let file = file.canonicalize().context("Application file not found")?;
    anyhow::ensure!(
        !file.to_string_lossy().contains(['\n', '\r', '\0']),
        "Application path contains control characters"
    );
    let mut header = [0; 16];
    fs::File::open(&file)?.read_exact(&mut header)?;
    anyhow::ensure!(
        &header[..4] == b"\x7fELF" && header[4] == 2 && header[5] == 1,
        "Select a 64-bit Linux ELF executable or AppImage, not a Windows/Steam game"
    );
    let mut machine = [0; 20];
    fs::File::open(&file)?.read_exact(&mut machine)?;
    anyhow::ensure!(
        machine[18..20] == [62, 0],
        "Only x86-64 applications are supported"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if make_executable {
            // Only after ELF/architecture validation and explicit user approval.
            let permissions = fs::metadata(&file)?.permissions().mode();
            if permissions & 0o100 == 0 {
                fs::set_permissions(&file, fs::Permissions::from_mode(permissions | 0o100))?;
            }
        }
        anyhow::ensure!(
            fs::metadata(&file)?.permissions().mode() & 0o111 != 0,
            "Mark the selected application executable first"
        );
    }
    let appimage = &header[8..11] == b"AI\x02";
    if let Some(app) = store.apps()?.into_iter().find(|a| a.file == file) {
        return Ok(app);
    }
    let app = App {
        id: uuid::Uuid::new_v4().to_string(),
        name: file
            .file_stem()
            .context("Missing application name")?
            .to_string_lossy()
            .into(),
        file,
        profile: PROFILE.into(),
        appimage,
    };
    atomic_json(
        &store.data.join("apps").join(format!("{}.json", app.id)),
        &app,
    )?;
    Ok(app)
}

struct Gpu {
    libraries: BTreeMap<String, PathBuf>,
    devices: Vec<PathBuf>,
    directory: PathBuf,
}

fn is_nvidia_runtime_library(name: &str) -> bool {
    // nvidia-settings' GTK2/GTK3 UI libraries are not graphics drivers. Pulling
    // them in drags the parent's obsolete GTK/PCRE ABI into a modern image.
    (name.starts_with("libnvidia-") && !name.starts_with("libnvidia-gtk"))
        || [
            "libGLX_nvidia.so",
            "libEGL_nvidia.so",
            "libGLESv1_CM_nvidia.so",
            "libGLESv2_nvidia.so",
            "libcuda.so",
            "libnvcuvid.so",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

impl Gpu {
    fn discover(session: &Session) -> Result<Self> {
        anyhow::ensure!(
            session.descriptor.driver_identity.is_some(),
            "NVIDIA driver identity unavailable; non-NVIDIA profiles are not yet supported"
        );
        let output = clean_command("ldconfig").arg("-p").output()?;
        anyhow::ensure!(
            output.status.success(),
            "Cannot inspect parent driver libraries"
        );
        let mut libraries = BTreeMap::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some((left, right)) = line.split_once(" => ") else {
                continue;
            };
            if !left.contains("x86-64") {
                continue;
            }
            let Some(name) = left.split_whitespace().next() else {
                continue;
            };
            if is_nvidia_runtime_library(name) {
                let path = PathBuf::from(right.trim()).canonicalize()?;
                anyhow::ensure!(path.is_file(), "Missing driver component");
                libraries.insert(name.to_string(), path);
            }
        }
        anyhow::ensure!(libraries.contains_key("libGLX_nvidia.so.0") && libraries.contains_key("libEGL_nvidia.so.0"), "Graphics driver injection is missing; request graphics/display capabilities when recreating the instance (do not replace its driver)");
        let mut devices = Vec::new();
        for entry in fs::read_dir("/dev")? {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with("nvidia")
                && (name[6..].chars().all(|c| c.is_ascii_digit())
                    || matches!(
                        &*name,
                        "nvidiactl" | "nvidia-modeset" | "nvidia-uvm" | "nvidia-uvm-tools"
                    ))
            {
                devices.push(path);
            }
        }
        if Path::new("/dev/dri").is_dir() {
            for entry in fs::read_dir("/dev/dri")? {
                let path = entry?.path();
                if path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with("renderD")
                {
                    devices.push(path);
                }
            }
        }
        anyhow::ensure!(
            !devices.is_empty(),
            "No NVIDIA graphics devices are available"
        );
        let directory = session.dir.join("driver");
        private_dir(&directory)?;
        for (name, path) in &libraries {
            // Machine-local bindings only; never put the parent's entire library tree in the guest.
            fs::copy(path, directory.join(name))?;
        }
        atomic_json(
            &directory.join("egl.json"),
            &driver_manifest(
                &[
                    "/usr/share/glvnd/egl_vendor.d/10_nvidia.json",
                    "/etc/glvnd/egl_vendor.d/10_nvidia.json",
                ],
                &libraries,
            )?,
        )?;
        atomic_json(
            &directory.join("vulkan.json"),
            &driver_manifest(
                &[
                    "/etc/vulkan/icd.d/nvidia_icd.json",
                    "/usr/share/vulkan/icd.d/nvidia_icd.json",
                ],
                &libraries,
            )?,
        )?;
        Ok(Self {
            libraries,
            devices,
            directory,
        })
    }
    fn validate_dependencies(&self, root: &Path) -> Result<()> {
        for path in self.libraries.values() {
            let output = clean_command("ldd").arg(path).output()?;
            anyhow::ensure!(
                output.status.success(),
                "Could not inspect NVIDIA dependency closure"
            );
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let Some((left, right)) = line.split_once("=>") else {
                    continue;
                };
                let name = left.trim();
                anyhow::ensure!(
                    !right.contains("not found"),
                    "Parent NVIDIA dependency is missing: {name}"
                );
                if !self.libraries.contains_key(name) {
                    anyhow::ensure!(["usr/lib/x86_64-linux-gnu", "lib/x86_64-linux-gnu"].iter().any(|dir| root.join(dir).join(name).exists()), "Runtime lacks NVIDIA dependency {name}; rebuild the image instead of importing the parent libc");
                }
            }
        }
        Ok(())
    }
}

fn driver_manifest(
    paths: &[&str],
    libraries: &BTreeMap<String, PathBuf>,
) -> Result<serde_json::Value> {
    let path = paths.iter().find(|p| Path::new(p).is_file()).context("NVIDIA vendor manifest is missing; request graphics/display driver injection when recreating the parent instance")?;
    let mut manifest: serde_json::Value = read_json(Path::new(path))?;
    let library = manifest
        .pointer("/ICD/library_path")
        .and_then(|v| v.as_str())
        .context("NVIDIA manifest has no library path")?;
    let basename = Path::new(library)
        .file_name()
        .and_then(|s| s.to_str())
        .context("Invalid NVIDIA manifest library path")?;
    let name = if libraries.contains_key(basename) {
        basename.to_string()
    } else {
        let canonical = Path::new(library)
            .canonicalize()
            .context("NVIDIA manifest references an unavailable absolute library")?;
        libraries
            .iter()
            .find(|(_, value)| **value == canonical)
            .map(|(key, _)| key.clone())
            .context("NVIDIA manifest library is outside the curated driver closure")?
    };
    manifest["ICD"]["library_path"] = format!("/opt/noland-driver/{name}").into();
    // Preserve the provider's file format/API version; never invent driver capabilities.
    Ok(manifest)
}

fn bind_input_devices(command: &mut Command, input: &Path, udev: &Path, sys: &Path) {
    // Bind the directory, not individual event/js nodes, so controllers added
    // after launch remain visible. Parent device permissions still apply.
    if input.is_dir() {
        command.arg("--dev-bind").arg(input).arg("/dev/input");
        // SDL/libudev use the parent's device properties to classify gamepads.
        if udev.is_dir() {
            command.arg("--ro-bind").arg(udev).arg("/run/udev");
        }
        // udev resolves /sys/class/input symlinks into /sys/devices and looks
        // up device numbers under /sys/dev/char. Keep that tree consistent.
        if sys.is_dir() {
            command.arg("--ro-bind").arg(sys).arg("/sys");
        }
    }
}

fn bind_gpu_devices(command: &mut Command, devices: &[PathBuf]) {
    // Direct access to every GPU node the parent grants us. No GPU scheduler,
    // VRAM quota, power/clock adjustment, or software-rendering layer is added.
    for device in devices {
        command.arg("--dev-bind").arg(device).arg(device);
    }
}

fn bwrap(
    store: &Store,
    session: &Session,
    gpu: &Gpu,
    data: &Path,
    app_directory: &Path,
) -> Result<Command> {
    let root = store.root()?;
    artifact::validate_root(&root)?;
    gpu.validate_dependencies(&root)?;
    anyhow::ensure!(
        Path::new(crate::XSOCKET).exists(),
        "Existing No Land X11 socket is missing"
    );
    let d = &session.descriptor;
    let run = session.dir.join("run");
    private_dir(&run)?;
    // getpwuid/getgrgid inside the userspace must describe the real desktop UID,
    // not the image's build-time account (or PRoot's fake root).
    let passwd = fs::read_to_string(root.join("etc/passwd"))?;
    let passwd = passwd
        .lines()
        .filter(|line| {
            let parts = line.split(':').collect::<Vec<_>>();
            parts.first() != Some(&d.user.as_str())
                && parts.get(2).and_then(|s| s.parse::<u32>().ok()) != Some(d.uid)
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        session.dir.join("passwd"),
        format!(
            "{passwd}\n{}:x:{}:{}:No Land:/home/runtime:/bin/sh\n",
            d.user, d.uid, d.gid
        ),
    )?;
    let group = fs::read_to_string(root.join("etc/group"))?;
    let group = group
        .lines()
        .filter(|line| {
            line.split(':').nth(2).and_then(|s| s.parse::<u32>().ok()) != Some(d.gid)
                && !line.starts_with(&format!("{}:", d.user))
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        session.dir.join("group"),
        format!("{group}\n{}:x:{}:\n", d.user, d.gid),
    )?;
    let mut c = clean_command("timeout");
    // Caller adjusts the timeout for supervised application launches.
    c.args([
        "--signal=TERM",
        "--kill-after=5",
        "30",
        "bwrap",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-uts",
        // A cgroup namespace changes visibility, not membership or capacity.
        // Keep the parent's CPU affinity/quotas; never add a runtime-only cap.
        "--unshare-cgroup-try",
        "--cap-drop",
        "ALL",
        "--uid",
        &d.uid.to_string(),
        "--gid",
        &d.gid.to_string(),
        "--ro-bind",
    ])
    .arg(&root)
    .arg("/")
    .args([
        "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--tmpfs", "/run", "--bind",
    ])
    .arg(&run)
    .arg("/run/noland")
    .args([
        "--dir",
        "/tmp/.X11-unix",
        "--ro-bind",
        crate::XSOCKET,
        crate::XSOCKET,
        "--ro-bind",
    ])
    .arg(&session.authority)
    .arg("/run/noland/Xauthority")
    .arg("--ro-bind")
    .arg(session.dir.join("passwd"))
    .arg("/etc/passwd")
    .arg("--ro-bind")
    .arg(session.dir.join("group"))
    .arg("/etc/group")
    .args(["--bind", "/dev/shm", "/dev/shm", "--bind"])
    .arg(data)
    .arg("/home/runtime")
    .arg("--ro-bind")
    .arg(app_directory)
    .arg("/app")
    .arg("--ro-bind")
    .arg(&gpu.directory)
    .arg("/opt/noland-driver");
    bind_gpu_devices(&mut c, &gpu.devices);
    bind_input_devices(
        &mut c,
        Path::new("/dev/input"),
        Path::new("/run/udev"),
        Path::new("/sys"),
    );
    for path in ["/etc/resolv.conf", "/etc/hosts", "/etc/localtime"] {
        if Path::new(path).is_file() {
            c.args(["--ro-bind", path, path]);
        }
    }
    if let Some(socket) = &d.pulse_socket {
        c.arg("--ro-bind").arg(socket).arg("/run/noland/pulse");
    }
    let cookie = d.home.join(".config/pulse/cookie");
    if cookie.is_file() {
        c.arg("--ro-bind")
            .arg(&cookie)
            .arg("/run/noland/pulse-cookie");
    }
    c.args([
        "--clearenv",
        "--setenv",
        "PATH",
        "/usr/local/bin:/usr/bin:/bin",
        "--setenv",
        "HOME",
        "/home/runtime",
        "--setenv",
        "USER",
        &d.user,
        "--setenv",
        "LOGNAME",
        &d.user,
        "--setenv",
        "LANG",
        "C.UTF-8",
        "--setenv",
        "DISPLAY",
        crate::DISPLAY,
        "--setenv",
        "XAUTHORITY",
        "/run/noland/Xauthority",
        "--setenv",
        "XDG_RUNTIME_DIR",
        "/run/noland",
        "--setenv",
        "XDG_CONFIG_HOME",
        "/home/runtime/.config",
        "--setenv",
        "XDG_DATA_HOME",
        "/home/runtime/.local/share",
        "--setenv",
        "XDG_CACHE_HOME",
        "/home/runtime/.cache",
        "--setenv",
        "LD_LIBRARY_PATH",
        "/opt/noland-driver",
        "--setenv",
        "__EGL_VENDOR_LIBRARY_FILENAMES",
        "/opt/noland-driver/egl.json",
        "--setenv",
        "VK_ICD_FILENAMES",
        "/opt/noland-driver/vulkan.json",
        "--setenv",
        "PULSE_SERVER",
        "unix:/run/noland/pulse",
        "--setenv",
        "PULSE_COOKIE",
        "/run/noland/pulse-cookie",
        "--chdir",
        "/app",
        "--",
        "/usr/bin/dbus-run-session",
        "--",
    ]);
    Ok(c)
}

fn check(name: &str, result: Result<String>) -> Check {
    match result {
        Ok(detail) => Check {
            name: name.into(),
            ok: true,
            detail,
        },
        Err(e) => Check {
            name: name.into(),
            ok: false,
            detail: e.to_string(),
        },
    }
}

fn cpu_access(parent_status: &str, runtime_status: &str) -> Result<String> {
    fn allowed(status: &str) -> Result<&str> {
        let list = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .map(str::trim)
            .filter(|list| !list.is_empty())
            .context("CPU affinity is unavailable")?;
        anyhow::ensure!(
            list.bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b',' | b'-')),
            "Invalid CPU affinity"
        );
        Ok(list)
    }
    let parent = allowed(parent_status)?;
    let runtime = allowed(runtime_status)?;
    anyhow::ensure!(
        parent == runtime,
        "Runtime CPU affinity ({runtime}) differs from the parent ({parent}); CPU access must not be narrowed by the runtime"
    );
    Ok(format!(
        "Runtime and parent allow the same CPUs: {parent}. Provider/parent quotas and scheduling still apply."
    ))
}

pub fn doctor(store: &Store) -> Result<Status> {
    let mut report = status(store)?;
    let probe = (|| -> Result<Vec<Check>> {
        let session = Session::discover(store)?;
        session.refresh_authority()?;
        let gpu = Gpu::discover(&session)?;
        let home = session.dir.join("probe-home");
        private_dir(&home)?;
        let mut checks = vec![check(
            "cpu-access",
            (|| -> Result<String> {
                let parent = fs::read_to_string("/proc/self/status")?;
                let output = bwrap(store, &session, &gpu, &home, &home)?
                    .args(["/bin/cat", "/proc/self/status"])
                    .output()?;
                anyhow::ensure!(
                    output.status.success(),
                    "Cannot inspect runtime CPU affinity"
                );
                cpu_access(&parent, &String::from_utf8(output.stdout)?)
            })(),
        )];
        for (name, args, expected) in [
            (
                "namespace-glibc",
                vec!["/usr/bin/getconf", "GNU_LIBC_VERSION"],
                "glibc 2.39",
            ),
            (
                "newer-glibc-symbol",
                vec!["/usr/local/bin/noland-glibc-probe"],
                "glibc-2.38-symbol-ok",
            ),
            ("x11", vec!["/usr/bin/xdpyinfo"], "name of display"),
            ("opengl", vec!["/usr/bin/glxinfo", "-B"], "NVIDIA"),
            ("vulkan", vec!["/usr/bin/vulkaninfo", "--summary"], "NVIDIA"),
            ("audio", vec!["/usr/bin/pactl", "info"], "Server Name"),
        ] {
            let result = (|| -> Result<String> {
                let output = bwrap(store, &session, &gpu, &home, &home)?
                    .args(&args)
                    .output()?;
                let text = String::from_utf8_lossy(&output.stdout);
                anyhow::ensure!(
                    output.status.success() && text.contains(expected),
                    "{name} failed (exit {:?}): {} {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                        .chars()
                        .take(2048)
                        .collect::<String>(),
                    text.chars().take(1024).collect::<String>()
                );
                if name == "opengl" || name == "vulkan" {
                    anyhow::ensure!(
                        !text.to_lowercase().contains("llvmpipe")
                            && !text.to_lowercase().contains("lavapipe"),
                        "Software renderer is not gaming-ready"
                    );
                }
                Ok(text.chars().take(4096).collect())
            })();
            checks.push(check(name, result));
        }
        // glxgears must actually draw a visible surface. A timeout is expected only
        // after it has emitted an FPS sample; a successful shell/NVML is insufficient.
        checks.push(check(
            "rendered-surface",
            (|| -> Result<String> {
                let output = bwrap(store, &session, &gpu, &home, &home)?
                    .args([
                        "/usr/bin/timeout",
                        "8",
                        "/usr/bin/stdbuf",
                        "-oL",
                        "/usr/bin/glxgears",
                        "-geometry",
                        "160x120",
                    ])
                    .output()?;
                let text = String::from_utf8_lossy(&output.stdout);
                anyhow::ensure!(
                    output.status.code() == Some(124)
                        && text.contains("frames in")
                        && text.contains("FPS"),
                    "Visible GPU surface did not render a frame sample (exit {:?}): {} {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                        .chars()
                        .take(2048)
                        .collect::<String>(),
                    text.chars().take(1024).collect::<String>()
                );
                Ok(text.chars().take(1024).collect())
            })(),
        ));
        Ok(checks)
    })();
    report.checks = match probe {
        Ok(checks) => checks,
        Err(e) => vec![check("session-backend", Err(e))],
    };
    report.compatibility = if report.checks.iter().all(|c| c.ok) {
        "validated-session"
    } else {
        "unsupported-on-this-instance"
    }
    .into();
    atomic_json(&store.state.join("diagnostics.json"), &report)?;
    Ok(report)
}

pub fn launch(store: &Store, id: &str) -> Result<serde_json::Value> {
    let app = store.app(id)?;
    let report = doctor(store)?;
    let failures = report
        .checks
        .iter()
        .filter(|check| !check.ok)
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::ensure!(failures.is_empty(), "Runtime checks failed:\n{failures}");
    // A dedicated worker owns session credentials and the child until it exits;
    // setsid in the CLI detaches this worker from SSH. No PID signaling API.
    let log_path = store
        .state
        .join(format!("launch-{}.log", uuid::Uuid::new_v4()));
    let log = crate::store::private_file(&log_path)?;
    let mut worker = Command::new(std::env::current_exe()?);
    worker
        .args(["supervise", "--app-id", &app.id])
        .env_clear()
        .env("HOME", std::env::var("HOME")?)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            worker.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = worker.spawn()?;
    let identity = process_start_identity(child.id());
    Ok(
        serde_json::json!({"pid":child.id(), "startIdentity":identity, "appId":app.id, "log":log_path, "status":"requested"}),
    )
}

pub fn process_start_identity(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(stat.rsplit_once(") ")?.1.split_whitespace().nth(19)?.into())
}

pub fn supervise(store: &Store, id: &str) -> Result<()> {
    let app = store.app(id)?;
    let session = Session::discover(store)?;
    session.refresh_authority()?;
    let gpu = Gpu::discover(&session)?;
    let data = store.data.join("app-data").join(id);
    private_dir(&data)?;
    let (app_directory, executable) = materialize_application(store, &app, &session, &gpu)?;
    let c = bwrap(store, &session, &gpu, &data, &app_directory)?;
    let mut args = c.get_args().map(|s| s.to_os_string()).collect::<Vec<_>>();
    let index = args
        .iter()
        .position(|a| a == "bwrap")
        .context("Missing backend")?;
    args.drain(..=index);
    let status = clean_command("bwrap").args(args).arg(executable).status()?;
    anyhow::ensure!(
        status.success(),
        "Application exited unsuccessfully ({status}); consult the launch log"
    );
    Ok(())
}

/// AppImage extraction still runs only as the approved desktop user, never in
/// the elevated package helper. Native ELF inspection does not execute the app.
pub(crate) fn dependency_files(store: &Store, app: &App) -> Result<(PathBuf, PathBuf)> {
    if !app.appimage {
        let directory = app
            .file
            .parent()
            .context("Missing application directory")?
            .to_owned();
        return Ok((directory, app.file.clone()));
    }
    let session = Session::discover(store)?;
    session.refresh_authority()?;
    let gpu = Gpu::discover(&session)?;
    let (directory, executable) = materialize_application(store, app, &session, &gpu)?;
    let relative = executable.strip_prefix("/app")?;
    Ok((directory.clone(), directory.join(relative)))
}

fn materialize_application(
    store: &Store,
    app: &App,
    session: &Session,
    gpu: &Gpu,
) -> Result<(PathBuf, PathBuf)> {
    let directory = app.file.parent().context("Missing application directory")?;
    let name = app.file.file_name().context("Missing application name")?;
    let mut executable = PathBuf::from("/app").join(name);
    let mut app_directory = directory.to_path_buf();
    if app.appimage {
        let digest = artifact::sha256(&app.file)?;
        let cache = store.data.join("cache").join(format!("appimage-{digest}"));
        let _lock = store.lock()?;
        if !cache.join("squashfs-root/AppRun").exists() {
            let staging = store
                .data
                .join("cache")
                .join(format!("extract-{}", uuid::Uuid::new_v4()));
            private_dir(&staging)?;
            let mut c = bwrap(store, session, gpu, &staging, directory)?;
            c.arg(executable).arg("--appimage-extract");
            // Extraction happens in the writable HOME, as the real desktop UID.
            let args = c.get_args().map(|s| s.to_os_string()).collect::<Vec<_>>();
            let mut replacement = clean_command("timeout");
            let mut args = args;
            for i in 0..args.len().saturating_sub(1) {
                if args[i] == "--chdir" {
                    args[i + 1] = "/home/runtime".into();
                }
            }
            let status = replacement
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() {
                let _ = fs::remove_dir_all(&staging);
                bail!("AppImage extraction failed (no FUSE or privileged extraction used)");
            }
            anyhow::ensure!(
                staging.join("squashfs-root/AppRun").is_file(),
                "AppImage did not extract AppRun"
            );
            if cache.exists() {
                fs::remove_dir_all(&cache)?;
            }
            fs::rename(staging, &cache)?;
        }
        app_directory = cache.join("squashfs-root");
        executable = "/app/AppRun".into();
    }
    Ok((app_directory, executable))
}

pub fn create_shortcut(store: &Store, id: Option<&str>) -> Result<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME")?);
    let apps = home.join(".local/share/applications");
    private_dir(&apps)?;
    let (name, filename, exec) = if let Some(id) = id {
        let app = store.app(id)?;
        (
            app.name.replace(['\n', '\r', '\\'], " "),
            format!("noland-runtime-{}.desktop", app.id),
            format!("/usr/local/bin/noland-runtime launch --app-id {}", app.id),
        )
    } else {
        (
            "No Land Runtime".into(),
            "noland-runtime.desktop".into(),
            "/usr/local/bin/noland-runtime gui".into(),
        )
    };
    let content = format!("[Desktop Entry]\nType=Application\nName={name}\nComment=Drop a trusted Linux app to run it in its own runtime\nExec={exec}\nIcon=applications-games\nTerminal=false\nCategories=Game;Utility;\nStartupNotify=true\n");
    let path = apps.join(&filename);
    fs::write(&path, &content)?;
    let output = clean_command("xdg-user-dir")
        .env("HOME", &home)
        .arg("DESKTOP")
        .output()
        .ok();
    let desktop = output
        .filter(|o| o.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
        .filter(|path| path.is_absolute() && path != &home)
        .unwrap_or_else(|| home.join("Desktop"));
    if desktop.is_absolute() && desktop != home {
        fs::create_dir_all(&desktop)?;
        let copy = desktop.join(filename);
        fs::write(&copy, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&copy, fs::Permissions::from_mode(0o700))?;
        }
        let _ = clean_command("gio")
            .env("HOME", &home)
            .args(["set"])
            .arg(&copy)
            .args(["metadata::trusted", "true"])
            .status();
    }
    Ok(path)
}

#[cfg(test)]
mod driver_tests {
    #[test]
    fn gpu_nodes_are_bound_directly_without_reducing_the_parent_device_set() {
        let devices = [
            "/dev/nvidia0",
            "/dev/nvidia1",
            "/dev/nvidiactl",
            "/dev/nvidia-modeset",
            "/dev/nvidia-uvm",
            "/dev/nvidia-uvm-tools",
            "/dev/dri/renderD128",
        ]
        .map(std::path::PathBuf::from);
        let mut command = std::process::Command::new("bwrap");
        super::bind_gpu_devices(&mut command, &devices);
        let expected = devices
            .iter()
            .flat_map(|device| {
                [
                    std::ffi::OsStr::new("--dev-bind"),
                    device.as_os_str(),
                    device.as_os_str(),
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn cpu_access_keeps_sparse_and_large_parent_allocations_intact() {
        for list in ["0", "0-14", "0-3,8,10-12", "0-255"] {
            let parent = format!("Name:\tlauncher\nCpus_allowed_list:\t{list}\n");
            let runtime = format!("Name:\tcat\nCpus_allowed_list:\t{list}\n");
            assert!(super::cpu_access(&parent, &runtime).unwrap().contains(list));
        }
    }

    #[test]
    fn cpu_access_reports_narrowed_missing_or_invalid_affinity() {
        let parent = "Cpus_allowed_list:\t0-14\n";
        let narrowed = "Cpus_allowed_list:\t0-3\n";
        assert!(super::cpu_access(parent, narrowed)
            .unwrap_err()
            .to_string()
            .contains("differs from the parent"));
        for invalid in ["", "Cpus_allowed_list:\n", "Cpus_allowed_list:\tunknown\n"] {
            assert!(super::cpu_access(parent, invalid).is_err());
            assert!(super::cpu_access(invalid, parent).is_err());
        }
    }

    #[test]
    fn input_devices_and_optional_udev_metadata_are_shared() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input");
        let udev = temp.path().join("udev");
        let sys = temp.path().join("sys");
        std::fs::create_dir(&input).unwrap();
        std::fs::create_dir(&udev).unwrap();
        std::fs::create_dir(&sys).unwrap();
        let mut command = std::process::Command::new("bwrap");
        super::bind_input_devices(&mut command, &input, &udev, &sys);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("--dev-bind"),
                input.as_os_str(),
                std::ffi::OsStr::new("/dev/input"),
                std::ffi::OsStr::new("--ro-bind"),
                udev.as_os_str(),
                std::ffi::OsStr::new("/run/udev"),
                std::ffi::OsStr::new("--ro-bind"),
                sys.as_os_str(),
                std::ffi::OsStr::new("/sys"),
            ]
        );
    }

    #[test]
    fn input_devices_work_without_udev_and_missing_input_is_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input");
        let udev = temp.path().join("missing-udev");
        let sys = temp.path().join("missing-sys");
        let mut command = std::process::Command::new("bwrap");
        super::bind_input_devices(&mut command, &input, &udev, &sys);
        assert_eq!(command.get_args().count(), 0);

        std::fs::create_dir(&input).unwrap();
        super::bind_input_devices(&mut command, &input, &udev, &sys);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("--dev-bind"),
                input.as_os_str(),
                std::ffi::OsStr::new("/dev/input"),
            ]
        );
    }

    #[test]
    fn graphics_dependency_closure_includes_cuda_and_video_driver_sonames() {
        for name in [
            "libnvidia-glcore.so.580.105.08",
            "libnvidia-egl-wayland.so.1",
            "libGLX_nvidia.so.0",
            "libEGL_nvidia.so.0",
            "libcuda.so.1",
            "libnvcuvid.so.1",
        ] {
            assert!(super::is_nvidia_runtime_library(name), "{name}");
        }
    }

    #[test]
    fn driver_bindings_exclude_nvidia_settings_gtk_and_parent_platform_libraries() {
        for name in [
            "libnvidia-gtk2.so.580.105.08",
            "libnvidia-gtk3.so.580.105.08",
            "libgtk-x11-2.0.so.0",
            "libpcre.so.3",
            "libc.so.6",
            "libGL.so.1",
        ] {
            assert!(!super::is_nvidia_runtime_library(name), "{name}");
        }
    }
}
