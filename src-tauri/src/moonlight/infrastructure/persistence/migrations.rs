use serde_json::Value;

use crate::moonlight::domain::{MoonlightConfiguration, MoonlightError};

pub fn migrate(configuration: Value) -> Result<MoonlightConfiguration, MoonlightError> {
    if configuration.is_null() {
        return Ok(MoonlightConfiguration::default());
    }

    if !configuration.is_object() {
        return Err(MoonlightError::Migration(
            "moonligConf must be a JSON object".to_string(),
        ));
    }

    let mut baseline = serde_json::to_value(MoonlightConfiguration::default())?;
    merge_json(&mut baseline, &configuration);
    normalize_legacy_reconnection_settings(&mut baseline);
    let migrated: MoonlightConfiguration = serde_json::from_value(baseline)?;

    match migrated.schema_version {
        1 => Ok(migrated),
        version => Err(MoonlightError::Migration(format!(
            "unsupported moonlight schema version {version}"
        ))),
    }
}

fn normalize_legacy_reconnection_settings(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(reconnection)) = map.get_mut("reconnection") {
                let enabled = reconnection
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let maximum_attempts = reconnection.get("maximumAttempts").and_then(Value::as_u64);
                let initial_delay_ms = reconnection.get("initialDelayMs").and_then(Value::as_u64);
                let maximum_delay_ms = reconnection.get("maximumDelayMs").and_then(Value::as_u64);
                if enabled
                    && maximum_attempts == Some(3)
                    && initial_delay_ms == Some(500)
                    && maximum_delay_ms == Some(5_000)
                {
                    reconnection.insert("maximumAttempts".to_string(), Value::from(1));
                    reconnection.insert("initialDelayMs".to_string(), Value::from(0));
                    reconnection.insert("maximumDelayMs".to_string(), Value::from(0));
                }
            }
            for child in map.values_mut() {
                normalize_legacy_reconnection_settings(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                normalize_legacy_reconnection_settings(child);
            }
        }
        _ => {}
    }
}

fn merge_json(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_value) in source_map {
                match target_map.get_mut(key) {
                    Some(target_value) => merge_json(target_value, source_value),
                    None => {
                        target_map.insert(key.clone(), source_value.clone());
                    }
                }
            }
        }
        (target_slot, source_value) => *target_slot = source_value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{migrate, normalize_legacy_reconnection_settings};

    #[test]
    fn normalizes_legacy_reconnection_defaults() {
        let migrated = migrate(json!({
            "schemaVersion": 1,
            "defaults": {
                "reconnection": {
                    "enabled": true,
                    "maximumAttempts": 3,
                    "initialDelayMs": 500,
                    "maximumDelayMs": 5000
                }
            }
        }))
        .unwrap();

        assert!(migrated.defaults.reconnection.enabled);
        assert_eq!(migrated.defaults.reconnection.maximum_attempts, 1);
        assert_eq!(migrated.defaults.reconnection.initial_delay_ms, 0);
        assert_eq!(migrated.defaults.reconnection.maximum_delay_ms, 0);
    }

    #[test]
    fn normalizes_nested_legacy_reconnection_overrides() {
        let mut value = json!({
            "hosts": {
                "host-1": {
                    "preferencesOverride": {
                        "reconnection": {
                            "enabled": true,
                            "maximumAttempts": 3,
                            "initialDelayMs": 500,
                            "maximumDelayMs": 5000
                        }
                    }
                }
            }
        });

        normalize_legacy_reconnection_settings(&mut value);

        let reconnection = &value["hosts"]["host-1"]["preferencesOverride"]["reconnection"];
        assert_eq!(reconnection["maximumAttempts"], 1);
        assert_eq!(reconnection["initialDelayMs"], 0);
        assert_eq!(reconnection["maximumDelayMs"], 0);
    }

    #[test]
    fn fills_missing_fields_from_defaults() {
        let migrated = migrate(json!({
            "schemaVersion": 1,
            "defaults": {
                "video": {
                    "width": 1280
                }
            }
        }))
        .unwrap();

        assert_eq!(migrated.defaults.video.width, 1280);
        assert_eq!(migrated.defaults.video.height, 1080);
    }
}
