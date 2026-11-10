use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub port: u16,
    pub instances: Vec<u64>,
    pub all_instances: bool,
    pub control: bool,
    pub shell: bool,
    pub elevated: bool,
    pub desktop: bool,
    pub dialogs: bool,
    pub local_roots: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 47831,
            instances: vec![],
            all_instances: false,
            control: false,
            shell: false,
            elevated: false,
            desktop: false,
            dialogs: false,
            local_roots: vec![],
        }
    }
}

impl Config {
    /// The desktop app has one opt-in for trusted local agents. Keep legacy
    /// fields readable, but do not require per-machine or capability setup.
    pub fn for_local_agents(mut self) -> Self {
        self.all_instances = true;
        self.instances.clear();
        self.control = true;
        self.shell = true;
        self.elevated = true;
        self.desktop = true;
        self.dialogs = true;
        self
    }

    pub fn allows_instance(&self, id: u64) -> bool {
        self.all_instances || self.instances.contains(&id)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.port < 1024 {
            return Err("Choose a localhost port between 1024 and 65535".into());
        }
        if self.instances.len() > 100 || self.local_roots.len() > 16 {
            return Err("Too many permission grants".into());
        }
        if self.elevated && !self.shell {
            return Err("Elevated access requires remote shell access".into());
        }
        for root in &self.local_roots {
            let p = std::path::Path::new(root);
            if !p.is_absolute() || !p.is_dir() {
                return Err("Local transfer roots must be existing absolute directories".into());
            }
        }
        Ok(())
    }
}

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!(
        "nlc_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

pub fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}
pub fn fingerprint(token: &str) -> String {
    format!(
        "SHA256:{}",
        digest(token.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

pub fn authorized_token(expected: &[u8; 32], supplied: &str) -> bool {
    supplied.len() <= 256 && bool::from(expected.ct_eq(&digest(supplied.as_bytes())))
}

pub fn allowed_http_peer(host: &str, origin: Option<&str>, port: u16) -> bool {
    let a = format!("127.0.0.1:{port}");
    let b = format!("localhost:{port}");
    (host == a || host == b)
        && origin.is_none_or(|o| o == format!("http://{a}") || o == format!("http://{b}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Control,
    Shell,
    Desktop,
    Dialog,
}

impl Scope {
    pub fn allowed(self, config: &Config) -> bool {
        match self {
            Self::Read => true,
            Self::Control => config.control,
            Self::Shell => config.shell,
            Self::Desktop => config.desktop,
            Self::Dialog => config.dialogs,
        }
    }
}

#[derive(Clone)]
pub struct Spec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Map<String, Value>,
    pub scope: Scope,
    pub machine: bool,
    pub approval: bool,
    pub background: bool,
}

impl Spec {
    pub fn authorize(&self, config: &Config, args: &Map<String, Value>) -> Result<(), String> {
        if !config.enabled {
            return Err("MCP is disabled".into());
        }
        if !self.scope.allowed(config) {
            return Err("This capability has not been granted in Settings → MCP".into());
        }
        if self.machine {
            let id = args
                .get("instance_id")
                .and_then(Value::as_u64)
                .ok_or("instance_id is required")?;
            if !config.allows_instance(id) {
                return Err("This instance has not been granted to the agent".into());
            }
        }
        if args.get("as_root").and_then(Value::as_bool) == Some(true) && !config.elevated {
            return Err("Elevated SSH access has not been granted".into());
        }
        validate_args(&self.schema, args)
    }
}

// Only the deliberately small schema vocabulary used by our fixed catalog is
// accepted. Runtime validation is server-side, not delegated to an AI client.
pub fn validate_args(schema: &Map<String, Value>, args: &Map<String, Value>) -> Result<(), String> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or("Invalid tool schema")?;
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !args.contains_key(name) {
                return Err(format!("Missing argument: {name}"));
            }
        }
    }
    for (name, value) in args {
        let p = properties
            .get(name)
            .ok_or_else(|| format!("Unknown argument: {name}"))?;
        let valid = match p.get("type").and_then(Value::as_str) {
            Some("string") => value.as_str().is_some_and(|s| {
                s.len() <= p.get("maxLength").and_then(Value::as_u64).unwrap_or(262144) as usize
                    && !s.contains('\0')
            }),
            Some("integer") => value.as_u64().is_some_and(|n| {
                n >= p.get("minimum").and_then(Value::as_u64).unwrap_or(0)
                    && n <= p.get("maximum").and_then(Value::as_u64).unwrap_or(u64::MAX)
            }),
            Some("number") => value.as_f64().is_some_and(|n| {
                n.is_finite()
                    && n >= p.get("minimum").and_then(Value::as_f64).unwrap_or(0.0)
                    && n <= p.get("maximum").and_then(Value::as_f64).unwrap_or(1000.0)
            }),
            Some("boolean") => value.is_boolean(),
            Some("object") => value.is_object() && value.to_string().len() <= 262144,
            Some("array") => value.as_array().is_some_and(|a| {
                a.len() <= 100
                    && a.iter()
                        .all(|v| v.is_string() && v.as_str().unwrap().len() <= 4096)
            }),
            _ => false,
        };
        if !valid
            || p.get("enum")
                .and_then(Value::as_array)
                .is_some_and(|options| !options.contains(value))
        {
            return Err(format!("Invalid argument: {name}"));
        }
    }
    Ok(())
}

pub fn argument_hash(tool: &str, args: &Map<String, Value>) -> String {
    let mut args = args.clone();
    args.remove("approval_id");
    args.remove("request_id");
    fingerprint(&format!("{tool}:{}", Value::Object(args)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_agent_setup_includes_future_instances_without_enabling_itself() {
        let config = Config {
            port: 47832,
            instances: vec![42],
            ..Default::default()
        }
        .for_local_agents();
        assert!(!config.enabled);
        assert_eq!(config.port, 47832);
        assert!(config.instances.is_empty());
        assert!(config.allows_instance(42));
        assert!(config.allows_instance(123456));
        assert!(
            config.control && config.shell && config.elevated && config.desktop && config.dialogs
        );
        assert!(
            config.local_roots.is_empty(),
            "no arbitrary local filesystem grant"
        );
        config.validate().unwrap();
    }

    #[test]
    fn legacy_scoped_configuration_still_deserializes() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "enabled": true, "instances": [42]
        }))
        .unwrap();
        assert!(config.allows_instance(42));
        assert!(!config.allows_instance(43));
        assert!(!config.all_instances);
    }

    #[test]
    fn tokens_are_random_and_not_fingerprints() {
        let token = generate_token();
        assert_ne!(token, generate_token());
        assert!(authorized_token(&digest(token.as_bytes()), &token));
        assert!(!authorized_token(
            &digest(token.as_bytes()),
            &fingerprint(&token)
        ));
    }
    #[test]
    fn refuses_dns_rebinding_and_foreign_origins() {
        assert!(allowed_http_peer("127.0.0.1:47831", None, 47831));
        for host in ["0.0.0.0:47831", "localhost.evil:47831", "127.0.0.1:1"] {
            assert!(!allowed_http_peer(host, None, 47831));
        }
        assert!(!allowed_http_peer(
            "localhost:47831",
            Some("https://evil.example"),
            47831
        ));
        assert!(!allowed_http_peer("localhost:47831", Some("null"), 47831));
    }
}
