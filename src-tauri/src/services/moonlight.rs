use std::process::Command;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn noland_macos_detect_main_display(
        width: *mut u32,
        height: *mut u32,
        refresh_hz: *mut u32,
    ) -> i32;
}

#[derive(Debug, Clone)]
struct DisplayDetection {
    width: u32,
    height: u32,
    refresh_rate_hz: u32,
}

pub(crate) fn detect_client_display_for_provisioning() -> Option<(u32, u32, u32)> {
    let detected = detect_display()?;
    Some((detected.width, detected.height, detected.refresh_rate_hz))
}

pub(crate) fn detect_hardware_display_for_provisioning() -> Option<(u32, u32, u32)> {
    #[cfg(target_os = "macos")]
    {
        if let Some(output) = Command::new("system_profiler")
            .arg("SPDisplaysDataType")
            .output()
            .ok()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("Resolution:") {
                    let numbers = rest
                        .split(|ch: char| !ch.is_ascii_digit())
                        .filter_map(|part| part.parse::<u32>().ok())
                        .collect::<Vec<_>>();
                    if numbers.len() >= 2 {
                        return Some((numbers[0], numbers[1], 60));
                    }
                }
            }
        }
    }
    None
}

fn detect_display() -> Option<DisplayDetection> {
    #[cfg(target_os = "macos")]
    {
        let mut width = 0u32;
        let mut height = 0u32;
        let mut refresh_hz = 0u32;
        let detected =
            unsafe { noland_macos_detect_main_display(&mut width, &mut height, &mut refresh_hz) };
        if detected != 0 && width > 0 && height > 0 {
            return Some(DisplayDetection {
                width,
                height,
                refresh_rate_hz: refresh_hz.max(60),
            });
        }

        let output = Command::new("system_profiler")
            .arg("SPDisplaysDataType")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("Resolution:") {
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
        for line in text.lines().filter(|line| line.contains('*')) {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if let Some((width, height)) = parts.first().and_then(|mode| parse_resolution(mode)) {
                let refresh_rate_hz = parts
                    .iter()
                    .find_map(|part| part.trim_end_matches('*').parse::<f32>().ok())
                    .map(|value| value.round() as u32)
                    .unwrap_or(60);
                return Some(DisplayDetection {
                    width,
                    height,
                    refresh_rate_hz,
                });
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
        if let Some((width, height)) = text.lines().next().and_then(parse_resolution) {
            return Some(DisplayDetection {
                width,
                height,
                refresh_rate_hz: 60,
            });
        }
    }

    None
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn parse_resolution(input: &str) -> Option<(u32, u32)> {
    let (width, height) = input.trim().split_once('x')?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
}
