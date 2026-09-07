use std::{fs, path::Path, time::Duration};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::{
    models::app_state::PersistedAppState,
    services::{
        app_context::AppContext,
        moonlight::detect_client_display_for_provisioning,
        os_detection::{ArchKind, OsDetection},
        shared_storage::agent_runtime::locate_state_agent_source_dir,
        wireguard::{locate_noland_net_helper_binary, locate_wintun_library},
    },
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HealthProbeStatus {
    Ok,
    Warning,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthProbe {
    pub id: String,
    pub label: String,
    pub category: String,
    pub status: HealthProbeStatus,
    pub summary: String,
    pub details: Option<String>,
    pub fix_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemHealthReport {
    pub ok: bool,
    pub checked_at_unix: u64,
    pub os: String,
    pub arch: String,
    pub summary: String,
    pub probes: Vec<HealthProbe>,
}

impl HealthProbeStatus {
    fn blocks_provisioning(&self) -> bool {
        matches!(self, Self::Failed)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn probe(
    id: impl Into<String>,
    label: impl Into<String>,
    category: impl Into<String>,
    status: HealthProbeStatus,
    summary: impl Into<String>,
    details: Option<String>,
    fix_hint: Option<String>,
) -> HealthProbe {
    HealthProbe {
        id: id.into(),
        label: label.into(),
        category: category.into(),
        status,
        summary: summary.into(),
        details,
        fix_hint,
    }
}

fn ok_probe(
    id: impl Into<String>,
    label: impl Into<String>,
    category: impl Into<String>,
    summary: impl Into<String>,
    details: Option<String>,
) -> HealthProbe {
    probe(
        id,
        label,
        category,
        HealthProbeStatus::Ok,
        summary,
        details,
        None,
    )
}

fn warn_probe(
    id: impl Into<String>,
    label: impl Into<String>,
    category: impl Into<String>,
    summary: impl Into<String>,
    details: Option<String>,
    fix_hint: Option<String>,
) -> HealthProbe {
    probe(
        id,
        label,
        category,
        HealthProbeStatus::Warning,
        summary,
        details,
        fix_hint,
    )
}

fn failed_probe(
    id: impl Into<String>,
    label: impl Into<String>,
    category: impl Into<String>,
    summary: impl Into<String>,
    details: Option<String>,
    fix_hint: Option<String>,
) -> HealthProbe {
    probe(
        id,
        label,
        category,
        HealthProbeStatus::Failed,
        summary,
        details,
        fix_hint,
    )
}

fn arch_label(arch: ArchKind) -> &'static str {
    match arch {
        ArchKind::X64 => "x64",
        ArchKind::Arm64 => "arm64",
        ArchKind::Unknown => "unknown",
    }
}

fn linux_forbidden_runtime_lib(name: &str) -> bool {
    let forbidden_prefixes = [
        "libglib-2.0.so",
        "libgobject-2.0.so",
        "libgio-2.0.so",
        "libgmodule-2.0.so",
        "libgthread-2.0.so",
        "libatspi.so",
        "libatk-1.0.so",
        "libatk-bridge-2.0.so",
        "libgtk-3.so",
        "libgdk-3.so",
        "libwebkit2gtk",
        "libjavascriptcoregtk",
        "libpango",
        "libcairo",
        // Hardware/audio/driver-integration libraries must stay on the distro.
        "libudev.so",
        "libgudev",
        "libva",
        "libvdpau",
        "libdrm",
        "libgbm",
        "libGL.so",
        "libGLX",
        "libEGL",
        "libGLESv",
        "libOpenGL.so",
        "libglapi",
        "libopengl.so",
        "libpipewire",
        "libspa",
        "libpulse",
        "libasound",
        "libjack",
        "libxkbcommon-x11",
        "libxshmfence",
        "libcurl",
        "libnghttp2",
        "libgnutls",
        "libpsl",
        "libssh2",
    ];
    forbidden_prefixes
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn can_write_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    let test_path = path.join(format!(".noland-health-{}.tmp", now_unix()));
    fs::write(&test_path, b"ok").map_err(|error| error.to_string())?;
    fs::remove_file(&test_path).map_err(|error| error.to_string())?;
    Ok(())
}

fn state_probe(state: &PersistedAppState) -> HealthProbe {
    if state.version == 0 {
        return warn_probe(
            "state.schema",
            "state.json schema",
            "state",
            "State loaded, but schema version is unset.",
            Some("The reconciliation layer can continue, but a future save should rewrite the schema version.".to_string()),
            Some("Open settings or update any preference once to force a clean state save.".to_string()),
        );
    }

    ok_probe(
        "state.schema",
        "state.json schema",
        "state",
        format!("State loaded with schema version {}.", state.version),
        None,
    )
}

async fn network_probe(client: &reqwest::Client) -> HealthProbe {
    match client
        .get("https://api.github.com/")
        .header("User-Agent", "Noland-Connect-HealthCheck")
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() || response.status().is_redirection() => ok_probe(
            "network.github",
            "GitHub API reachability",
            "network",
            "GitHub API is reachable for update checks.",
            Some(format!("HTTP {}", response.status())),
        ),
        Ok(response) => warn_probe(
            "network.github",
            "GitHub API reachability",
            "network",
            "GitHub API responded, but not with a normal success code.",
            Some(format!("HTTP {}", response.status())),
            Some("Update checks may fail on restricted networks; the app can still provision if Vast and SSH are reachable.".to_string()),
        ),
        Err(error) => warn_probe(
            "network.github",
            "GitHub API reachability",
            "network",
            "GitHub API is not reachable from this machine.",
            Some(error.to_string()),
            Some("Check DNS, VPN, captive portal, firewall, or corporate network restrictions.".to_string()),
        ),
    }
}

async fn vast_probe(
    client: &reqwest::Client,
    state: &PersistedAppState,
    vast_base_url: &str,
) -> HealthProbe {
    if state.credentials.vast_api_key.trim().is_empty() {
        return failed_probe(
            "vast.credentials",
            "Vast.ai API key",
            "vast",
            "Vast API key is missing.",
            None,
            Some(
                "Add the Vast.ai API key in onboarding or settings before provisioning."
                    .to_string(),
            ),
        );
    }

    let url = format!(
        "{}/api/v0/users/current",
        vast_base_url.trim_end_matches('/')
    );
    match client
        .get(url)
        .bearer_auth(state.credentials.vast_api_key.trim())
        .timeout(Duration::from_secs(12))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => ok_probe(
            "vast.credentials",
            "Vast.ai API key",
            "vast",
            "Vast API key is present and accepted.",
            Some(format!("HTTP {}", response.status())),
        ),
        Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED || response.status() == reqwest::StatusCode::FORBIDDEN => failed_probe(
            "vast.credentials",
            "Vast.ai API key",
            "vast",
            "Vast API key was rejected.",
            Some(format!("HTTP {}", response.status())),
            Some("Update the Vast.ai API key in settings before provisioning.".to_string()),
        ),
        Ok(response) => warn_probe(
            "vast.credentials",
            "Vast.ai API key",
            "vast",
            "Vast API key check returned an unexpected status.",
            Some(format!("HTTP {}", response.status())),
            Some("Provisioning may still work if Vast has changed this endpoint; retry from another network if it fails.".to_string()),
        ),
        Err(error) => warn_probe(
            "vast.credentials",
            "Vast.ai API reachability",
            "vast",
            "Could not reach Vast.ai API for credential validation.",
            Some(error.to_string()),
            Some("Check internet, DNS, VPN/proxy, firewall, and captive portal restrictions.".to_string()),
        ),
    }
}

pub async fn run_system_health_report(app: &AppHandle, context: &AppContext) -> SystemHealthReport {
    let os = OsDetection::new();
    let arch = arch_label(os.arch()).to_string();
    let state = context.load_state().await;
    let mut probes = Vec::new();

    probes.push(ok_probe(
        "platform.os",
        "Operating system",
        "system",
        format!("Running on {} ({arch}).", os.platform_display_name()),
        Some(std::env::consts::OS.to_string()),
    ));

    match app.path().app_data_dir() {
        Ok(app_data_dir) => match can_write_directory(&app_data_dir) {
            Ok(()) => probes.push(ok_probe(
                "filesystem.app_data",
                "App data directory",
                "filesystem",
                "App data directory is writable.",
                Some(app_data_dir.display().to_string()),
            )),
            Err(error) => probes.push(failed_probe(
                "filesystem.app_data",
                "App data directory",
                "filesystem",
                "App cannot write to its data directory.",
                Some(format!("{}: {error}", app_data_dir.display())),
                Some(
                    "Fix filesystem permissions or reinstall the app for the current user."
                        .to_string(),
                ),
            )),
        },
        Err(error) => probes.push(failed_probe(
            "filesystem.app_data",
            "App data directory",
            "filesystem",
            "App data directory could not be resolved.",
            Some(error.to_string()),
            Some("Reinstall the app or check OS profile permissions.".to_string()),
        )),
    }

    probes.push(state_probe(&state));

    let managed_ssh =
        os.locate_app_managed_binary("ssh", "NOLAND_SSH_BIN", cfg!(target_os = "windows"));
    probes.push(match managed_ssh {
        Some(path) => ok_probe(
            "binary.ssh",
            "Managed SSH client",
            "bundled binaries",
            "Bundled SSH client found.",
            Some(path.display().to_string()),
        ),
        None => failed_probe(
            "binary.ssh",
            "Managed SSH client",
            "bundled binaries",
            "Bundled SSH client is missing.",
            None,
            Some(
                "Reinstall or rebuild Noland Connect so ssh is packaged in app resources."
                    .to_string(),
            ),
        ),
    });

    let managed_scp =
        os.locate_app_managed_binary("scp", "NOLAND_SCP_BIN", cfg!(target_os = "windows"));
    probes.push(match managed_scp {
        Some(path) => ok_probe(
            "binary.scp",
            "Managed SCP client",
            "bundled binaries",
            "Bundled SCP client found.",
            Some(path.display().to_string()),
        ),
        None => failed_probe(
            "binary.scp",
            "Managed SCP client",
            "bundled binaries",
            "Bundled SCP client is missing.",
            None,
            Some(
                "Reinstall or rebuild Noland Connect so scp is packaged in app resources."
                    .to_string(),
            ),
        ),
    });

    let managed_ssh_keygen = os.locate_app_managed_binary(
        "ssh-keygen",
        "NOLAND_SSH_KEYGEN_BIN",
        cfg!(target_os = "windows"),
    );
    probes.push(match managed_ssh_keygen {
        Some(path) => ok_probe(
            "binary.ssh_keygen",
            "Managed SSH key generator",
            "bundled binaries",
            "Bundled ssh-keygen found.",
            Some(path.display().to_string()),
        ),
        None => failed_probe(
            "binary.ssh_keygen",
            "Managed SSH key generator",
            "bundled binaries",
            "Bundled ssh-keygen is missing.",
            None,
            Some(
                "Reinstall or rebuild Noland Connect so ssh-keygen is packaged in app resources."
                    .to_string(),
            ),
        ),
    });

    probes.push(match locate_noland_net_helper_binary() {
        Some(path) => ok_probe(
            "binary.net_helper",
            "Managed tunnel helper",
            "wireguard",
            "noland-net-helper was found.",
            Some(path.display().to_string()),
        ),
        None => failed_probe(
            "binary.net_helper",
            "Managed tunnel helper",
            "wireguard",
            "noland-net-helper is missing.",
            None,
            Some("Reinstall or rebuild Noland Connect; the embedded tunnel cannot run without this sidecar.".to_string()),
        ),
    });

    probes.push(
        match crate::mic_client::runtime::resolve_mic_sender_binary() {
            Ok(path) => ok_probe(
                "binary.mic_sender",
                "Microphone sidecar",
                "audio",
                "noland-mic-sender was found.",
                Some(path.display().to_string()),
            ),
            Err(error) => warn_probe(
                "binary.mic_sender",
                "Microphone sidecar",
                "audio",
                "noland-mic-sender is missing or not executable.",
                Some(error.to_string()),
                Some("Microphone forwarding will be unavailable until the sidecar is packaged for this OS/arch.".to_string()),
            ),
        },
    );

    if os.is_linux() {
        match crate::mic_client::runtime::resolve_gstreamer_root_for_current_exe() {
            Some(root) => {
                let mut offenders = Vec::new();
                for lib_dir in [root.join("lib"), root.join("lib64")] {
                    if let Ok(entries) = fs::read_dir(&lib_dir) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if linux_forbidden_runtime_lib(&name) {
                                offenders.push(entry.path().display().to_string());
                            }
                        }
                    }
                }
                if offenders.is_empty() {
                    probes.push(ok_probe(
                        "runtime.gstreamer_linux",
                        "Linux GStreamer runtime isolation",
                        "audio",
                        "Bundled GStreamer runtime avoids GLib/GTK/AT-SPI libraries.",
                        Some(root.display().to_string()),
                    ));
                } else {
                    probes.push(failed_probe(
                        "runtime.gstreamer_linux",
                        "Linux GStreamer runtime isolation",
                        "audio",
                        "Bundled GStreamer runtime contains distro-owned desktop libraries.",
                        Some(offenders.join("\n")),
                        Some("Rebuild after cleaning src-tauri/.native-deps/*/gstreamer and src-tauri/binaries/gstreamer.".to_string()),
                    ));
                }
            }
            None => probes.push(warn_probe(
                "runtime.gstreamer_linux",
                "Linux GStreamer runtime isolation",
                "audio",
                "No bundled Linux GStreamer runtime was found.",
                None,
                Some("Microphone/embedded streaming may rely entirely on system GStreamer or be unavailable.".to_string()),
            )),
        }
    }

    if os.is_windows() {
        probes.push(match locate_wintun_library() {
            Some(path) => ok_probe("binary.wintun", "Windows Wintun driver library", "wireguard", "wintun.dll was found.", Some(path.display().to_string())),
            None => failed_probe("binary.wintun", "Windows Wintun driver library", "wireguard", "wintun.dll is missing.", None, Some("Reinstall or rebuild Noland Connect with wintun.dll bundled next to the network helper.".to_string())),
        });
    }

    match app.path().resource_dir() {
        Ok(resource_dir) => probes.push(ok_probe(
            "resources.dir",
            "Tauri resource directory",
            "resources",
            "Resource directory resolved.",
            Some(resource_dir.display().to_string()),
        )),
        Err(error) => probes.push(failed_probe(
            "resources.dir",
            "Tauri resource directory",
            "resources",
            "Resource directory could not be resolved.",
            Some(error.to_string()),
            Some("Reinstall the packaged app; bundled state-agent resources cannot be located without it.".to_string()),
        )),
    }

    probes.push(match locate_state_agent_source_dir() {
        Ok(path) => ok_probe("resources.state_agent", "Bundled state-agent source", "resources", "state-agent source tree was found.", Some(path.display().to_string())),
        Err(error) => failed_probe("resources.state_agent", "Bundled state-agent source", "resources", "state-agent source tree is missing.", Some(error.to_string()), Some("Reinstall or rebuild the app with state-agent resources included for this OS package.".to_string())),
    });

    if let Some((width, height, refresh)) = detect_client_display_for_provisioning() {
        probes.push(ok_probe(
            "display.detect",
            "Display detection",
            "moonlight",
            format!("Detected display profile {width}x{height}@{refresh}Hz."),
            None,
        ));
    } else {
        probes.push(warn_probe(
            "display.detect",
            "Display detection",
            "moonlight",
            "Could not detect a local display profile; fallback EDID will be used.",
            None,
            Some("This is usually okay, but streaming may default to 1920x1080@60.".to_string()),
        ));
    }

    if state.server_preferences.template_hash.trim().is_empty() {
        probes.push(failed_probe(
            "vast.template",
            "Vast template hash",
            "vast",
            "Server template hash is empty.",
            None,
            Some(
                "Set the Noland Vast template hash in server preferences before provisioning."
                    .to_string(),
            ),
        ));
    } else {
        probes.push(ok_probe(
            "vast.template",
            "Vast template hash",
            "vast",
            "Server template hash is configured.",
            Some(state.server_preferences.template_hash.clone()),
        ));
    }

    probes.push(network_probe(&context.http_client).await);
    probes.push(vast_probe(&context.http_client, &state, &context.config.vast_base_url).await);

    if state.wireguard.config_path.trim().is_empty() {
        probes.push(warn_probe(
            "wireguard.cached_config",
            "Cached WireGuard config",
            "wireguard",
            "No previous managed tunnel config is cached yet.",
            None,
            Some("This is normal before the first provisioning run.".to_string()),
        ));
    } else if Path::new(&state.wireguard.config_path).is_file() {
        probes.push(ok_probe(
            "wireguard.cached_config",
            "Cached WireGuard config",
            "wireguard",
            "Previous managed tunnel config exists on disk.",
            Some(state.wireguard.config_path.clone()),
        ));
    } else {
        probes.push(warn_probe(
            "wireguard.cached_config",
            "Cached WireGuard config",
            "wireguard",
            "State references a WireGuard config path that no longer exists.",
            Some(state.wireguard.config_path.clone()),
            Some("A new config will be generated during provisioning; if reconnecting an old instance fails, reprovision WireGuard.".to_string()),
        ));
    }

    let failed_count = probes
        .iter()
        .filter(|probe| matches!(probe.status, HealthProbeStatus::Failed))
        .count();
    let warning_count = probes
        .iter()
        .filter(|probe| matches!(probe.status, HealthProbeStatus::Warning))
        .count();
    let ok = !probes
        .iter()
        .any(|probe| probe.status.blocks_provisioning());
    let summary = if failed_count > 0 {
        format!("{failed_count} blocking issue(s), {warning_count} warning(s).")
    } else if warning_count > 0 {
        format!("Ready with {warning_count} warning(s).")
    } else {
        "All local health checks passed.".to_string()
    };

    SystemHealthReport {
        ok,
        checked_at_unix: now_unix(),
        os: os.platform_display_name().to_string(),
        arch,
        summary,
        probes,
    }
}
