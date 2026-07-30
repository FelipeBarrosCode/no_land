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
    let migrated: MoonlightConfiguration = serde_json::from_value(baseline)?;

    match migrated.schema_version {
        1 => Ok(migrated),
        version => Err(MoonlightError::Migration(format!(
            "unsupported moonlight schema version {version}"
        ))),
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

    use super::migrate;

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
