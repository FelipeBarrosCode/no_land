//! Last boundary before telemetry, UI snapshots, errors or tool results leave
//! the application. This is not a sandbox for a deliberately granted shell.
use base64::{engine::general_purpose::STANDARD, Engine};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        let mut secrets = values
            .into_iter()
            .filter(|s| s.len() >= 4)
            .collect::<Vec<_>>();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Self { secrets }
    }

    pub fn sanitize(&self, mut value: Value) -> Value {
        self.visit(&mut value);
        value
    }

    fn visit(&self, value: &mut Value) {
        match value {
            Value::String(s) => *s = self.text(s),
            Value::Array(items) => items.iter_mut().for_each(|v| self.visit(v)),
            Value::Object(object) => {
                for (key, value) in object.iter_mut() {
                    if sensitive_field(key) {
                        *value = Value::String("[redacted]".into());
                    } else if key == "base64"
                        && value.as_str().is_some_and(|s| self.encoded_secret(s))
                    {
                        *value =
                            Value::String("[redacted: file contains credential material]".into());
                    } else {
                        self.visit(value);
                    }
                }
            }
            _ => {}
        }
    }

    fn encoded_secret(&self, text: &str) -> bool {
        STANDARD.decode(text).ok().is_some_and(|bytes| {
            self.secrets
                .iter()
                .any(|s| bytes.windows(s.len()).any(|window| window == s.as_bytes()))
                || bytes.windows(11).any(|window| window == b"PRIVATE KEY")
        })
    }

    pub fn text(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for secret in &self.secrets {
            text = text.replace(secret, "[redacted]");
        }
        static PEM: OnceLock<Regex> = OnceLock::new();
        static BEARER: OnceLock<Regex> = OnceLock::new();
        static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
        static MCP_TOKEN: OnceLock<Regex> = OnceLock::new();
        let text = PEM.get_or_init(|| Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)").unwrap())
            .replace_all(&text, "[redacted private key]");
        let text = BEARER
            .get_or_init(|| Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._~+/=-]+").unwrap())
            .replace_all(&text, "Bearer [redacted]");
        let text = ASSIGNMENT.get_or_init(|| Regex::new(r"(?im)(?:password|private[_ -]?key|api[_ -]?key|client[_ -]?secret|access[_ -]?token)\s*[:=]\s*[^\r\n]+").unwrap())
            .replace_all(&text, "[redacted credential]");
        MCP_TOKEN
            .get_or_init(|| Regex::new(r"nlc_[a-f0-9]{64}").unwrap())
            .replace_all(&text, "[redacted MCP token]")
            .into_owned()
    }
}

fn sensitive_field(key: &str) -> bool {
    let key = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "password",
        "privatekey",
        "apikey",
        "apitoken",
        "accesstoken",
        "refreshtoken",
        "clientsecret",
        "repositorykey",
        "wireguardconfig",
        "authorization",
        "secretaccesskey",
        "backblazekey",
        "backblazeapplicationkey",
        "credentials",
    ]
    .iter()
    .any(|part| key.contains(part))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn redacts_nested_secrets_and_encoded_file_contents() {
        let r = Redactor::new(["very-private-value".into()]);
        let result = r.sanitize(json!({
            "details":"error: very-private-value", "nested":[{"api_key":"unknown-secret"}],
            "base64":STANDARD.encode(b"a file with very-private-value"), "fingerprint":"SHA256:abc",
        }));
        let text = result.to_string();
        assert!(!text.contains("very-private-value"));
        assert!(!text.contains("unknown-secret"));
        assert_eq!(result["fingerprint"], "SHA256:abc");
        assert!(result["base64"].as_str().unwrap().starts_with("[redacted"));
    }

    #[test]
    fn catches_bearer_pem_wireguard_and_mcp_token_text() {
        let r = Redactor::new([]);
        for text in [
            "Bearer abcdefg",
            "PrivateKey = abcdefg",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabcdefg\n-----END OPENSSH PRIVATE KEY-----",
        ] {
            assert!(!r.text(text).contains("abcdefg"));
        }
        assert!(!r
            .text(&format!("token {}", crate::policy::generate_token()))
            .contains("nlc_"));
    }
}
