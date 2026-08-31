use std::{cmp::Ordering, error::Error, fmt};

use serde::{Deserialize, Serialize};

pub const MIN_DISPLAY_WIDTH: u32 = 320;
pub const MAX_DISPLAY_WIDTH: u32 = 4_095;
pub const MIN_DISPLAY_HEIGHT: u32 = 200;
pub const MAX_DISPLAY_HEIGHT: u32 = 4_095;
pub const MIN_REFRESH_MILLIHZ: u32 = 30_000;
pub const MAX_REFRESH_MILLIHZ: u32 = 240_000;
pub const STANDARD_FALLBACK_REFRESH_MILLIHZ: u32 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayModeSpec {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
}

impl DisplayModeSpec {
    pub const fn new(width: u32, height: u32, refresh_millihz: u32) -> Self {
        Self {
            width,
            height,
            refresh_millihz,
        }
    }

    pub const fn from_hz(width: u32, height: u32, refresh_hz: u32) -> Self {
        Self::new(width, height, hz_to_millihz(refresh_hz))
    }

    pub fn validate(&self) -> Result<(), DisplayModeValidationError> {
        validate_display_mode(self)
    }

    pub fn label(&self) -> String {
        format!(
            "{}x{}@{}",
            self.width,
            self.height,
            format_refresh_millihz(self.refresh_millihz)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DisplayProfileSource {
    AutoDetected,
    Manual,
    Fallback,
}

impl DisplayProfileSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::AutoDetected => "Auto-Detected",
            Self::Manual => "Manual",
            Self::Fallback => "Fallback",
        }
    }
}

impl fmt::Display for DisplayProfileSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayProfile {
    pub preferred_mode: DisplayModeSpec,
    pub advertised_modes: Vec<DisplayModeSpec>,
    pub source_label: String,
}

impl DisplayProfile {
    pub fn new(
        preferred_mode: DisplayModeSpec,
        source: DisplayProfileSource,
    ) -> Result<Self, DisplayModeValidationError> {
        preferred_mode.validate()?;

        Ok(Self {
            preferred_mode,
            advertised_modes: common_mode_catalog(preferred_mode),
            source_label: source.label().to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayModeValidationError {
    WidthOutOfRange { width: u32 },
    HeightOutOfRange { height: u32 },
    RefreshOutOfRange { refresh_millihz: u32 },
}

impl fmt::Display for DisplayModeValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WidthOutOfRange { width } => write!(
                formatter,
                "display width must be between {MIN_DISPLAY_WIDTH} and {MAX_DISPLAY_WIDTH} pixels (got {width})"
            ),
            Self::HeightOutOfRange { height } => write!(
                formatter,
                "display height must be between {MIN_DISPLAY_HEIGHT} and {MAX_DISPLAY_HEIGHT} pixels (got {height})"
            ),
            Self::RefreshOutOfRange { refresh_millihz } => write!(
                formatter,
                "display refresh rate must be between {} and {} (got {})",
                format_refresh_millihz(MIN_REFRESH_MILLIHZ),
                format_refresh_millihz(MAX_REFRESH_MILLIHZ),
                format_refresh_millihz(*refresh_millihz)
            ),
        }
    }
}

impl Error for DisplayModeValidationError {}

pub fn validate_display_mode(mode: &DisplayModeSpec) -> Result<(), DisplayModeValidationError> {
    if !(MIN_DISPLAY_WIDTH..=MAX_DISPLAY_WIDTH).contains(&mode.width) {
        return Err(DisplayModeValidationError::WidthOutOfRange { width: mode.width });
    }
    if !(MIN_DISPLAY_HEIGHT..=MAX_DISPLAY_HEIGHT).contains(&mode.height) {
        return Err(DisplayModeValidationError::HeightOutOfRange {
            height: mode.height,
        });
    }
    if !(MIN_REFRESH_MILLIHZ..=MAX_REFRESH_MILLIHZ).contains(&mode.refresh_millihz) {
        return Err(DisplayModeValidationError::RefreshOutOfRange {
            refresh_millihz: mode.refresh_millihz,
        });
    }

    Ok(())
}

pub const fn hz_to_millihz(refresh_hz: u32) -> u32 {
    refresh_hz.saturating_mul(1_000)
}

#[cfg(test)]
pub fn millihz_to_hz(refresh_millihz: u32) -> f64 {
    f64::from(refresh_millihz) / 1_000.0
}

pub fn format_refresh_millihz(refresh_millihz: u32) -> String {
    let whole_hz = refresh_millihz / 1_000;
    let fractional_millihz = refresh_millihz % 1_000;

    if fractional_millihz == 0 {
        return format!("{whole_hz} Hz");
    }

    let mut fraction = format!("{fractional_millihz:03}");
    while fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{whole_hz}.{fraction} Hz")
}

pub fn common_mode_catalog(preferred_mode: DisplayModeSpec) -> Vec<DisplayModeSpec> {
    let mut modes = vec![preferred_mode];
    let family = AspectFamily::for_dimensions(preferred_mode.width, preferred_mode.height);

    for &(width, height) in family.resolutions() {
        if width > preferred_mode.width || height > preferred_mode.height {
            continue;
        }

        // The preferred/native timing keeps its detected refresh. Compatibility
        // resolutions are advertised at 60 Hz so they fit standard EDID timing
        // slots and remain broadly usable by games and desktop environments.
        let mode = DisplayModeSpec::new(width, height, STANDARD_FALLBACK_REFRESH_MILLIHZ);
        if standard_timing_representable(mode) {
            modes.push(mode);
        }
    }

    // Always expose familiar 16:9 compatibility choices where they fit, even
    // when the client-native panel is 16:10 or ultrawide.
    for &(width, height) in &[(1_920, 1_080), (1_600, 900), (1_280, 720), (1_024, 768)] {
        if width <= preferred_mode.width && height <= preferred_mode.height {
            let mode = DisplayModeSpec::new(width, height, STANDARD_FALLBACK_REFRESH_MILLIHZ);
            if standard_timing_representable(mode) {
                modes.push(mode);
            }
        }
    }

    modes.sort_by(|left, right| compare_modes(*left, *right, preferred_mode));
    modes.dedup();
    let mut result = vec![preferred_mode];
    result.extend(
        modes
            .into_iter()
            .filter(|mode| *mode != preferred_mode)
            .take(8),
    );
    result
}

pub fn standard_timing_representable(mode: DisplayModeSpec) -> bool {
    if mode.width < 256 || mode.width > 2_288 || mode.width % 8 != 0 {
        return false;
    }
    if mode.refresh_millihz % 1_000 != 0 {
        return false;
    }
    let refresh_hz = mode.refresh_millihz / 1_000;
    if !(60..=123).contains(&refresh_hz) {
        return false;
    }
    mode.width.saturating_mul(10) == mode.height.saturating_mul(16)
        || mode.width.saturating_mul(3) == mode.height.saturating_mul(4)
        || mode.width.saturating_mul(4) == mode.height.saturating_mul(5)
        || mode.width.saturating_mul(9) == mode.height.saturating_mul(16)
}

pub fn build_display_profile(
    preferred_mode: DisplayModeSpec,
    source: DisplayProfileSource,
) -> Result<DisplayProfile, DisplayModeValidationError> {
    DisplayProfile::new(preferred_mode, source)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AspectFamily {
    Widescreen16By9,
    Widescreen16By10,
    Ultrawide,
    SuperUltrawide,
    Standard4By3,
}

impl AspectFamily {
    fn for_dimensions(width: u32, height: u32) -> Self {
        let ratio = f64::from(width) / f64::from(height.max(1));

        if ratio >= 3.0 {
            return Self::SuperUltrawide;
        }
        if ratio >= 2.0 {
            return Self::Ultrawide;
        }

        let candidates = [
            (Self::Widescreen16By9, 16.0 / 9.0),
            (Self::Widescreen16By10, 16.0 / 10.0),
            (Self::Standard4By3, 4.0 / 3.0),
        ];

        candidates
            .into_iter()
            .min_by(|(_, left_ratio), (_, right_ratio)| {
                (ratio - left_ratio)
                    .abs()
                    .partial_cmp(&(ratio - right_ratio).abs())
                    .unwrap_or(Ordering::Equal)
            })
            .map(|(family, _)| family)
            .unwrap_or(Self::Widescreen16By9)
    }

    const fn resolutions(self) -> &'static [(u32, u32)] {
        match self {
            Self::Widescreen16By9 => &[
                (7_680, 4_320),
                (3_840, 2_160),
                (2_560, 1_440),
                (1_920, 1_080),
                (1_600, 900),
                (1_366, 768),
                (1_280, 720),
                (854, 480),
                (640, 360),
            ],
            Self::Widescreen16By10 => &[
                (3_840, 2_400),
                (2_560, 1_600),
                (1_920, 1_200),
                (1_680, 1_050),
                (1_440, 900),
                (1_280, 800),
                (960, 600),
                (640, 400),
            ],
            Self::Ultrawide => &[
                (5_120, 2_160),
                (3_840, 1_600),
                (3_440, 1_440),
                (2_560, 1_080),
                (1_920, 800),
                (1_280, 540),
            ],
            Self::SuperUltrawide => &[
                (7_680, 2_160),
                (5_120, 1_440),
                (3_840, 1_080),
                (2_560, 720),
                (1_920, 540),
            ],
            Self::Standard4By3 => &[
                (4_096, 3_072),
                (2_048, 1_536),
                (1_600, 1_200),
                (1_280, 960),
                (1_024, 768),
                (800, 600),
                (640, 480),
                (320, 240),
            ],
        }
    }
}

fn compare_modes(
    left: DisplayModeSpec,
    right: DisplayModeSpec,
    preferred: DisplayModeSpec,
) -> Ordering {
    match (left == preferred, right == preferred) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }

    let left_pixels = u64::from(left.width) * u64::from(left.height);
    let right_pixels = u64::from(right.width) * u64::from(right.height);

    right_pixels
        .cmp(&left_pixels)
        .then_with(|| right.width.cmp(&left.width))
        .then_with(|| right.height.cmp(&left.height))
        .then_with(|| right.refresh_millihz.cmp(&left.refresh_millihz))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, refresh_millihz: u32) -> DisplayModeSpec {
        DisplayModeSpec::new(width, height, refresh_millihz)
    }

    #[test]
    fn mode_spec_uses_camel_case_serde_fields() {
        let value = serde_json::to_value(mode(1_920, 1_080, 59_940)).unwrap();

        assert_eq!(value["width"], 1_920);
        assert_eq!(value["height"], 1_080);
        assert_eq!(value["refreshMillihz"], 59_940);
        assert!(value.get("refresh_millihz").is_none());

        let decoded: DisplayModeSpec = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, mode(1_920, 1_080, 59_940));
    }

    #[test]
    fn profile_and_source_use_camel_case_serde() {
        let profile = DisplayProfile::new(
            mode(1_920, 1_080, 60_000),
            DisplayProfileSource::AutoDetected,
        )
        .unwrap();
        let value = serde_json::to_value(profile).unwrap();

        assert!(value.get("preferredMode").is_some());
        assert!(value.get("advertisedModes").is_some());
        assert_eq!(value["sourceLabel"], "Auto-Detected");
        assert_eq!(
            serde_json::to_value(DisplayProfileSource::AutoDetected).unwrap(),
            "autoDetected"
        );
    }

    #[test]
    fn source_labels_are_stable() {
        assert_eq!(DisplayProfileSource::AutoDetected.label(), "Auto-Detected");
        assert_eq!(DisplayProfileSource::Manual.label(), "Manual");
        assert_eq!(DisplayProfileSource::Fallback.label(), "Fallback");
        assert_eq!(DisplayProfileSource::Fallback.to_string(), "Fallback");
    }

    #[test]
    fn converts_and_formats_refresh_rates() {
        assert_eq!(hz_to_millihz(60), 60_000);
        assert_eq!(millihz_to_hz(59_940), 59.94);
        assert_eq!(format_refresh_millihz(60_000), "60 Hz");
        assert_eq!(format_refresh_millihz(59_940), "59.94 Hz");
        assert_eq!(format_refresh_millihz(119_880), "119.88 Hz");
        assert_eq!(format_refresh_millihz(60_001), "60.001 Hz");
        assert_eq!(mode(2_560, 1_440, 59_940).label(), "2560x1440@59.94 Hz");
    }

    #[test]
    fn validates_supported_boundaries() {
        assert!(
            mode(MIN_DISPLAY_WIDTH, MIN_DISPLAY_HEIGHT, MIN_REFRESH_MILLIHZ)
                .validate()
                .is_ok()
        );
        assert!(
            mode(MAX_DISPLAY_WIDTH, MAX_DISPLAY_HEIGHT, MAX_REFRESH_MILLIHZ)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn rejects_each_out_of_range_component() {
        assert_eq!(
            mode(MIN_DISPLAY_WIDTH - 1, 1_080, 60_000).validate(),
            Err(DisplayModeValidationError::WidthOutOfRange {
                width: MIN_DISPLAY_WIDTH - 1
            })
        );
        assert_eq!(
            mode(1_920, MAX_DISPLAY_HEIGHT + 1, 60_000).validate(),
            Err(DisplayModeValidationError::HeightOutOfRange {
                height: MAX_DISPLAY_HEIGHT + 1
            })
        );
        assert_eq!(
            mode(1_920, 1_080, MIN_REFRESH_MILLIHZ - 1).validate(),
            Err(DisplayModeValidationError::RefreshOutOfRange {
                refresh_millihz: MIN_REFRESH_MILLIHZ - 1
            })
        );
    }

    #[test]
    fn sixteen_by_nine_catalog_is_preferred_first_deduplicated_and_bounded() {
        let preferred = mode(3_840, 2_160, 120_000);
        let catalog = common_mode_catalog(preferred);

        assert_eq!(catalog.first(), Some(&preferred));
        assert!(!catalog.contains(&mode(3_840, 2_160, 60_000)));
        assert!(!catalog.contains(&mode(2_560, 1_440, 60_000)));
        assert!(catalog.contains(&mode(1_920, 1_080, 60_000)));
        assert!(!catalog
            .iter()
            .any(|entry| { entry.width > preferred.width || entry.height > preferred.height }));

        let mut unique = catalog.clone();
        unique.sort_by_key(|entry| (entry.width, entry.height, entry.refresh_millihz));
        unique.dedup();
        assert_eq!(unique.len(), catalog.len());
    }

    #[test]
    fn standard_preferred_mode_is_not_duplicated() {
        let preferred = mode(1_920, 1_080, 60_000);
        let catalog = common_mode_catalog(preferred);

        assert_eq!(catalog.first(), Some(&preferred));
        assert_eq!(
            catalog.iter().filter(|entry| **entry == preferred).count(),
            1
        );
    }

    #[test]
    fn sixteen_by_ten_catalog_uses_matching_fallbacks() {
        let catalog = common_mode_catalog(mode(2_560, 1_600, 60_000));

        assert!(catalog.contains(&mode(1_920, 1_200, 60_000)));
        assert!(catalog.contains(&mode(1_280, 800, 60_000)));
        assert!(catalog.contains(&mode(1_920, 1_080, 60_000)));
    }

    #[test]
    fn ultrawide_catalog_has_ultrawide_fallbacks() {
        let catalog = common_mode_catalog(mode(3_440, 1_440, 100_000));

        assert!(!catalog.contains(&mode(3_440, 1_440, 60_000)));
        assert!(catalog.contains(&mode(1_920, 1_080, 60_000)));
        assert!(!catalog.contains(&mode(1_280, 540, 60_000)));
        assert!(!catalog.contains(&mode(3_840, 1_600, 60_000)));
    }

    #[test]
    fn super_ultrawide_catalog_does_not_mix_in_regular_ultrawide_modes() {
        let catalog = common_mode_catalog(mode(5_120, 1_440, 60_000));

        assert!(!catalog.contains(&mode(3_840, 1_080, 60_000)));
        assert!(!catalog.contains(&mode(2_560, 720, 60_000)));
        assert!(catalog.contains(&mode(1_920, 1_080, 60_000)));
        assert!(!catalog.contains(&mode(3_440, 1_440, 60_000)));
    }

    #[test]
    fn nonstandard_native_mode_is_preserved_exactly() {
        let preferred = mode(2_880, 1_800, 90_000);
        let catalog = common_mode_catalog(preferred);

        assert_eq!(catalog[0], preferred);
        assert!(catalog.contains(&mode(1_920, 1_200, 60_000)));
        assert!(catalog.contains(&mode(1_920, 1_080, 60_000)));
    }

    #[test]
    fn fallback_modes_are_sorted_by_resolution_then_refresh() {
        let preferred = mode(2_560, 1_440, 144_000);
        let catalog = common_mode_catalog(preferred);

        assert_eq!(catalog[0], preferred);
        for pair in catalog[1..].windows(2) {
            let left_pixels = u64::from(pair[0].width) * u64::from(pair[0].height);
            let right_pixels = u64::from(pair[1].width) * u64::from(pair[1].height);
            assert!(left_pixels >= right_pixels);
            if left_pixels == right_pixels {
                assert!(pair[0].refresh_millihz >= pair[1].refresh_millihz);
            }
        }
    }

    #[test]
    fn profile_builder_validates_and_populates_catalog() {
        let preferred = mode(1_920, 1_200, 59_940);
        let profile = build_display_profile(preferred, DisplayProfileSource::Manual).unwrap();

        assert_eq!(profile.preferred_mode, preferred);
        assert_eq!(profile.advertised_modes.first(), Some(&preferred));
        assert_eq!(profile.source_label, "Manual");

        let error = build_display_profile(
            mode(1_920, 1_080, MAX_REFRESH_MILLIHZ + 1),
            DisplayProfileSource::Fallback,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            DisplayModeValidationError::RefreshOutOfRange { .. }
        ));
    }
}
