use async_trait::async_trait;

use super::{request::PinnedCertificate, GameStreamRequest, GameStreamResponse};
use crate::moonlight::domain::MoonlightError;

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
}

impl ReqwestGameStreamHttpClient {
    pub fn new() -> Result<Self, MoonlightError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| MoonlightError::Persistence(error.to_string()))?;
        Ok(Self { client })
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
        let response = self
            .client
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
