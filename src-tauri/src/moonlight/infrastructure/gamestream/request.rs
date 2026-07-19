use std::time::Duration;

use crate::moonlight::domain::SecretReference;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameStreamScheme {
    Http,
    Https,
}

impl GameStreamScheme {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentityReference {
    pub certificate_pem: String,
    pub private_key_ref: SecretReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedCertificate {
    pub sha256_hex: String,
}

#[derive(Debug, Clone)]
pub struct GameStreamRequest {
    pub address: String,
    pub port: u16,
    pub scheme: GameStreamScheme,
    pub endpoint: String,
    pub query: Vec<(String, String)>,
    pub identity: Option<ClientIdentityReference>,
    pub pinned_certificate: Option<PinnedCertificate>,
    pub timeout: Duration,
}
