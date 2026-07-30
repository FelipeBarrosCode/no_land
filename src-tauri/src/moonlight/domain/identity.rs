use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretReference(pub String);

impl SecretReference {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedIdentity {
    pub unique_id: String,
    pub client_name: String,
    pub certificate_pem: String,
    pub private_key_ref: SecretReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    pub unique_id: String,
    pub client_name: String,
    pub certificate_pem: String,
    pub private_key_ref: SecretReference,
    pub private_key_pem: String,
}

impl ClientIdentity {
    pub fn persisted(&self) -> PersistedIdentity {
        PersistedIdentity {
            unique_id: self.unique_id.clone(),
            client_name: self.client_name.clone(),
            certificate_pem: self.certificate_pem.clone(),
            private_key_ref: self.private_key_ref.clone(),
        }
    }
}
