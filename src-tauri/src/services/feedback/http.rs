use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use reqwest::{Method, Url};
use serde_json::Value;

const RESPONSE_LIMIT: usize = 512 * 1024;

#[derive(Debug)]
pub(super) struct Failure {
    pub message: String,
    pub status: Option<u16>,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
}
impl Failure {
    pub fn local(message: &str) -> Self {
        Self {
            message: message.into(),
            status: None,
            retryable: false,
            retry_after: None,
        }
    }
}

pub(super) struct FeedbackHttp {
    base: Option<String>,
    client: Option<reqwest::Client>,
    blocked_until: Mutex<Option<Instant>>,
}
impl FeedbackHttp {
    pub fn new() -> Self {
        let base = configured_url(option_env!("NOLAND_FEEDBACK_API_URL"));
        let client = base.as_ref().and_then(|_| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(8))
                .build()
                .ok()
        });
        Self {
            base,
            client,
            blocked_until: Mutex::new(None),
        }
    }
    pub fn configured(&self) -> bool {
        self.base.is_some() && self.client.is_some()
    }

    // Only FeedbackProgramService can reach this adapter. It authorizes each operation at the last possible point.
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
        request_id: Option<&str>,
    ) -> Result<Value, Failure> {
        self.request_with_revision(method, path, token, body, request_id, None)
            .await
    }

    pub async fn delete_review(
        &self,
        path: &str,
        token: &str,
        revision: u64,
    ) -> Result<Value, Failure> {
        self.request_with_revision(
            Method::DELETE,
            path,
            Some(token),
            None,
            None,
            Some(revision),
        )
        .await
    }

    async fn request_with_revision(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
        request_id: Option<&str>,
        review_revision: Option<u64>,
    ) -> Result<Value, Failure> {
        if let Some(until) = *self
            .blocked_until
            .lock()
            .map_err(|_| Failure::local("Feedback rate-limit state is unavailable"))?
        {
            if let Some(remaining) = until.checked_duration_since(Instant::now()) {
                return Err(Failure {
                    message: "Feedback is rate limited; pending work remains local".into(),
                    status: Some(429),
                    retryable: true,
                    retry_after: Some(remaining),
                });
            }
        }
        let base = self
            .base
            .as_ref()
            .ok_or_else(|| Failure::local("Feedback service is not configured in this build"))?;
        let client = self.client.as_ref().ok_or_else(|| {
            Failure::local("Feedback HTTP support is unavailable; rental functions are unaffected")
        })?;
        let mut request = client.request(method, format!("{base}/v1{path}"));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(revision) = review_revision {
            request = request.header("X-Review-Revision", revision.to_string());
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(request_id) = request_id {
            request = request
                .header("Idempotency-Key", request_id)
                .header("X-Request-Id", request_id);
        }
        let mut response = request.send().await.map_err(|_| Failure {
            message: "Feedback service is unreachable; pending work is retained locally".into(),
            status: None,
            retryable: true,
            retry_after: None,
        })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(v).ok().map(|at| {
                        Duration::from_secs(
                            (at.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64,
                        )
                    })
                })
            })
            .map(|d| d.min(Duration::from_secs(24 * 60 * 60)));
        if status.as_u16() == 429 {
            let delay = retry_after.unwrap_or(Duration::from_secs(60));
            if let Ok(mut until) = self.blocked_until.lock() {
                *until = Some(Instant::now() + delay);
            }
        }
        if !status.is_success() {
            // Never surface arbitrary server bodies, URLs, tokens, or submitted prose in logs/UI errors.
            return Err(Failure {
                message: format!(
                    "Feedback request was not accepted (HTTP {})",
                    status.as_u16()
                ),
                status: Some(status.as_u16()),
                retryable: status.as_u16() == 429 || status.is_server_error(),
                retry_after,
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > RESPONSE_LIMIT as u64)
        {
            return Err(Failure::local("Feedback response exceeds its size limit"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure {
            message: "Feedback response was interrupted; pending work is retained locally".into(),
            status: None,
            retryable: true,
            retry_after: None,
        })? {
            if bytes.len() + chunk.len() > RESPONSE_LIMIT {
                return Err(Failure::local("Feedback response exceeds its size limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Ok(serde_json::json!({}));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| Failure::local("Feedback service returned an invalid response"))
    }
}

fn configured_url(raw: Option<&str>) -> Option<String> {
    let url = Url::parse(raw?.trim()).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let local_debug = cfg!(debug_assertions)
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() != "https" && !(local_debug && url.scheme() == "http") {
        return None;
    }
    if !matches!(url.path(), "" | "/") {
        return None;
    }
    Some(url.as_str().trim_end_matches('/').into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_or_unsafe_public_url_is_unavailable() {
        assert!(configured_url(None).is_none());
        assert!(configured_url(Some("http://feedback.example")).is_none());
        assert!(configured_url(Some("https://token@feedback.example")).is_none());
        assert_eq!(
            configured_url(Some("https://feedback.example/")),
            Some("https://feedback.example".into())
        );
    }
}
