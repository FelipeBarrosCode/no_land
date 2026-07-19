use async_trait::async_trait;

use super::{request::PinnedCertificate, GameStreamRequest, GameStreamResponse};
use crate::moonlight::{
    domain::MoonlightError,
    infrastructure::secrets::{KeyringSecretStore, SecretStore},
};

#[async_trait]
pub trait GameStreamHttpClient: Send + Sync {
    async fn execute(
        &self,
        request: GameStreamRequest,
    ) -> Result<GameStreamResponse, MoonlightError>;
}

#[derive(Clone)]
pub struct ReqwestGameStreamHttpClient {
    client: reqwest::Client,
    secret_store: KeyringSecretStore,
}

impl ReqwestGameStreamHttpClient {
    pub fn new() -> Result<Self, MoonlightError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|error| MoonlightError::Persistence(error.to_string()))?;
        Ok(Self {
            client,
            secret_store: KeyringSecretStore::default(),
        })
    }

    fn validate_https_prerequisites(
        pinned: &Option<PinnedCertificate>,
        has_identity: bool,
    ) -> Result<(), MoonlightError> {
        if pinned.is_none() {
            return Err(MoonlightError::Validation(
                "HTTPS GameStream requests require a pinned server certificate".to_string(),
            ));
        }
        if !has_identity {
            return Err(MoonlightError::Validation(
                "HTTPS GameStream requests require a client identity reference".to_string(),
            ));
        }
        Ok(())
    }

    async fn https_client_for_request(
        &self,
        request: &GameStreamRequest,
    ) -> Result<reqwest::Client, MoonlightError> {
        let identity = request.identity.as_ref().ok_or_else(|| {
            MoonlightError::Validation(
                "HTTPS GameStream requests require a client identity reference".to_string(),
            )
        })?;
        let pinned = request.pinned_certificate.as_ref().ok_or_else(|| {
            MoonlightError::Validation(
                "HTTPS GameStream requests require a pinned server certificate".to_string(),
            )
        })?;
        let private_key_bytes = self
            .secret_store
            .get(&identity.private_key_ref)
            .await?
            .ok_or_else(|| {
                MoonlightError::IdentityInvalid(
                    "private key is missing for the persisted Moonlight identity".to_string(),
                )
            })?;
        let private_key_pem = String::from_utf8(private_key_bytes.0)
            .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
        let identity_pem = format!("{}\n{}", identity.certificate_pem, private_key_pem);
        let client_identity = reqwest::Identity::from_pem(identity_pem.as_bytes())
            .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
        let pinned_certificate = reqwest::Certificate::from_pem(pinned.certificate_pem.as_bytes())
            .map_err(|error| MoonlightError::Validation(error.to_string()))?;

        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .identity(client_identity)
            .add_root_certificate(pinned_certificate)
            .build()
            .map_err(|error| MoonlightError::Persistence(error.to_string()))
    }
}

#[async_trait]
impl GameStreamHttpClient for ReqwestGameStreamHttpClient {
    async fn execute(
        &self,
        request: GameStreamRequest,
    ) -> Result<GameStreamResponse, MoonlightError> {
        if matches!(request.scheme, super::request::GameStreamScheme::Https) {
            Self::validate_https_prerequisites(
                &request.pinned_certificate,
                request.identity.is_some(),
            )?;
        }

        let url = format!(
            "{}://{}:{}{}",
            request.scheme.as_str(),
            request.address,
            request.port,
            request.endpoint
        );
        let client = if matches!(request.scheme, super::request::GameStreamScheme::Https) {
            self.https_client_for_request(&request).await?
        } else {
            self.client.clone()
        };
        let response = client
            .get(url)
            .query(&request.query)
            .timeout(request.timeout)
            .send()
            .await
            .map_err(|error| MoonlightError::Persistence(error.to_string()))?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let body = response
            .text()
            .await
            .map_err(|error| MoonlightError::Persistence(error.to_string()))?;

        Ok(GameStreamResponse {
            status,
            body,
            content_type,
        })
    }
}
