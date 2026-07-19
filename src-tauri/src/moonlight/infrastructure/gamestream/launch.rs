use std::{ffi::CStr, time::Duration};

use crate::moonlight::{
    domain::{
        LaunchOperation, LaunchRequestParameters, LaunchResult, MoonlightError, PersistedIdentity,
        PersistedPairing,
    },
    infrastructure::gamestream::{
        xml::{first_text, parse_document, parse_success_status},
        ClientIdentityReference, GameStreamRequest, GameStreamScheme, PinnedCertificate,
    },
};

pub fn build_launch_or_resume_request(
    address: String,
    port: u16,
    identity: &PersistedIdentity,
    pairing: &PersistedPairing,
    operation: LaunchOperation,
    parameters: &LaunchRequestParameters,
    timeout: Duration,
) -> GameStreamRequest {
    let endpoint = match operation {
        LaunchOperation::Launch => "/launch",
        LaunchOperation::Resume => "/resume",
    };

    let mut query = vec![
        ("appid".to_string(), parameters.app_id.to_string()),
        ("mode".to_string(), parameters.mode.clone()),
        ("rikey".to_string(), parameters.ri_key_hex.clone()),
        ("rikeyid".to_string(), parameters.ri_key_id.clone()),
        (
            "localAudioPlayMode".to_string(),
            if parameters.play_local_audio {
                "1"
            } else {
                "0"
            }
            .to_string(),
        ),
        (
            "surroundAudioInfo".to_string(),
            match parameters.audio_configuration {
                crate::moonlight::domain::AudioConfiguration::Stereo => "2",
                crate::moonlight::domain::AudioConfiguration::Surround51 => "6",
                crate::moonlight::domain::AudioConfiguration::Surround71 => "8",
            }
            .to_string(),
        ),
        (
            "gcmap".to_string(),
            if parameters.persist_gamepads_after_disconnect {
                "1"
            } else {
                "0"
            }
            .to_string(),
        ),
        (
            "hdr".to_string(),
            if parameters.hdr { "1" } else { "0" }.to_string(),
        ),
    ];
    query.push(("client".to_string(), "Noland Connect".to_string()));
    query.extend(moonlight_launch_query_parameters());

    GameStreamRequest {
        address,
        port,
        scheme: GameStreamScheme::Https,
        endpoint: endpoint.to_string(),
        query,
        identity: Some(ClientIdentityReference {
            certificate_pem: identity.certificate_pem.clone(),
            private_key_ref: identity.private_key_ref.clone(),
        }),
        pinned_certificate: Some(PinnedCertificate {
            sha256_hex: pairing.server_certificate_sha256.clone(),
            certificate_pem: pairing.server_certificate_pem.clone(),
        }),
        timeout,
    }
}

fn moonlight_launch_query_parameters() -> Vec<(String, String)> {
    let pointer = unsafe { crate::moonlight::native::nl_get_launch_query_parameters() };
    if pointer.is_null() {
        return Vec::new();
    }

    let value = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
    let trimmed = value.trim_start_matches('&');
    if trimmed.is_empty() {
        return Vec::new();
    }

    trimmed
        .split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let mut pieces = part.splitn(2, '=');
            let key = pieces.next()?.trim();
            if key.is_empty() {
                return None;
            }
            let value = pieces.next().unwrap_or("").trim();
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

pub fn build_cancel_request(
    address: String,
    port: u16,
    identity: &PersistedIdentity,
    pairing: &PersistedPairing,
    timeout: Duration,
) -> GameStreamRequest {
    GameStreamRequest {
        address,
        port,
        scheme: GameStreamScheme::Https,
        endpoint: "/cancel".to_string(),
        query: vec![],
        identity: Some(ClientIdentityReference {
            certificate_pem: identity.certificate_pem.clone(),
            private_key_ref: identity.private_key_ref.clone(),
        }),
        pinned_certificate: Some(PinnedCertificate {
            sha256_hex: pairing.server_certificate_sha256.clone(),
            certificate_pem: pairing.server_certificate_pem.clone(),
        }),
        timeout,
    }
}

pub fn parse_launch_response(
    xml: &str,
    operation: LaunchOperation,
) -> Result<LaunchResult, MoonlightError> {
    let endpoint = match operation {
        LaunchOperation::Launch => "/launch",
        LaunchOperation::Resume => "/resume",
    };
    let document = parse_document(xml, endpoint)?;
    parse_success_status(&document, endpoint)?;
    Ok(LaunchResult {
        operation,
        rtsp_session_url: first_text(&document, "sessionUrl0")
            .or_else(|| first_text(&document, "sessionUrl")),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{build_cancel_request, build_launch_or_resume_request, parse_launch_response};
    use crate::moonlight::domain::{
        AudioConfiguration, LaunchOperation, LaunchRequestParameters, PairingStatus,
        PersistedIdentity, PersistedPairing, SecretReference,
    };

    #[test]
    fn builds_launch_request() {
        let identity = PersistedIdentity {
            unique_id: "abc".to_string(),
            client_name: "Noland Connect".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_ref: SecretReference::new("os-keychain://noland/moonlight-client-key"),
        };
        let pairing = PersistedPairing {
            status: PairingStatus::Paired,
            server_certificate_pem: "server-cert".to_string(),
            server_certificate_sha256: "deadbeef".to_string(),
            paired_at: "now".to_string(),
        };
        let req = build_launch_or_resume_request(
            "10.77.0.1".to_string(),
            47989,
            &identity,
            &pairing,
            LaunchOperation::Launch,
            &LaunchRequestParameters {
                app_id: 1,
                mode: "1920x1080x60".to_string(),
                ri_key_hex: "abcd".to_string(),
                ri_key_id: "123".to_string(),
                audio_configuration: AudioConfiguration::Stereo,
                play_local_audio: true,
                persist_gamepads_after_disconnect: false,
                hdr: false,
            },
            Duration::from_secs(10),
        );
        assert_eq!(req.endpoint, "/launch");
        assert!(matches!(
            req.scheme,
            crate::moonlight::infrastructure::gamestream::GameStreamScheme::Https
        ));
        assert!(req.query.iter().any(|(k, _)| k == "rikey"));
        assert!(req.query.iter().any(|(k, v)| k == "corever" && v == "1"));
        assert!(req.identity.is_some());
        assert!(req.pinned_certificate.is_some());
    }

    #[test]
    fn parses_launch_response() {
        let parsed = parse_launch_response(r#"<root><status_code>200</status_code><status_message>OK</status_message><sessionUrl0>rtsp://example</sessionUrl0></root>"#, LaunchOperation::Launch).unwrap();
        assert_eq!(parsed.rtsp_session_url.as_deref(), Some("rtsp://example"));
    }

    #[test]
    fn builds_cancel_request() {
        let identity = PersistedIdentity {
            unique_id: "abc".to_string(),
            client_name: "Noland Connect".to_string(),
            certificate_pem: "cert".to_string(),
            private_key_ref: SecretReference::new("os-keychain://noland/moonlight-client-key"),
        };
        let pairing = PersistedPairing {
            status: PairingStatus::Paired,
            server_certificate_pem: "server-cert".to_string(),
            server_certificate_sha256: "deadbeef".to_string(),
            paired_at: "now".to_string(),
        };
        let req = build_cancel_request(
            "10.77.0.1".to_string(),
            47989,
            &identity,
            &pairing,
            Duration::from_secs(5),
        );
        assert_eq!(req.endpoint, "/cancel");
        assert!(matches!(
            req.scheme,
            crate::moonlight::infrastructure::gamestream::GameStreamScheme::Https
        ));
    }
}
