use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_RSA_SHA256};
use rsa::{
    pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    RsaPrivateKey,
};
use time::{Duration, OffsetDateTime};
use x509_parser::{parse_x509_certificate, pem::parse_x509_pem};

use crate::moonlight::{
    domain::{ClientIdentity, MoonlightError, PersistedIdentity, SecretReference},
    infrastructure::{
        persistence::MoonlightStateRepository,
        secrets::{SecretBytes, SecretStore},
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentityBootstrapResult {
    pub identity: ClientIdentity,
    pub created: bool,
}

pub async fn bootstrap_client_identity<R, S>(
    state_repo: &R,
    secret_store: &S,
) -> Result<ClientIdentityBootstrapResult, MoonlightError>
where
    R: MoonlightStateRepository,
    S: SecretStore + ?Sized,
{
    let snapshot = state_repo.snapshot()?;
    match snapshot.identity {
        Some(identity) => {
            let client_identity = load_existing_identity(identity, secret_store).await?;
            Ok(ClientIdentityBootstrapResult {
                identity: client_identity,
                created: false,
            })
        }
        None => {
            let client_identity = generate_client_identity(secret_store).await?;
            let persisted = client_identity.persisted();
            state_repo.update(|configuration| {
                configuration.identity = Some(persisted.clone());
                Ok(())
            })?;
            Ok(ClientIdentityBootstrapResult {
                identity: client_identity,
                created: true,
            })
        }
    }
}

async fn load_existing_identity<S>(
    identity: PersistedIdentity,
    secret_store: &S,
) -> Result<ClientIdentity, MoonlightError>
where
    S: SecretStore + ?Sized,
{
    let private_key_bytes = secret_store
        .get(&identity.private_key_ref)
        .await?
        .ok_or_else(|| {
            MoonlightError::IdentityInvalid(
                "private key is missing for the persisted Moonlight identity; repair must be explicit because existing host pairings are no longer trustworthy".to_string(),
            )
        })?;

    let private_key_pem = String::from_utf8(private_key_bytes.0)
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
    validate_certificate_matches_private_key(&identity.certificate_pem, &private_key_pem)?;

    Ok(ClientIdentity {
        unique_id: identity.unique_id,
        client_name: identity.client_name,
        certificate_pem: identity.certificate_pem,
        private_key_ref: identity.private_key_ref,
        private_key_pem,
    })
}

async fn generate_client_identity<S>(secret_store: &S) -> Result<ClientIdentity, MoonlightError>
where
    S: SecretStore + ?Sized,
{
    let mut rng = rsa::rand_core::OsRng;
    let private_key = RsaPrivateKey::new(&mut rng, 2048)
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
    let private_key_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?
        .to_string();

    let mut params = CertificateParams::new(Vec::new())
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, "Noland Connect");
    params.distinguished_name = distinguished_name;
    params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(3650);

    let key_pair = KeyPair::from_pem_and_sign_algo(&private_key_pem, &PKCS_RSA_SHA256)
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
    let certificate_pem = cert.pem();

    let unique_id = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    let private_key_ref = SecretReference::new("os-keychain://noland/moonlight-client-key");
    secret_store
        .put(
            &private_key_ref,
            SecretBytes(private_key_pem.as_bytes().to_vec()),
        )
        .await?;

    Ok(ClientIdentity {
        unique_id,
        client_name: "Noland Connect".to_string(),
        certificate_pem,
        private_key_ref,
        private_key_pem,
    })
}

fn validate_certificate_matches_private_key(
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<(), MoonlightError> {
    let private_key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
    let public_key_der = private_key
        .to_public_key()
        .to_public_key_der()
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;

    let (_, pem) = parse_x509_pem(certificate_pem.as_bytes())
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;
    let (_, certificate) = parse_x509_certificate(&pem.contents)
        .map_err(|error| MoonlightError::IdentityInvalid(error.to_string()))?;

    if certificate.public_key().raw != public_key_der.as_ref() {
        return Err(MoonlightError::IdentityInvalid(
            "private key does not match the persisted certificate".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::bootstrap_client_identity;
    use crate::moonlight::infrastructure::{
        persistence::{JsonMoonlightStateRepository, MoonlightStateRepository},
        secrets::{testsupport::InMemorySecretStore, SecretStore},
    };

    fn temp_state_path(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "noland-moonlight-bootstrap-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&base).unwrap();
        base.join("state.json")
    }

    #[tokio::test]
    async fn generates_identity_on_first_run() {
        let repo = JsonMoonlightStateRepository::new(temp_state_path("first-run"));
        let secrets = InMemorySecretStore::default();

        let result = bootstrap_client_identity(&repo, &secrets).await.unwrap();
        assert!(result.created);
        assert!(repo.snapshot().unwrap().identity.is_some());
    }

    #[tokio::test]
    async fn reuses_identity_on_restart() {
        let repo = JsonMoonlightStateRepository::new(temp_state_path("restart"));
        let secrets = InMemorySecretStore::default();

        let first = bootstrap_client_identity(&repo, &secrets).await.unwrap();
        let second = bootstrap_client_identity(&repo, &secrets).await.unwrap();

        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.identity.unique_id, second.identity.unique_id);
    }

    #[tokio::test]
    async fn rejects_missing_private_key_for_existing_identity() {
        let repo = JsonMoonlightStateRepository::new(temp_state_path("missing-key"));
        let secrets = InMemorySecretStore::default();
        let created = bootstrap_client_identity(&repo, &secrets).await.unwrap();
        secrets
            .remove(&created.identity.private_key_ref)
            .await
            .unwrap();

        let error = bootstrap_client_identity(&repo, &secrets)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            crate::moonlight::domain::MoonlightError::IdentityInvalid(_)
        ));
    }
}
