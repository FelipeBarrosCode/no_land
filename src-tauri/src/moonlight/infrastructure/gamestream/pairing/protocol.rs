use std::{future::Future, time::Duration};

use reqwest::{Identity, Url};
use uuid::Uuid;

use super::crypto::{
    aes_ecb_decrypt, aes_ecb_encrypt, cert_signature_from_pem, derive_aes_key,
    generate_random_bytes, sha256_hex, sign_with_private_key_sha256, verify_signature_sha256,
    PairingHashAlgorithm,
};
use crate::moonlight::{
    domain::MoonlightError,
    infrastructure::gamestream::{parse_server_info_response, request::GameStreamScheme},
};

#[derive(Debug, Clone)]
pub struct PairHostRequest {
    pub address: String,
    pub http_port: u16,
    pub https_port: Option<u16>,
    pub unique_id: String,
    pub pin: String,
    pub client_certificate_pem: String,
    pub client_private_key_pem: String,
    pub server_app_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PairHostResult {
    pub server_certificate_pem: String,
    pub server_certificate_sha256: String,
}

pub async fn pair_host_with_stage1_authorization<F, Fut>(
    request: PairHostRequest,
    authorize_after_stage1_pending: F,
) -> Result<PairHostResult, MoonlightError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), MoonlightError>>,
{
    let resolved_ports = resolve_pairing_ports(&request).await?;
    let app_version = match request.server_app_version {
        Some(version) => version,
        None => resolved_ports.app_version.clone(),
    };
    let server_major_version = app_version
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(7);
    let hash = PairingHashAlgorithm::for_server_major_version(server_major_version);

    let salt = generate_random_bytes(16);
    let aes_key = derive_aes_key(&salt, &request.pin, hash);
    let client_cert_hex = hex_encode(request.client_certificate_pem.as_bytes());

    let address = request.address.clone();
    let unique_id = request.unique_id.clone();
    let http_port = resolved_ports.http_port;
    let stage1_params = vec![
        ("devicename", "roth".to_string()),
        ("updateState", "1".to_string()),
        ("phrase", "getservercert".to_string()),
        ("salt", hex_encode(&salt)),
        ("clientcert", client_cert_hex),
    ];
    let stage1_task = tokio::spawn(async move {
        pair_request(
            &address,
            http_port,
            &unique_id,
            &stage1_params,
            GameStreamScheme::Http,
            None,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(250)).await;
    authorize_after_stage1_pending().await?;

    let stage1 = stage1_task.await.map_err(|error| {
        MoonlightError::Persistence(format!("pair stage 1 task failed: {error}"))
    })??;
    ensure_paired(&stage1, 1)?;
    let server_cert_pem_bytes = hex_decode(&xml_value(&stage1, "plaincert")?)?;
    if server_cert_pem_bytes.is_empty() {
        return Err(MoonlightError::Validation(
            "server likely already pairing with another client".to_string(),
        ));
    }
    let server_certificate_pem = String::from_utf8(server_cert_pem_bytes.clone())
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;

    let random_challenge = generate_random_bytes(16);
    let encrypted_challenge = aes_ecb_encrypt(&random_challenge, &aes_key)?;
    let stage2 = pair_request(
        &request.address,
        resolved_ports.http_port,
        &request.unique_id,
        &[
            ("devicename", "roth".to_string()),
            ("updateState", "1".to_string()),
            ("clientchallenge", hex_encode(&encrypted_challenge)),
        ],
        GameStreamScheme::Http,
        None,
    )
    .await?;
    ensure_paired(&stage2, 2)?;
    let challenge_response_data = aes_ecb_decrypt(
        &hex_decode(&xml_value(&stage2, "challengeresponse")?)?,
        &aes_key,
    )?;
    if challenge_response_data.len() < hash.digest_len() + 16 {
        return Err(MoonlightError::Validation(
            "invalid challengeresp at stage 2".to_string(),
        ));
    }
    let server_response = challenge_response_data[..hash.digest_len()].to_vec();
    let mut challenge_response =
        challenge_response_data[hash.digest_len()..hash.digest_len() + 16].to_vec();
    challenge_response
        .extend_from_slice(&cert_signature_from_pem(&request.client_certificate_pem)?);
    let client_secret = generate_random_bytes(16);
    challenge_response.extend_from_slice(&client_secret);
    let mut padded_hash = hash.digest(&challenge_response);
    padded_hash.resize(32, 0);
    let encrypted_hash = aes_ecb_encrypt(&padded_hash, &aes_key)?;

    let stage3 = pair_request(
        &request.address,
        resolved_ports.http_port,
        &request.unique_id,
        &[
            ("devicename", "roth".to_string()),
            ("updateState", "1".to_string()),
            ("serverchallengeresp", hex_encode(&encrypted_hash)),
        ],
        GameStreamScheme::Http,
        None,
    )
    .await?;
    ensure_paired(&stage3, 3)?;
    let pairing_secret = hex_decode(&xml_value(&stage3, "pairingsecret")?)?;
    if pairing_secret.len() <= 16 {
        return Err(MoonlightError::Validation(
            "invalid pairingsecret at stage 3".to_string(),
        ));
    }
    let server_secret = &pairing_secret[..16];
    let server_signature = &pairing_secret[16..];
    if !verify_signature_sha256(&server_certificate_pem, server_secret, server_signature)? {
        let _ = unpair(&request.address, request.http_port, &request.unique_id).await;
        return Err(MoonlightError::Validation(
            "MITM detected during pairing".to_string(),
        ));
    }

    let mut expected_response = random_challenge.clone();
    expected_response.extend_from_slice(&cert_signature_from_pem(&server_certificate_pem)?);
    expected_response.extend_from_slice(server_secret);
    if hash.digest(&expected_response) != server_response {
        let _ = unpair(&request.address, request.http_port, &request.unique_id).await;
        return Err(MoonlightError::Validation("incorrect PIN".to_string()));
    }

    let mut client_pairing_secret = client_secret.clone();
    client_pairing_secret.extend_from_slice(&sign_with_private_key_sha256(
        &request.client_private_key_pem,
        &client_secret,
    )?);
    let stage4 = pair_request(
        &request.address,
        resolved_ports.http_port,
        &request.unique_id,
        &[
            ("devicename", "roth".to_string()),
            ("updateState", "1".to_string()),
            ("clientpairingsecret", hex_encode(&client_pairing_secret)),
        ],
        GameStreamScheme::Http,
        None,
    )
    .await?;
    ensure_paired(&stage4, 4)?;

    let https_port = resolved_ports.https_port;
    let identity_pem = format!(
        "{}{}",
        request.client_certificate_pem, request.client_private_key_pem
    );
    let stage5 = pair_request(
        &request.address,
        https_port,
        &request.unique_id,
        &[
            ("devicename", "roth".to_string()),
            ("updateState", "1".to_string()),
            ("phrase", "pairchallenge".to_string()),
        ],
        GameStreamScheme::Https,
        Some(identity_pem),
    )
    .await?;
    ensure_paired(&stage5, 5)?;

    Ok(PairHostResult {
        server_certificate_pem: server_certificate_pem.clone(),
        server_certificate_sha256: sha256_hex(server_certificate_pem.as_bytes()),
    })
}

#[derive(Debug, Clone)]
struct ResolvedPairingPorts {
    http_port: u16,
    https_port: u16,
    app_version: String,
}

async fn fetch_server_info(
    address: &str,
    port: u16,
) -> Result<crate::moonlight::infrastructure::gamestream::server_info::ServerInfo, MoonlightError> {
    let xml = simple_get(
        address,
        port,
        GameStreamScheme::Http,
        "/serverinfo",
        &[],
        None,
    )
    .await?;
    parse_server_info_response(&xml)
}

async fn resolve_pairing_ports(
    request: &PairHostRequest,
) -> Result<ResolvedPairingPorts, MoonlightError> {
    let mut http_candidates = vec![request.http_port, 47989];
    http_candidates.sort_unstable();
    http_candidates.dedup();

    let mut last_http_error = None;
    for &port in &http_candidates {
        match fetch_server_info(&request.address, port).await {
            Ok(info) => {
                return Ok(ResolvedPairingPorts {
                    http_port: port,
                    https_port: info.https_port.or(request.https_port).unwrap_or(47984),
                    app_version: info.app_version,
                });
            }
            Err(error) => {
                last_http_error = Some(format!("HTTP {port}: {error}"));
            }
        }
    }

    let mut https_candidates = vec![request.https_port.unwrap_or(47984), 47984, 47990];
    https_candidates.sort_unstable();
    https_candidates.dedup();

    for &port in &https_candidates {
        if let Ok(info) = fetch_server_info_https(&request.address, port).await {
            return Err(MoonlightError::Persistence(format!(
                "Sunshine responded on HTTPS port {port}, but the GameStream HTTP pairing port is unreachable. Embedded pairing requires HTTP /serverinfo and /pair on port 47989 (or the host's GameStream HTTP port). Last HTTP error: {}. Detected app version: {}",
                last_http_error.unwrap_or_else(|| "unknown HTTP failure".to_string()),
                info.app_version,
            )));
        }
    }

    Err(MoonlightError::Persistence(format!(
        "Unable to reach Sunshine GameStream pairing endpoints on {}. Tried HTTP ports {:?} and HTTPS ports {:?}. Last HTTP error: {}",
        request.address,
        http_candidates,
        https_candidates,
        last_http_error.unwrap_or_else(|| "unknown HTTP failure".to_string())
    )))
}

async fn fetch_server_info_https(
    address: &str,
    port: u16,
) -> Result<crate::moonlight::infrastructure::gamestream::server_info::ServerInfo, MoonlightError> {
    let xml = simple_get(
        address,
        port,
        GameStreamScheme::Https,
        "/serverinfo",
        &[],
        None,
    )
    .await?;
    parse_server_info_response(&xml)
}

async fn unpair(address: &str, port: u16, unique_id: &str) -> Result<(), MoonlightError> {
    let params = vec![
        ("uniqueid".to_string(), unique_id.to_string()),
        ("uuid".to_string(), Uuid::new_v4().simple().to_string()),
    ];
    let _ = simple_get(
        address,
        port,
        GameStreamScheme::Http,
        "/unpair",
        &params,
        None,
    )
    .await?;
    Ok(())
}

async fn pair_request(
    address: &str,
    port: u16,
    unique_id: &str,
    params: &[(&str, String)],
    scheme: GameStreamScheme,
    identity_pem: Option<String>,
) -> Result<String, MoonlightError> {
    let mut full_params: Vec<(String, String)> = vec![
        ("uniqueid".to_string(), unique_id.to_string()),
        ("uuid".to_string(), Uuid::new_v4().simple().to_string()),
    ];
    full_params.extend(params.iter().map(|(k, v)| (k.to_string(), v.clone())));
    simple_get(address, port, scheme, "/pair", &full_params, identity_pem).await
}

async fn simple_get(
    address: &str,
    port: u16,
    scheme: GameStreamScheme,
    path: &str,
    params: &[(String, String)],
    identity_pem: Option<String>,
) -> Result<String, MoonlightError> {
    let base = format!(
        "{}://{}:{}{}",
        match scheme {
            GameStreamScheme::Http => "http",
            GameStreamScheme::Https => "https",
        },
        address,
        port,
        path
    );
    let mut url =
        Url::parse(&base).map_err(|error| MoonlightError::Validation(error.to_string()))?;
    {
        let mut qp = url.query_pairs_mut();
        for (k, v) in params {
            qp.append_pair(k, v);
        }
    }

    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .no_proxy();
    if matches!(scheme, GameStreamScheme::Https) {
        builder = builder.danger_accept_invalid_certs(true);
        if let Some(identity_pem) = identity_pem {
            let identity = Identity::from_pem(identity_pem.as_bytes())
                .map_err(|error| MoonlightError::Validation(error.to_string()))?;
            builder = builder.identity(identity);
        }
    }
    let client = builder
        .build()
        .map_err(|error| MoonlightError::Persistence(error.to_string()))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| MoonlightError::Persistence(error.to_string()))?;
    response
        .text()
        .await
        .map_err(|error| MoonlightError::Persistence(error.to_string()))
}

fn xml_value(xml: &str, tag: &str) -> Result<String, MoonlightError> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    if let Some(root) = document
        .descendants()
        .find(|node| node.has_tag_name("root"))
    {
        if let Some(status_code) = root.attribute("status_code") {
            if status_code != "200" {
                return Err(MoonlightError::Validation(format!(
                    "pair request failed status_code={status_code}"
                )));
            }
        }
    }
    document
        .descendants()
        .find(|node| node.has_tag_name(tag))
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| MoonlightError::Validation(format!("missing XML tag {tag}")))
}

fn ensure_paired(xml: &str, stage: u8) -> Result<(), MoonlightError> {
    let paired = xml_value(xml, "paired")?;
    if paired != "1" {
        return Err(MoonlightError::Validation(format!(
            "failed pairing at stage #{stage}"
        )));
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(value: &str) -> Result<Vec<u8>, MoonlightError> {
    if value.len() % 2 != 0 {
        return Err(MoonlightError::Validation(
            "hex string must have even length".to_string(),
        ));
    }
    let mut output = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    for index in (0..bytes.len()).step_by(2) {
        let high = decode_hex_nibble(bytes[index])?;
        let low = decode_hex_nibble(bytes[index + 1])?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

fn decode_hex_nibble(value: u8) -> Result<u8, MoonlightError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(MoonlightError::Validation("invalid hex string".to_string())),
    }
}
