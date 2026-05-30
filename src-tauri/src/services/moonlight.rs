use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::env;

#[cfg(target_os = "linux")]
use std::ffi::OsString;

use serde::Serialize;
use tokio::fs;

use crate::{
    errors::{AppError, AppResult},
    models::app_state::MoonlightPreferences,
};

#[cfg(target_os = "linux")]
use crate::services::os_detection::OsDetection;

#[derive(Debug, Clone)]
pub struct MoonlightService;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightCodecSupport {
    pub h264: bool,
    pub hevc: bool,
    pub av1: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightAppliedSetting {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightConfigureResult {
    pub installed: bool,
    pub success: bool,
    pub platform: String,
    pub settings_location: Option<String>,
    pub backup_path: Option<String>,
    pub display_resolution: String,
    pub refresh_rate_hz: u32,
    pub network_type: String,
    pub codec_support: MoonlightCodecSupport,
    pub selected_settings: Vec<MoonlightAppliedSetting>,
    pub static_defaults: Vec<MoonlightAppliedSetting>,
    pub preserved_settings: Vec<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub can_launch_anyway: bool,
}

#[derive(Debug, Clone)]
pub enum MoonlightNetworkPreference {
    Auto,
    Lan,
    Wifi,
    Remote,
}

#[derive(Debug, Clone)]
pub enum MoonlightCodecPreference {
    Auto,
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone)]
pub struct MoonlightConfigureOptions {
    pub apply: bool,
    pub force_close: bool,
    pub native: bool,
    pub network: MoonlightNetworkPreference,
    pub prefer_codec: MoonlightCodecPreference,
    pub max_bitrate: Option<u32>,
    pub fps_override: Option<u32>,
    pub resolution_override: Option<(u32, u32)>,
    pub set_overrides: BTreeMap<String, String>,
}

impl Default for MoonlightConfigureOptions {
    fn default() -> Self {
        Self {
            apply: false,
            force_close: false,
            native: false,
            network: MoonlightNetworkPreference::Auto,
            prefer_codec: MoonlightCodecPreference::Auto,
            max_bitrate: None,
            fps_override: None,
            resolution_override: None,
            set_overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
enum MoonlightSettingsBackend {
    IniFile(PathBuf),
    #[cfg(target_os = "macos")]
    PlistFile(PathBuf),
    #[cfg(target_os = "windows")]
    Registry(String),
}

#[derive(Debug, Clone)]
struct DisplayDetection {
    width: u32,
    height: u32,
    refresh_rate_hz: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectedNetworkType {
    Lan,
    Wifi,
    Remote,
}

impl DetectedNetworkType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::Wifi => "wifi",
            Self::Remote => "remote",
        }
    }
}

impl MoonlightService {
    pub fn detected_executable_path(&self) -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            return find_macos_moonlight_executable();
        }

        #[cfg(target_os = "windows")]
        {
            return find_windows_moonlight_executable();
        }

        #[cfg(target_os = "linux")]
        {
            return find_linux_moonlight_command().map(|_| PathBuf::from("moonlight"));
        }

        #[allow(unreachable_code)]
        None
    }

    pub fn is_installed(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return find_macos_moonlight_app().is_some();
        }

        #[cfg(target_os = "windows")]
        {
            return self.detected_executable_path().is_some();
        }

        #[cfg(target_os = "linux")]
        {
            return find_linux_moonlight_command().is_some();
        }

        #[allow(unreachable_code)]
        false
    }

    pub async fn configure_client(
        &self,
        options: MoonlightConfigureOptions,
    ) -> MoonlightConfigureResult {
        let platform = current_platform_label().to_string();

        if !self.is_installed() {
            return MoonlightConfigureResult {
                installed: false,
                success: false,
                platform,
                settings_location: None,
                backup_path: None,
                display_resolution: "1920x1080".to_string(),
                refresh_rate_hz: 60,
                network_type: "wifi".to_string(),
                codec_support: MoonlightCodecSupport {
                    h264: true,
                    hevc: false,
                    av1: false,
                },
                selected_settings: Vec::new(),
                static_defaults: Vec::new(),
                preserved_settings: Vec::new(),
                warnings: vec![
                    "Moonlight is not installed. Configuration was skipped and the download page should be used instead.".to_string(),
                ],
                error: Some("Moonlight installation was not found on this machine.".to_string()),
                can_launch_anyway: false,
            };
        }

        let backend = match detect_settings_backend() {
            Ok(backend) => backend,
            Err(error) => {
                return MoonlightConfigureResult {
                    installed: true,
                    success: false,
                    platform,
                    settings_location: None,
                    backup_path: None,
                    display_resolution: "1920x1080".to_string(),
                    refresh_rate_hz: 60,
                    network_type: "wifi".to_string(),
                    codec_support: MoonlightCodecSupport {
                        h264: true,
                        hevc: false,
                        av1: false,
                    },
                    selected_settings: Vec::new(),
                    static_defaults: Vec::new(),
                    preserved_settings: Vec::new(),
                    warnings: Vec::new(),
                    error: Some(error.to_string()),
                    can_launch_anyway: true,
                };
            }
        };

        let settings_location = Some(backend.label());

        if let Err(error) = ensure_moonlight_not_running(options.force_close) {
            return MoonlightConfigureResult {
                installed: true,
                success: false,
                platform,
                settings_location,
                backup_path: None,
                display_resolution: "1920x1080".to_string(),
                refresh_rate_hz: 60,
                network_type: "wifi".to_string(),
                codec_support: MoonlightCodecSupport {
                    h264: true,
                    hevc: false,
                    av1: false,
                },
                selected_settings: Vec::new(),
                static_defaults: Vec::new(),
                preserved_settings: Vec::new(),
                warnings: Vec::new(),
                error: Some(error.to_string()),
                can_launch_anyway: true,
            };
        }

        let mut warnings = Vec::new();
        let display = detect_display(options.native).unwrap_or(DisplayDetection {
            width: 1920,
            height: 1080,
            refresh_rate_hz: 60,
        });
        let network_type = detect_network_type(&options).unwrap_or(DetectedNetworkType::Wifi);
        let codec_support = detect_codec_support();

        let existing = match read_backend_settings(&backend) {
            Ok(values) => values,
            Err(error) => {
                return MoonlightConfigureResult {
                    installed: true,
                    success: false,
                    platform,
                    settings_location,
                    backup_path: None,
                    display_resolution: format!("{}x{}", display.width, display.height),
                    refresh_rate_hz: display.refresh_rate_hz,
                    network_type: network_type.as_str().to_string(),
                    codec_support,
                    selected_settings: Vec::new(),
                    static_defaults: Vec::new(),
                    preserved_settings: Vec::new(),
                    warnings,
                    error: Some(error.to_string()),
                    can_launch_anyway: true,
                };
            }
        };

        let (dynamic_settings, static_defaults, preserved_settings) = build_setting_plan(
            &options,
            &existing,
            &display,
            network_type,
            &codec_support,
            &mut warnings,
        );

        let mut merged = existing.clone();
        for setting in dynamic_settings.iter().chain(static_defaults.iter()) {
            merged.insert(setting.key.clone(), setting.value.clone());
        }

        for (key, value) in &options.set_overrides {
            merged.insert(key.clone(), value.clone());
        }

        let backup_path = if options.apply {
            match backup_backend(&backend).await {
                Ok(path) => Some(path.display().to_string()),
                Err(error) => {
                    return MoonlightConfigureResult {
                        installed: true,
                        success: false,
                        platform,
                        settings_location,
                        backup_path: None,
                        display_resolution: format!("{}x{}", display.width, display.height),
                        refresh_rate_hz: display.refresh_rate_hz,
                        network_type: network_type.as_str().to_string(),
                        codec_support,
                        selected_settings: dynamic_settings,
                        static_defaults,
                        preserved_settings,
                        warnings,
                        error: Some(error.to_string()),
                        can_launch_anyway: true,
                    };
                }
            }
        } else {
            None
        };

        if options.apply {
            if let Err(error) = write_backend_settings(&backend, &existing, &merged).await {
                return MoonlightConfigureResult {
                    installed: true,
                    success: false,
                    platform,
                    settings_location,
                    backup_path,
                    display_resolution: format!("{}x{}", display.width, display.height),
                    refresh_rate_hz: display.refresh_rate_hz,
                    network_type: network_type.as_str().to_string(),
                    codec_support,
                    selected_settings: dynamic_settings,
                    static_defaults,
                    preserved_settings,
                    warnings,
                    error: Some(error.to_string()),
                    can_launch_anyway: true,
                };
            }
        }

        MoonlightConfigureResult {
            installed: true,
            success: true,
            platform,
            settings_location,
            backup_path,
            display_resolution: format!("{}x{}", display.width, display.height),
            refresh_rate_hz: display.refresh_rate_hz,
            network_type: network_type.as_str().to_string(),
            codec_support,
            selected_settings: dynamic_settings,
            static_defaults,
            preserved_settings,
            warnings,
            error: None,
            can_launch_anyway: true,
        }
    }

    pub async fn restore_backup(&self, backup_file: &str) -> AppResult<String> {
        let backend = detect_settings_backend()?;
        restore_backend(&backend, PathBuf::from(backup_file)).await?;
        Ok("Moonlight settings backup restored.".to_string())
    }

    pub fn launch_native_client(&self) -> AppResult<()> {
        #[cfg(target_os = "macos")]
        {
            if let Some(app_path) = find_macos_moonlight_app() {
                let status = Command::new("open")
                    .arg(app_path)
                    .status()
                    .map_err(|error| {
                        AppError::Command(format!(
                            "Failed to launch Moonlight from the detected macOS app path: {error}"
                        ))
                    })?;

                if status.success() {
                    return Ok(());
                }
            }

            let status = Command::new("open")
                .arg("-a")
                .arg("Moonlight")
                .status()
                .map_err(|error| {
                    AppError::Command(format!("Failed to launch Moonlight via open -a: {error}"))
                })?;

            if status.success() {
                return Ok(());
            }

            let fallback = Command::new("open")
                .arg("moonlight://")
                .status()
                .map_err(|error| {
                    AppError::Command(format!(
                        "Failed to launch Moonlight via macOS URL handler fallback: {error}"
                    ))
                })?;

            if fallback.success() {
                return Ok(());
            }

            return Err(AppError::Command(
                "Moonlight launch failed on macOS".to_string(),
            ));
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(executable) = find_windows_moonlight_executable() {
                Command::new(executable).spawn().map_err(|error| {
                    AppError::Command(format!(
                        "Failed to launch Moonlight from the detected Windows executable: {error}"
                    ))
                })?;
                return Ok(());
            }

            return Err(AppError::NotFound(
                "Moonlight desktop app not found. Install Moonlight from the official download and try again.".to_string(),
            ));
        }

        #[cfg(target_os = "linux")]
        {
            if let Some((command, args)) = find_linux_moonlight_command() {
                let mut child = Command::new(command);
                child.args(args);
                child.spawn().map_err(|error| {
                    AppError::Command(format!(
                        "Failed to launch Moonlight from the detected Linux command: {error}"
                    ))
                })?;
                return Ok(());
            }

            let os = OsDetection::new();
            if !os.command_exists("xdg-open") {
                return Err(AppError::Command(format!(
                    "Moonlight CLI is unavailable and Linux fallback launcher is missing. {}",
                    os.install_hint_for_tool("xdg-open")
                )));
            }

            let fallback = Command::new("xdg-open")
                .arg("moonlight://")
                .status()
                .map_err(|error| {
                    AppError::Command(format!("Failed to launch Moonlight via xdg-open: {error}"))
                })?;

            if fallback.success() {
                return Ok(());
            }

            return Err(AppError::Command(
                "Moonlight launch failed on Linux".to_string(),
            ));
        }

        #[allow(unreachable_code)]
        Err(AppError::Command(
            "Moonlight launch is not supported on this platform".to_string(),
        ))
    }

    pub fn pair_host(&self, host: &str) -> AppResult<()> {
        #[cfg(target_os = "macos")]
        {
            if let Some(executable) = find_macos_moonlight_executable() {
                Command::new(executable)
                    .arg("pair")
                    .arg(host)
                    .spawn()
                    .map_err(|error| {
                        AppError::Command(format!(
                            "Failed to start Moonlight pairing from the detected macOS executable: {error}"
                        ))
                    })?;
                return Ok(());
            }

            Command::new("moonlight")
                .arg("pair")
                .arg(host)
                .spawn()
                .map_err(|error| {
                    AppError::Command(format!(
                        "Failed to start Moonlight pairing via moonlight CLI: {error}"
                    ))
                })?;
            return Ok(());
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(executable) = find_windows_moonlight_executable() {
                Command::new(executable)
                    .arg("pair")
                    .arg(host)
                    .spawn()
                    .map_err(|error| {
                        AppError::Command(format!(
                            "Failed to start Moonlight pairing from the detected Windows executable: {error}"
                        ))
                    })?;
                return Ok(());
            }

            Command::new("moonlight")
                .arg("pair")
                .arg(host)
                .spawn()
                .map_err(|error| {
                    AppError::Command(format!(
                        "Failed to start Moonlight pairing via moonlight CLI: {error}"
                    ))
                })?;
            return Ok(());
        }

        #[cfg(target_os = "linux")]
        {
            Command::new("moonlight")
                .arg("pair")
                .arg(host)
                .spawn()
                .map_err(|error| {
                    AppError::Command(format!(
                        "Failed to start Moonlight pairing via moonlight CLI: {error}"
                    ))
                })?;
            return Ok(());
        }

        #[allow(unreachable_code)]
        Err(AppError::Command(
            "Moonlight pairing is not supported on this platform".to_string(),
        ))
    }

    pub async fn patch_local_config(
        &self,
        host_address: &str,
        host_port: u16,
        preferences: &MoonlightPreferences,
    ) -> AppResult<PathBuf> {
        let config_path = resolve_moonlight_config_path()?;
        let parent = config_path.parent().ok_or_else(|| {
            AppError::State("Moonlight config parent directory not found".to_string())
        })?;
        fs::create_dir_all(parent).await?;

        if config_path.exists() {
            backup_file(&config_path).await?;
        }

        let existing = if config_path.exists() {
            fs::read_to_string(&config_path).await?
        } else {
            String::new()
        };

        let mut updates = values_from_preferences(preferences);
        updates.insert("manualaddress".to_string(), host_address.to_string());
        updates.insert("manualport".to_string(), host_port.to_string());

        let mut applied = HashSet::new();
        let mut next_lines = Vec::new();
        for line in existing.lines() {
            let mut replaced = false;
            if let Some((key, _value)) = line.split_once('=') {
                let normalized_key = key.trim().to_ascii_lowercase();
                if let Some(new_value) = updates.get(&normalized_key) {
                    next_lines.push(format!("{normalized_key}={new_value}"));
                    applied.insert(normalized_key);
                    replaced = true;
                }
            }

            if !replaced {
                next_lines.push(line.to_string());
            }
        }

        for (key, value) in updates {
            if !applied.contains(&key) {
                next_lines.push(format!("{key}={value}"));
            }
        }

        let output = next_lines.join("\n") + "\n";
        fs::write(&config_path, output).await?;
        Ok(config_path)
    }
}

#[cfg(target_os = "macos")]
fn find_macos_moonlight_app() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("/Applications/Moonlight.app")];

    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Applications").join("Moonlight.app"));
    }

    candidates.into_iter().find(|path| path.exists())
}

#[cfg(target_os = "macos")]
fn find_macos_moonlight_executable() -> Option<PathBuf> {
    find_macos_moonlight_app().map(|app| app.join("Contents").join("MacOS").join("Moonlight"))
}

#[cfg(target_os = "windows")]
fn find_windows_moonlight_executable() -> Option<PathBuf> {
    if let Some(path) = resolve_windows_executable_from_path("Moonlight.exe") {
        return Some(path);
    }

    if let Some(path) = resolve_windows_app_path_registry("moonlight.exe") {
        return Some(path);
    }

    if let Some(path) = resolve_windows_uninstall_install_location() {
        return Some(path);
    }

    if let Some(path) = resolve_windows_public_desktop_moonlight() {
        return Some(path);
    }

    let mut candidates = Vec::new();

    for key in ["LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(base) = env::var_os(key) {
            let base = PathBuf::from(base);
            candidates.push(
                base.join("Moonlight Game Streaming Project")
                    .join("Moonlight.exe"),
            );
            candidates.push(
                base.join("Programs")
                    .join("Moonlight Game Streaming Project")
                    .join("Moonlight.exe"),
            );
            candidates.push(base.join("Moonlight").join("Moonlight.exe"));
        }
    }

    candidates.into_iter().find(|path| path.exists())
}

#[cfg(target_os = "windows")]
fn resolve_windows_executable_from_path(exe_name: &str) -> Option<PathBuf> {
    let output = Command::new("where").arg(exe_name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .find(|path| path.exists())
}

#[cfg(target_os = "windows")]
fn resolve_windows_app_path_registry(exe_name: &str) -> Option<PathBuf> {
    let key = format!(
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\App Paths\{}",
        exe_name
    );
    read_reg_default_value(&key).and_then(|value| {
        let path = PathBuf::from(value);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    })
}

#[cfg(target_os = "windows")]
fn resolve_windows_uninstall_install_location() -> Option<PathBuf> {
    let keys = [
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ];

    for key in keys {
        let output = Command::new("reg")
            .args(["query", key, "/s", "/f", "Moonlight", "/d"])
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }

        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("HKEY_") {
                if let Some(location) = read_reg_named_value(trimmed, "InstallLocation") {
                    let candidate = PathBuf::from(location).join("Moonlight.exe");
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
                if let Some(display_icon) = read_reg_named_value(trimmed, "DisplayIcon") {
                    let cleaned = display_icon.trim_matches('"').to_string();
                    let path = PathBuf::from(cleaned);
                    if path.exists() {
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn resolve_windows_public_desktop_moonlight() -> Option<PathBuf> {
    if let Ok(public_dir) = env::var("PUBLIC") {
        let desktop = PathBuf::from(public_dir).join("Desktop");
        let direct_exe = desktop.join("Moonlight.exe");
        if direct_exe.exists() {
            return Some(direct_exe);
        }

        let shortcut = desktop.join("Moonlight.lnk");
        if shortcut.exists() {
            if let Some(target) = resolve_windows_shortcut_target(&shortcut) {
                if target.exists() {
                    return Some(target);
                }
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn resolve_windows_shortcut_target(shortcut: &std::path::Path) -> Option<PathBuf> {
    let shortcut_text = shortcut.to_string_lossy().replace('"', "\"\"");
    let script = format!(
        "$w = New-Object -ComObject WScript.Shell; $s = $w.CreateShortcut(\"{}\"); [Console]::WriteLine($s.TargetPath)",
        shortcut_text
    );

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let target = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if target.is_empty() {
        return None;
    }

    Some(PathBuf::from(target))
}

#[cfg(target_os = "windows")]
fn read_reg_default_value(key: &str) -> Option<String> {
    let output = Command::new("reg")
        .args(["query", key, "/ve"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_reg_query_value(&String::from_utf8_lossy(&output.stdout), "(Default)")
}

#[cfg(target_os = "windows")]
fn read_reg_named_value(key: &str, value_name: &str) -> Option<String> {
    let output = Command::new("reg")
        .args(["query", key, "/v", value_name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_reg_query_value(&String::from_utf8_lossy(&output.stdout), value_name)
}

#[cfg(target_os = "windows")]
fn parse_reg_query_value(output: &str, value_name: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with(value_name) {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let first = parts.next()?;
        if first != value_name {
            continue;
        }
        let _value_type = parts.next()?;
        let remaining = parts.collect::<Vec<_>>().join(" ").trim().to_string();
        if remaining.is_empty() {
            continue;
        }
        return Some(remaining.trim_matches('"').to_string());
    }
    None
}

#[cfg(target_os = "linux")]
fn find_linux_moonlight_command() -> Option<(OsString, Vec<OsString>)> {
    if resolve_command_in_path("moonlight").is_some() {
        return Some((OsString::from("moonlight"), Vec::new()));
    }

    if resolve_command_in_path("flatpak").is_some() {
        return Some((
            OsString::from("flatpak"),
            vec![
                OsString::from("run"),
                OsString::from("com.moonlight_stream.Moonlight"),
            ],
        ));
    }

    if resolve_command_in_path("snap").is_some() {
        return Some((
            OsString::from("snap"),
            vec![OsString::from("run"), OsString::from("moonlight")],
        ));
    }

    None
}

#[cfg(target_os = "linux")]
fn resolve_command_in_path(command: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    env::split_paths(&path_var)
        .map(|dir| dir.join(command))
        .find(|candidate| candidate.exists())
}

fn current_platform_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

fn detect_settings_backend() -> AppResult<MoonlightSettingsBackend> {
    #[cfg(target_os = "macos")]
    {
        let conf = resolve_moonlight_config_path()?;
        if conf.exists() {
            return Ok(MoonlightSettingsBackend::IniFile(conf));
        }

        if let Some(home) = dirs::home_dir() {
            let plist = home
                .join("Library")
                .join("Preferences")
                .join("Moonlight Game Streaming Project.plist");
            if plist.exists() {
                return Ok(MoonlightSettingsBackend::PlistFile(plist));
            }
        }

        return Ok(MoonlightSettingsBackend::IniFile(conf));
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            let flatpak = home
                .join(".var")
                .join("app")
                .join("com.moonlight_stream.Moonlight")
                .join("config")
                .join("Moonlight Game Streaming Project")
                .join("Moonlight.conf");
            if flatpak.exists() {
                return Ok(MoonlightSettingsBackend::IniFile(flatpak));
            }
        }

        return Ok(MoonlightSettingsBackend::IniFile(
            resolve_moonlight_config_path()?,
        ));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(executable) = find_windows_moonlight_executable() {
            let portable_ini = executable.with_file_name("Moonlight.ini");
            if portable_ini.exists() {
                return Ok(MoonlightSettingsBackend::IniFile(portable_ini));
            }
        }

        let app_data = env::var("APPDATA")
            .map_err(|_| AppError::NotFound("APPDATA not available on this machine".to_string()))?;
        let conf = PathBuf::from(app_data)
            .join("Moonlight Game Streaming Project")
            .join("Moonlight.conf");
        if conf.exists() {
            return Ok(MoonlightSettingsBackend::IniFile(conf));
        }

        return Ok(MoonlightSettingsBackend::Registry(
            r"HKCU\Software\Moonlight Game Streaming Project".to_string(),
        ));
    }

    #[allow(unreachable_code)]
    Err(AppError::NotFound(
        "Moonlight settings store could not be resolved on this platform".to_string(),
    ))
}

impl MoonlightSettingsBackend {
    fn label(&self) -> String {
        match self {
            Self::IniFile(path) => path.display().to_string(),
            #[cfg(target_os = "macos")]
            Self::PlistFile(path) => path.display().to_string(),
            #[cfg(target_os = "windows")]
            Self::Registry(key) => key.clone(),
        }
    }
}

fn ensure_moonlight_not_running(force_close: bool) -> AppResult<()> {
    if !is_moonlight_running()? {
        return Ok(());
    }

    if !force_close {
        return Err(AppError::Command(
            "Moonlight is currently running. Close Moonlight and retry, or use --force-close."
                .to_string(),
        ));
    }

    force_close_moonlight()?;

    if is_moonlight_running()? {
        return Err(AppError::Command(
            "Moonlight is still running after force-close. Close it manually and retry."
                .to_string(),
        ));
    }

    Ok(())
}

fn is_moonlight_running() -> AppResult<bool> {
    #[cfg(target_os = "macos")]
    {
        return Ok(Command::new("pgrep")
            .arg("-x")
            .arg("Moonlight")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false));
    }

    #[cfg(target_os = "linux")]
    {
        return Ok(Command::new("pgrep")
            .arg("-fi")
            .arg("moonlight")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false));
    }

    #[cfg(target_os = "windows")]
    {
        let output = Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq Moonlight.exe"])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Ok(stdout.contains("Moonlight.exe"));
    }

    #[allow(unreachable_code)]
    Ok(false)
}

fn force_close_moonlight() -> AppResult<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("pkill").arg("-x").arg("Moonlight").status()?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("pkill").arg("-fi").arg("moonlight").status()?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("taskkill")
            .args(["/IM", "Moonlight.exe", "/F"])
            .status()?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Ok(())
}

fn detect_display(native: bool) -> AppResult<DisplayDetection> {
    let mut detection = detect_display_impl().unwrap_or(DisplayDetection {
        width: 1920,
        height: 1080,
        refresh_rate_hz: 60,
    });

    if !native && (detection.width > 2560 || detection.height > 1440) {
        detection.width = 2560;
        detection.height = 1440;
    }

    detection.refresh_rate_hz = normalize_fps(detection.refresh_rate_hz);
    Ok(detection)
}

fn detect_display_impl() -> Option<DisplayDetection> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("system_profiler")
            .arg("SPDisplaysDataType")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Resolution:") {
                let numbers = rest
                    .split(|ch: char| !ch.is_ascii_digit())
                    .filter_map(|part| part.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                if numbers.len() >= 2 {
                    return Some(DisplayDetection {
                        width: numbers[0],
                        height: numbers[1],
                        refresh_rate_hz: 60,
                    });
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let output = Command::new("xrandr").arg("--current").output().ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if !line.contains('*') {
                continue;
            }
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if let Some(mode) = parts.first() {
                if let Some((w, h)) = parse_resolution(mode) {
                    let refresh = parts
                        .iter()
                        .find_map(|part| part.trim_end_matches('*').parse::<f32>().ok())
                        .map(|value| value.round() as u32)
                        .unwrap_or(60);
                    return Some(DisplayDetection {
                        width: w,
                        height: h,
                        refresh_rate_hz: refresh,
                    });
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; $s=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; Write-Output \"$($s.Width)x$($s.Height)\"",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let first_line = text.lines().next()?.trim();
        if let Some((w, h)) = parse_resolution(first_line) {
            return Some(DisplayDetection {
                width: w,
                height: h,
                refresh_rate_hz: 60,
            });
        }
    }

    None
}

pub(crate) fn detect_client_display_for_provisioning() -> Option<(u32, u32, u32)> {
    let detected = detect_display_impl()?;
    Some((detected.width, detected.height, detected.refresh_rate_hz))
}

fn normalize_fps(value: u32) -> u32 {
    match value {
        1..=75 => 60,
        76..=105 => 90,
        106..=132 => 120,
        _ => 144,
    }
}

fn parse_resolution(input: &str) -> Option<(u32, u32)> {
    let (width, height) = input.split_once('x')?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
}

fn detect_network_type(options: &MoonlightConfigureOptions) -> Option<DetectedNetworkType> {
    match options.network {
        MoonlightNetworkPreference::Lan => return Some(DetectedNetworkType::Lan),
        MoonlightNetworkPreference::Wifi => return Some(DetectedNetworkType::Wifi),
        MoonlightNetworkPreference::Remote => return Some(DetectedNetworkType::Remote),
        MoonlightNetworkPreference::Auto => {}
    }

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("sh")
            .arg("-lc")
            .arg("networksetup -listallhardwareports 2>/dev/null")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        if text.contains("wi-fi") {
            return Some(DetectedNetworkType::Wifi);
        }
        if text.contains("ethernet") {
            return Some(DetectedNetworkType::Lan);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("nmcli")
            .args(["-t", "-f", "TYPE,STATE", "dev"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            if text.contains("wifi:connected") {
                return Some(DetectedNetworkType::Wifi);
            }
            if text.contains("ethernet:connected") {
                return Some(DetectedNetworkType::Lan);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-NetAdapter | Where-Object {$_.Status -eq 'Up'} | Select-Object -ExpandProperty NdisPhysicalMedium",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        if text.contains("wireless") {
            return Some(DetectedNetworkType::Wifi);
        }
        if text.contains("802.3") || text.contains("ethernet") {
            return Some(DetectedNetworkType::Lan);
        }
    }

    Some(DetectedNetworkType::Wifi)
}

fn detect_codec_support() -> MoonlightCodecSupport {
    #[cfg(target_os = "macos")]
    {
        return MoonlightCodecSupport {
            h264: true,
            hevc: cfg!(target_arch = "aarch64"),
            av1: false,
        };
    }

    #[cfg(target_os = "linux")]
    {
        let mut support = MoonlightCodecSupport {
            h264: true,
            hevc: false,
            av1: false,
        };

        if let Ok(output) = Command::new("sh")
            .arg("-lc")
            .arg("ffmpeg -decoders 2>/dev/null || true")
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            support.hevc = text.contains(" hevc ")
                || text.contains("hevc_cuvid")
                || text.contains("hevc_vaapi");
            support.av1 =
                text.contains(" av1 ") || text.contains("av1_cuvid") || text.contains("av1_vaapi");
        }

        return support;
    }

    #[cfg(target_os = "windows")]
    {
        return MoonlightCodecSupport {
            h264: true,
            hevc: true,
            av1: false,
        };
    }

    #[allow(unreachable_code)]
    MoonlightCodecSupport {
        h264: true,
        hevc: false,
        av1: false,
    }
}

fn read_backend_settings(
    backend: &MoonlightSettingsBackend,
) -> AppResult<BTreeMap<String, String>> {
    match backend {
        MoonlightSettingsBackend::IniFile(path) => {
            if !path.exists() {
                return Ok(BTreeMap::new());
            }
            let content = std::fs::read_to_string(path)?;
            Ok(parse_ini_like_settings(&content))
        }
        #[cfg(target_os = "macos")]
        MoonlightSettingsBackend::PlistFile(path) => read_plist_settings(path),
        #[cfg(target_os = "windows")]
        MoonlightSettingsBackend::Registry(key) => read_registry_settings(key),
    }
}

async fn backup_backend(backend: &MoonlightSettingsBackend) -> AppResult<PathBuf> {
    match backend {
        MoonlightSettingsBackend::IniFile(path) => backup_file(path).await,
        #[cfg(target_os = "macos")]
        MoonlightSettingsBackend::PlistFile(path) => backup_file(path).await,
        #[cfg(target_os = "windows")]
        MoonlightSettingsBackend::Registry(key) => backup_registry_backend(key),
    }
}

async fn write_backend_settings(
    backend: &MoonlightSettingsBackend,
    existing: &BTreeMap<String, String>,
    merged: &BTreeMap<String, String>,
) -> AppResult<()> {
    match backend {
        MoonlightSettingsBackend::IniFile(path) => write_ini_backend(path, existing, merged).await,
        #[cfg(target_os = "macos")]
        MoonlightSettingsBackend::PlistFile(path) => write_plist_backend(path, merged),
        #[cfg(target_os = "windows")]
        MoonlightSettingsBackend::Registry(key) => write_registry_backend(key, merged),
    }
}

async fn restore_backend(
    backend: &MoonlightSettingsBackend,
    backup_file: PathBuf,
) -> AppResult<()> {
    match backend {
        MoonlightSettingsBackend::IniFile(path) => {
            let content = fs::read(&backup_file).await?;
            fs::write(path, content).await?;
            Ok(())
        }
        #[cfg(target_os = "macos")]
        MoonlightSettingsBackend::PlistFile(path) => {
            let content = fs::read(&backup_file).await?;
            fs::write(path, content).await?;
            Ok(())
        }
        #[cfg(target_os = "windows")]
        MoonlightSettingsBackend::Registry(key) => restore_registry_backend(key, &backup_file),
    }
}

fn parse_ini_like_settings(content: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    values
}

fn build_setting_plan(
    options: &MoonlightConfigureOptions,
    existing: &BTreeMap<String, String>,
    display: &DisplayDetection,
    network_type: DetectedNetworkType,
    codec_support: &MoonlightCodecSupport,
    warnings: &mut Vec<String>,
) -> (
    Vec<MoonlightAppliedSetting>,
    Vec<MoonlightAppliedSetting>,
    Vec<String>,
) {
    let mut dynamic_settings = Vec::new();
    let mut static_defaults = Vec::new();
    let mut preserved_settings = Vec::new();

    let (width, height) = options
        .resolution_override
        .unwrap_or((display.width, display.height));
    let fps = options
        .fps_override
        .unwrap_or(display.refresh_rate_hz)
        .clamp(1, 240);
    let bitrate = choose_bitrate(
        width,
        height,
        fps,
        network_type,
        options.max_bitrate,
        warnings,
    );

    dynamic_settings.push(setting("width", width));
    dynamic_settings.push(setting("height", height));
    dynamic_settings.push(setting("fps", fps));
    dynamic_settings.push(setting("bitrate", bitrate));
    dynamic_settings.push(setting("hdr", 0));
    dynamic_settings.push(setting("yuv444", 0));

    let selected_videocfg =
        choose_videocfg(options, existing, codec_support, &mut preserved_settings);
    if let Some(value) = selected_videocfg {
        dynamic_settings.push(setting("videocfg", value));
    }

    let selected_videodec = choose_videodec(existing, &mut preserved_settings);
    if let Some(value) = selected_videodec {
        dynamic_settings.push(setting("videodec", value));
    }

    let preserve_only = [
        "windowmode",
        "uidisplaymode",
        "audiocfg",
        "defaultver",
        "richpresence",
    ];
    for key in preserve_only {
        if existing.contains_key(key) {
            preserved_settings.push(format!("{key} preserved because enum mapping is uncertain"));
        }
    }

    for (key, value) in [
        ("vsync", "1"),
        ("framepacing", "1"),
        ("autoadjustbitrate", "0"),
        ("unlockbitrate", "0"),
        ("connwarnings", "1"),
        ("detectnetblocking", "1"),
        ("showperfoverlay", "1"),
        ("keepawake", "1"),
        ("mdns", "1"),
        ("gameopts", "0"),
        ("quitappafter", "0"),
        ("hostaudio", "0"),
        ("muteonfocusloss", "0"),
        ("mouseacceleration", "0"),
        ("abstouchmode", "0"),
        ("swapmousebuttons", "0"),
        ("reversescroll", "0"),
        ("swapfacebuttons", "0"),
        ("gamepadmouse", "1"),
        ("backgroundgamepad", "0"),
        ("multicontroller", "1"),
        ("capturesyskeys", "0"),
        ("language", "auto"),
    ] {
        static_defaults.push(MoonlightAppliedSetting {
            key: key.to_string(),
            value: value.to_string(),
        });
    }

    (dynamic_settings, static_defaults, preserved_settings)
}

fn choose_bitrate(
    width: u32,
    height: u32,
    fps: u32,
    network_type: DetectedNetworkType,
    max_bitrate: Option<u32>,
    warnings: &mut Vec<String>,
) -> u32 {
    let megapixels = (width as f32 * height as f32) / 1_000_000.0;
    let mut bitrate = (megapixels * fps as f32 * 170.0).round() as u32;
    let pacing_safe_ceiling = match network_type {
        DetectedNetworkType::Lan if fps >= 120 => 45_000,
        DetectedNetworkType::Lan => 35_000,
        DetectedNetworkType::Wifi if fps >= 120 => 30_000,
        DetectedNetworkType::Wifi => 25_000,
        DetectedNetworkType::Remote if fps >= 120 => 22_000,
        DetectedNetworkType::Remote => 18_000,
    };

    bitrate = match network_type {
        DetectedNetworkType::Lan => bitrate,
        DetectedNetworkType::Wifi => ((bitrate as f32) * 0.7).round() as u32,
        DetectedNetworkType::Remote => ((bitrate as f32) * 0.5).round() as u32,
    };

    bitrate = bitrate.clamp(500, 150_000);
    if let Some(limit) = max_bitrate {
        bitrate = bitrate.min(limit);
    }

    if bitrate > pacing_safe_ceiling {
        warnings.push(
            format!(
                "Bitrate reduced to {} Kbps to leave pacing headroom and avoid bursty queue buildup at {}x{} {} FPS.",
                pacing_safe_ceiling, width, height, fps
            )
        );
        bitrate = pacing_safe_ceiling;
    }

    if bitrate > 150_000 {
        warnings.push(
            "Bitrate above 150000 Kbps requires unlockbitrate=true; clamped to safe maximum."
                .to_string(),
        );
    }

    bitrate
}

fn choose_videocfg(
    options: &MoonlightConfigureOptions,
    existing: &BTreeMap<String, String>,
    codec_support: &MoonlightCodecSupport,
    preserved_settings: &mut Vec<String>,
) -> Option<u8> {
    match options.prefer_codec {
        MoonlightCodecPreference::Av1 if codec_support.av1 => Some(3),
        MoonlightCodecPreference::Hevc if codec_support.hevc => Some(2),
        MoonlightCodecPreference::H264 => Some(1),
        MoonlightCodecPreference::Auto => {
            if codec_support.hevc {
                Some(2)
            } else {
                Some(1)
            }
        }
        _ => {
            if existing.contains_key("videocfg") {
                preserved_settings.push(
                    "videocfg preserved because the requested codec support could not be confirmed"
                        .to_string(),
                );
                None
            } else {
                Some(1)
            }
        }
    }
}

fn choose_videodec(
    existing: &BTreeMap<String, String>,
    preserved_settings: &mut Vec<String>,
) -> Option<u8> {
    if existing.contains_key("videodec") {
        return Some(1);
    }

    preserved_settings
        .push("videodec preserved because hardware decoder enum mapping is uncertain".to_string());
    None
}

fn setting(key: &str, value: impl ToString) -> MoonlightAppliedSetting {
    MoonlightAppliedSetting {
        key: key.to_string(),
        value: value.to_string(),
    }
}

#[cfg(target_os = "macos")]
fn read_plist_settings(path: &PathBuf) -> AppResult<BTreeMap<String, String>> {
    let output = Command::new("plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(AppError::Command(
            "Failed to read Moonlight plist settings".to_string(),
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let mut settings = BTreeMap::new();
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            settings.insert(key.to_ascii_lowercase(), json_value_to_string(value));
        }
    }
    Ok(settings)
}

#[cfg(target_os = "macos")]
fn write_plist_backend(path: &PathBuf, merged: &BTreeMap<String, String>) -> AppResult<()> {
    for (key, value) in merged {
        let mut command = Command::new("defaults");
        command.arg("write").arg(path).arg(key);
        if let Ok(number) = value.parse::<i64>() {
            command.arg("-int").arg(number.to_string());
        } else if value == "0" || value == "1" {
            command
                .arg("-bool")
                .arg(if value == "1" { "true" } else { "false" });
        } else {
            command.arg("-string").arg(value);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(AppError::Command(format!(
                "Failed to write plist key {key}"
            )));
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn read_registry_settings(key: &str) -> AppResult<BTreeMap<String, String>> {
    let output = Command::new("reg").args(["query", key]).output()?;
    if !output.status.success() {
        return Ok(BTreeMap::new());
    }
    let mut settings = BTreeMap::new();
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() >= 3 && parts[1].starts_with("REG_") {
            settings.insert(parts[0].to_ascii_lowercase(), parts[2..].join(" "));
        }
    }
    Ok(settings)
}

#[cfg(target_os = "windows")]
fn backup_registry_backend(key: &str) -> AppResult<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            AppError::State(format!("Clock failure while backing up registry: {error}"))
        })?
        .as_secs();
    let backup_path =
        std::env::temp_dir().join(format!("moonlight-registry-backup-{timestamp}.reg"));
    let status = Command::new("reg")
        .arg("export")
        .arg(key)
        .arg(&backup_path)
        .arg("/y")
        .status()?;
    if !status.success() {
        return Err(AppError::Command(
            "Failed to export Moonlight registry settings".to_string(),
        ));
    }
    Ok(backup_path)
}

#[cfg(target_os = "windows")]
fn write_registry_backend(key: &str, merged: &BTreeMap<String, String>) -> AppResult<()> {
    for (name, value) in merged {
        let status = Command::new("reg")
            .args(["add", key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"])
            .status()?;
        if !status.success() {
            return Err(AppError::Command(format!(
                "Failed to write registry key {name}"
            )));
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn restore_registry_backend(_key: &str, backup_file: &PathBuf) -> AppResult<()> {
    let status = Command::new("reg")
        .arg("import")
        .arg(backup_file)
        .status()?;
    if !status.success() {
        return Err(AppError::Command(
            "Failed to restore Moonlight registry backup".to_string(),
        ));
    }
    Ok(())
}

fn json_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(value) => {
            if *value {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

async fn write_ini_backend(
    path: &PathBuf,
    existing: &BTreeMap<String, String>,
    merged: &BTreeMap<String, String>,
) -> AppResult<()> {
    let parent = path.parent().ok_or_else(|| {
        AppError::State("Moonlight config parent directory not found".to_string())
    })?;
    fs::create_dir_all(parent).await?;

    let existing_content = if path.exists() {
        fs::read_to_string(path).await?
    } else {
        String::new()
    };

    let mut next_lines = Vec::new();
    let mut seen = HashSet::new();
    for line in existing_content.lines() {
        if let Some((key, _)) = line.split_once('=') {
            let normalized = key.trim().to_ascii_lowercase();
            if let Some(value) = merged.get(&normalized) {
                next_lines.push(format!("{normalized}={value}"));
                seen.insert(normalized);
                continue;
            }
        }
        next_lines.push(line.to_string());
    }

    for (key, value) in merged {
        if !seen.contains(key) && (!existing.contains_key(key) || merged.contains_key(key)) {
            next_lines.push(format!("{key}={value}"));
        }
    }

    fs::write(path, next_lines.join("\n") + "\n").await?;
    Ok(())
}

fn resolve_moonlight_config_path() -> AppResult<PathBuf> {
    if cfg!(target_os = "windows") {
        let app_data = std::env::var("APPDATA")
            .map_err(|_| AppError::NotFound("APPDATA not available on this machine".to_string()))?;
        return Ok(PathBuf::from(app_data)
            .join("Moonlight Game Streaming Project")
            .join("Moonlight.conf"));
    }

    if cfg!(target_os = "macos") {
        let home = dirs::home_dir()
            .ok_or_else(|| AppError::NotFound("Could not resolve home directory".to_string()))?;
        return Ok(home
            .join("Library")
            .join("Preferences")
            .join("Moonlight Game Streaming Project")
            .join("Moonlight.conf"));
    }

    let config_dir = dirs::config_dir()
        .ok_or_else(|| AppError::NotFound("Could not resolve config directory".to_string()))?;
    Ok(config_dir
        .join("Moonlight Game Streaming Project")
        .join("Moonlight.conf"))
}

async fn backup_file(path: &PathBuf) -> AppResult<PathBuf> {
    let content = fs::read(path).await?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            AppError::State(format!(
                "Clock failure while backing up moonlight file: {error}"
            ))
        })?
        .as_secs();

    let backup_path = path.with_extension(format!("conf.bak.{timestamp}"));
    fs::write(&backup_path, content).await?;
    Ok(backup_path)
}

fn values_from_preferences(preferences: &MoonlightPreferences) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("bitrate".to_string(), preferences.bitrate.to_string()),
        ("fps".to_string(), preferences.fps.to_string()),
        ("width".to_string(), preferences.width.to_string()),
        ("height".to_string(), preferences.height.to_string()),
        ("hostaudio".to_string(), preferences.hostaudio.to_string()),
        (
            "showperfoverlay".to_string(),
            preferences.showperfoverlay.to_string(),
        ),
        ("keepawake".to_string(), preferences.keepawake.to_string()),
        (
            "framepacing".to_string(),
            preferences.framepacing.to_string(),
        ),
        ("vsync".to_string(), preferences.vsync.to_string()),
        ("hdr".to_string(), preferences.hdr.to_string()),
        ("videocfg".to_string(), preferences.videocfg.to_string()),
        ("videodec".to_string(), preferences.videodec.to_string()),
        ("yuv444".to_string(), preferences.yuv444.to_string()),
        ("gameopts".to_string(), preferences.gameopts.to_string()),
        (
            "gamepadmouse".to_string(),
            preferences.gamepadmouse.to_string(),
        ),
        (
            "detectnetblocking".to_string(),
            preferences.detectnetblocking.to_string(),
        ),
    ])
}
