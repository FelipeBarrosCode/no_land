use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::fs;

use crate::{
    errors::{AppError, AppResult},
    models::app_state::MoonlightPreferences,
};

#[cfg(target_os = "linux")]
use super::os_detection::OsDetection;

#[derive(Debug, Clone)]
pub struct MoonlightService;

impl MoonlightService {
    pub fn launch_native_client(&self) -> AppResult<()> {
        #[cfg(target_os = "macos")]
        {
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

            return Err(AppError::Command(
                "Moonlight app launch returned non-zero status".to_string(),
            ));
        }

        #[cfg(target_os = "windows")]
        {
            let status = Command::new("cmd")
                .arg("/C")
                .arg("start")
                .arg("")
                .arg("moonlight://")
                .status()
                .map_err(|error| {
                    AppError::Command(format!(
                        "Failed to launch Moonlight via protocol handler: {error}"
                    ))
                })?;

            if status.success() {
                return Ok(());
            }

            return Err(AppError::Command(
                "Moonlight protocol launch returned non-zero status".to_string(),
            ));
        }

        #[cfg(target_os = "linux")]
        {
            let primary = Command::new("moonlight").status();
            if let Ok(status) = primary {
                if status.success() {
                    return Ok(());
                }
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

async fn backup_file(path: &PathBuf) -> AppResult<()> {
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
    fs::write(backup_path, content).await?;
    Ok(())
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
