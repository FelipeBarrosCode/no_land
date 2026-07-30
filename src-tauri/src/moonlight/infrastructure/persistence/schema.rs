use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::moonlight::domain::MoonlightConfiguration;

pub const MOONLIGHT_CONFIG_KEY: &str = "moonligConf";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MoonlightRootDocument {
    #[serde(rename = "moonligConf")]
    pub moonlight_conf: MoonlightConfiguration,
    #[serde(flatten)]
    pub other_categories: BTreeMap<String, Value>,
}

impl Default for MoonlightRootDocument {
    fn default() -> Self {
        Self {
            moonlight_conf: MoonlightConfiguration::default(),
            other_categories: BTreeMap::new(),
        }
    }
}
