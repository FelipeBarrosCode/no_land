use std::{env, process};

#[path = "../errors/mod.rs"]
mod errors;

#[path = "../models/app_state.rs"]
pub mod app_state_mod;

#[path = "../models/application_bundle.rs"]
pub mod application_bundle_mod;

#[path = "../services/os_detection.rs"]
pub mod os_detection_mod;

#[path = "../services/moonlight.rs"]
pub mod moonlight_mod;

#[path = "../utils/mod.rs"]
mod utils;

mod models {
    pub use crate::app_state_mod as app_state;
    pub use crate::application_bundle_mod as application_bundle;
}

mod services {
    pub use crate::moonlight_mod as moonlight;
    #[allow(unused_imports)]
    pub use crate::os_detection_mod as os_detection;
}

use crate::services::moonlight::MoonlightAppliedSetting;
use errors::AppError;
use services::moonlight::{
    MoonlightCodecPreference, MoonlightConfigureOptions, MoonlightNetworkPreference,
    MoonlightService,
};

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_relative_mouse(_delta_x: f64, _delta_y: f64) {}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_absolute_mouse(
    _x: f64,
    _y: f64,
    _content_width: f64,
    _content_height: f64,
) {
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_mouse_button(_button: u8, _pressed: bool) {}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_keyboard(
    _virtual_key: u16,
    _pressed: bool,
    _modifiers: u8,
) {
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_vertical_scroll(_amount: f64, _high_resolution: bool) {}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_horizontal_scroll(_amount: f64, _high_resolution: bool) {}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_focus_changed(_focused: bool) {}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_on_capture_changed(_active: bool, _mode: i32) {}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_request_capture() -> i32 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_event(_kind: i32) {}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_capture_active() -> bool {
    false
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_capture_mode() -> i32 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_capture_requests() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_mouse_moves() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_mouse_downs() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_mouse_ups() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_native_keys() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_rust_relative_callbacks() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_rust_absolute_callbacks() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_rust_button_callbacks() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_macos_input_debug_rust_key_callbacks() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_relative_send_attempts() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_absolute_send_attempts() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_button_send_attempts() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_key_send_attempts() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_scroll_send_attempts() -> u64 {
    0
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_send_errors() -> u64 {
    0
}

fn parse_args() -> Result<(MoonlightConfigureOptions, Option<String>, bool), AppError> {
    let mut options = MoonlightConfigureOptions::default();
    let mut args = env::args().skip(1).peekable();
    let mut restore_backup = None;
    let mut dry_run = true;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" => {
                dry_run = true;
                options.apply = false;
            }
            "--apply" => {
                dry_run = false;
                options.apply = true;
            }
            "--force-close" => options.force_close = true,
            "--native" => options.native = true,
            "--network" => {
                let value = args.next().ok_or_else(|| {
                    AppError::InvalidInput("--network requires a value".to_string())
                })?;
                options.network = match value.as_str() {
                    "lan" => MoonlightNetworkPreference::Lan,
                    "wifi" => MoonlightNetworkPreference::Wifi,
                    "remote" => MoonlightNetworkPreference::Remote,
                    "auto" => MoonlightNetworkPreference::Auto,
                    _ => {
                        return Err(AppError::InvalidInput(
                            "--network must be lan|wifi|remote|auto".to_string(),
                        ))
                    }
                };
            }
            "--prefer-codec" => {
                let value = args.next().ok_or_else(|| {
                    AppError::InvalidInput("--prefer-codec requires a value".to_string())
                })?;
                options.prefer_codec = match value.as_str() {
                    "auto" => MoonlightCodecPreference::Auto,
                    "h264" => MoonlightCodecPreference::H264,
                    "hevc" => MoonlightCodecPreference::Hevc,
                    "av1" => MoonlightCodecPreference::Av1,
                    _ => {
                        return Err(AppError::InvalidInput(
                            "--prefer-codec must be auto|h264|hevc|av1".to_string(),
                        ))
                    }
                };
            }
            "--max-bitrate" => {
                let value = args.next().ok_or_else(|| {
                    AppError::InvalidInput("--max-bitrate requires a value".to_string())
                })?;
                let parsed = value.parse::<u32>().map_err(|_| {
                    AppError::InvalidInput("--max-bitrate must be a number".to_string())
                })?;
                if !(500..=500_000).contains(&parsed) {
                    return Err(AppError::InvalidInput(
                        "bitrate must be between 500 and 500000 Kbps".to_string(),
                    ));
                }
                options.max_bitrate = Some(parsed);
            }
            "--fps" => {
                let value = args
                    .next()
                    .ok_or_else(|| AppError::InvalidInput("--fps requires a value".to_string()))?;
                let parsed = value
                    .parse::<u32>()
                    .map_err(|_| AppError::InvalidInput("--fps must be a number".to_string()))?;
                if !(1..=240).contains(&parsed) {
                    return Err(AppError::InvalidInput(
                        "fps must be between 1 and 240".to_string(),
                    ));
                }
                options.fps_override = Some(parsed);
            }
            "--resolution" => {
                let value = args.next().ok_or_else(|| {
                    AppError::InvalidInput("--resolution requires a value".to_string())
                })?;
                let (width, height) = value.split_once('x').ok_or_else(|| {
                    AppError::InvalidInput("resolution must use WIDTHxHEIGHT".to_string())
                })?;
                let width = width.parse::<u32>().map_err(|_| {
                    AppError::InvalidInput("resolution width must be numeric".to_string())
                })?;
                let height = height.parse::<u32>().map_err(|_| {
                    AppError::InvalidInput("resolution height must be numeric".to_string())
                })?;
                if !(256..=8192).contains(&width) || !(256..=8192).contains(&height) {
                    return Err(AppError::InvalidInput(
                        "width and height must be between 256 and 8192".to_string(),
                    ));
                }
                options.resolution_override = Some((width, height));
            }
            "--set" => {
                let value = args.next().ok_or_else(|| {
                    AppError::InvalidInput("--set requires key=value".to_string())
                })?;
                let (key, raw_value) = value.split_once('=').ok_or_else(|| {
                    AppError::InvalidInput("--set requires key=value".to_string())
                })?;
                options.set_overrides.insert(
                    key.trim().to_ascii_lowercase(),
                    raw_value.trim().to_string(),
                );
            }
            "--restore-backup" => {
                restore_backup = Some(args.next().ok_or_else(|| {
                    AppError::InvalidInput("--restore-backup requires a file path".to_string())
                })?);
            }
            other => {
                return Err(AppError::InvalidInput(format!("Unknown argument: {other}")));
            }
        }
    }

    Ok((options, restore_backup, dry_run))
}

fn print_settings(label: &str, settings: &[MoonlightAppliedSetting]) {
    println!("{label}:");
    for setting in settings {
        println!("  - {}={}", setting.key, setting.value);
    }
}

#[tokio::main]
async fn main() {
    let (options, restore_backup, dry_run) = match parse_args() {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("Error: {error}");
            process::exit(2);
        }
    };

    let moonlight = MoonlightService;

    if let Some(backup_file) = restore_backup {
        match moonlight.restore_backup(&backup_file).await {
            Ok(message) => {
                println!("{message}");
                println!("Done. Reopen Moonlight and verify the GUI.");
                return;
            }
            Err(error) => {
                eprintln!("Error: {error}");
                process::exit(1);
            }
        }
    }

    let result = moonlight.configure_client(options).await;

    println!("Detected platform: {}", result.platform);
    println!(
        "Detected Moonlight settings location: {}",
        result.settings_location.as_deref().unwrap_or("not found")
    );
    println!(
        "Backup file path: {}",
        result.backup_path.as_deref().unwrap_or(if dry_run {
            "dry-run: no backup created"
        } else {
            "not created"
        })
    );
    println!("Detected display resolution: {}", result.display_resolution);
    println!("Detected refresh rate: {} Hz", result.refresh_rate_hz);
    println!("Detected network type: {}", result.network_type);
    println!(
        "Detected codec support: H.264={} HEVC/H.265={} AV1={}",
        result.codec_support.h264, result.codec_support.hevc, result.codec_support.av1
    );
    print_settings("Selected dynamic settings", &result.selected_settings);
    print_settings("Static defaults applied", &result.static_defaults);

    if !result.preserved_settings.is_empty() {
        println!("Preserved settings:");
        for item in &result.preserved_settings {
            println!("  - {item}");
        }
    }

    if !result.warnings.is_empty() {
        println!("Warnings:");
        for warning in &result.warnings {
            println!("  - {warning}");
        }
    }

    if !result.success {
        eprintln!(
            "Error: {}",
            result
                .error
                .as_deref()
                .unwrap_or("Moonlight configuration failed.")
        );
        process::exit(1);
    }

    if dry_run {
        println!("Dry-run complete. Re-run with --apply to write changes.");
    }

    println!("Done. Reopen Moonlight and verify the GUI.");
}
