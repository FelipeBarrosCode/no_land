//! Optional capture policy. Omitting regenerable data never deletes source files.
use noland_state_core::{BundleManifest, LogicalPath, LogicalRoot, Result, StateError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RegenerableCachePolicy {
    #[default]
    Include,
    Exclude,
}

impl RegenerableCachePolicy {
    pub fn from_value(value: Option<&Value>) -> Result<Self> {
        match value {
            None | Some(Value::Null) => Ok(Self::Include),
            Some(Value::String(value)) if value == "include" => Ok(Self::Include),
            Some(Value::String(value)) if value == "exclude" => Ok(Self::Exclude),
            _ => Err(StateError::Invalid(
                "regenerable_caches must be 'include' or 'exclude'".into(),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Exclude => "exclude",
        }
    }

    pub fn matches_parent(self, parent: &BundleManifest) -> bool {
        Self::from_value(parent.environment.get("regenerable_caches"))
            .is_ok_and(|previous| previous == self)
    }
}

/// Wine generates machine-specific serial/block-device bindings. They are not
/// application data and must neither abort a complete backup nor be restored as
/// links to the destination PC's devices. Never follow the target.
pub(crate) fn is_host_device_mapping(logical: &LogicalPath, target: &Path) -> bool {
    if !matches!(
        logical.logical_root,
        LogicalRoot::ProtonPrefix { .. }
            | LogicalRoot::WinePrefix { .. }
            | LogicalRoot::BottlesPrefix { .. }
    ) || !target.starts_with("/dev")
    {
        return false;
    }
    let Some(name) = logical.relative_path.strip_prefix("dosdevices/") else {
        return false;
    };
    let port = name
        .strip_prefix("com")
        .or_else(|| name.strip_prefix("lpt"))
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()));
    let drive = name.len() == 3 && name.as_bytes()[0].is_ascii_lowercase() && &name[1..] == "::";
    port || drive
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use noland_state_core::{AppId, AppIdentity, BackupMode, ManifestApp, ManifestSource};
    use serde_json::json;

    #[test]
    fn host_device_exclusion_is_limited_to_runtime_dosdevices_bindings() {
        let logical = |root, path| LogicalPath::new(root, path);
        let prefix = LogicalRoot::ProtonPrefix {
            steam_app_id: 553850,
        };
        for name in [
            "dosdevices/com1",
            "dosdevices/com32",
            "dosdevices/lpt1",
            "dosdevices/d::",
        ] {
            assert!(is_host_device_mapping(
                &logical(prefix.clone(), name),
                Path::new("/dev/ttyS0")
            ));
        }
        for name in [
            "dosdevices/c:",
            "drive_c/save.dat",
            "dosdevices/com1/save",
            "dosdevices/com",
            "config/com1",
        ] {
            assert!(!is_host_device_mapping(
                &logical(prefix.clone(), name),
                Path::new("/dev/ttyS0")
            ));
        }
        assert!(!is_host_device_mapping(
            &logical(prefix.clone(), "dosdevices/com1"),
            Path::new("../drive_c")
        ));
        assert!(!is_host_device_mapping(
            &logical(prefix, "dosdevices/com1"),
            Path::new("/device/save")
        ));
        assert!(!is_host_device_mapping(
            &logical(LogicalRoot::Home, "dosdevices/com1"),
            Path::new("/dev/ttyS0")
        ));
    }

    #[test]
    fn legacy_policy_preserves_caches_and_invalid_values_are_rejected() {
        assert_eq!(
            RegenerableCachePolicy::from_value(None).unwrap(),
            RegenerableCachePolicy::Include
        );
        assert_eq!(
            RegenerableCachePolicy::from_value(Some(&Value::Null)).unwrap(),
            RegenerableCachePolicy::Include
        );
        assert_eq!(
            RegenerableCachePolicy::from_value(Some(&json!("exclude"))).unwrap(),
            RegenerableCachePolicy::Exclude
        );
        for value in [json!(false), json!("fast"), json!({"exclude": true})] {
            assert!(RegenerableCachePolicy::from_value(Some(&value)).is_err());
        }
    }

    #[test]
    fn policy_change_cannot_reuse_a_noop_parent() {
        let identity = AppIdentity::new(AppId::desktop("game"), "Game");
        let mut manifest = BundleManifest::new(
            ManifestApp::from(&identity),
            ManifestSource {
                instance_id: uuid::Uuid::new_v4(),
                image_id: "test".into(),
                captured_at: Utc::now(),
            },
            BackupMode::CompleteApplication,
        );
        assert!(RegenerableCachePolicy::Include.matches_parent(&manifest));
        assert!(!RegenerableCachePolicy::Exclude.matches_parent(&manifest));
        manifest.environment["regenerable_caches"] = json!("exclude");
        assert!(RegenerableCachePolicy::Exclude.matches_parent(&manifest));
        assert!(!RegenerableCachePolicy::Include.matches_parent(&manifest));
    }
}
