use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use async_trait::async_trait;
use x509_parser::{parse_x509_certificate, pem::parse_x509_pem};

use super::{request::PinnedCertificate, GameStreamRequest, GameStreamResponse};
use crate::moonlight::{domain::MoonlightError, infrastructure::secrets::SecretStore};

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
    secret_store: Arc<dyn SecretStore>,
}

impl ReqwestGameStreamHttpClient {
    pub fn new(secret_store: Arc<dyn SecretStore>) -> Result<Self, MoonlightError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|error| MoonlightError::Persistence(error.to_string()))?;
        Ok(Self {
            client,
            secret_store,
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

    fn request_host_and_resolve_override(
        request: &GameStreamRequest,
    ) -> Result<(String, Option<SocketAddr>), MoonlightError> {
        let address = request.address.trim();
        let Ok(ip) = address.parse::<IpAddr>() else {
            return Ok((address.to_string(), None));
        };

        let pinned = request.pinned_certificate.as_ref().ok_or_else(|| {
            MoonlightError::Validation(
                "HTTPS GameStream requests require a pinned server certificate".to_string(),
            )
        })?;

        if let Some(hostname) = tls_hostname_from_pinned_certificate(pinned)? {
            return Ok((hostname, Some(SocketAddr::new(ip, request.port))));
        }

        Ok((address.to_string(), None))
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
        let (request_host, resolve_override) = Self::request_host_and_resolve_override(request)?;

        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .identity(client_identity)
            .add_root_certificate(pinned_certificate);

        if let Some(socket_addr) = resolve_override {
            builder = builder.resolve(&request_host, socket_addr);
        }

        builder
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

        let (url_host, _) = if matches!(request.scheme, super::request::GameStreamScheme::Https) {
            Self::request_host_and_resolve_override(&request)?
        } else {
            (request.address.clone(), None)
        };
        let url = format!(
            "{}://{}:{}{}",
            request.scheme.as_str(),
            url_host,
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

        let body = response
            .text()
            .await
            .map_err(|error| MoonlightError::Persistence(error.to_string()))?;

        Ok(GameStreamResponse { body })
    }
}

fn tls_hostname_from_pinned_certificate(
    pinned: &PinnedCertificate,
) -> Result<Option<String>, MoonlightError> {
    let (_, pem) = parse_x509_pem(pinned.certificate_pem.as_bytes())
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    let (_, certificate) = parse_x509_certificate(&pem.contents)
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;

    if let Ok(Some(san)) = certificate.subject_alternative_name() {
        for general_name in &san.value.general_names {
            if let x509_parser::extensions::GeneralName::DNSName(hostname) = general_name {
                let hostname = hostname.trim();
                if !hostname.is_empty() {
                    return Ok(Some(hostname.to_string()));
                }
            }
        }
    }

    for attribute in certificate.subject().iter_common_name() {
        if let Ok(hostname) = attribute.as_str() {
            let hostname = hostname.trim();
            if !hostname.is_empty() {
                return Ok(Some(hostname.to_string()));
            }
        }
    }

    Ok(None)
}
