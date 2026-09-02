//! XChaCha20-Poly1305 + HKDF. Master key never persisted on the disposable VM.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use noland_state_core::{Result, StateError};
use rand::RngCore;
use sha2::Sha256;

pub const MASTER_KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;

#[derive(Clone)]
pub struct MasterKey(pub [u8; MASTER_KEY_LEN]);

impl MasterKey {
    pub fn generate() -> Self {
        let mut key = [0u8; MASTER_KEY_LEN];
        rand::rngs::OsRng.fill_bytes(&mut key);
        Self(key)
    }

    pub fn from_bytes(bytes: [u8; MASTER_KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; MASTER_KEY_LEN] = bytes
            .try_into()
            .map_err(|_| StateError::Crypto("master key must be 32 bytes".into()))?;
        Ok(Self(arr))
    }
}

impl Drop for MasterKey {
    fn drop(&mut self) {
        for b in &mut self.0 {
            *b = 0;
        }
    }
}

#[derive(Clone)]
pub struct DerivedKeys {
    pub catalog: [u8; 32],
    pub manifest: [u8; 32],
}

pub fn derive_keys(master: &MasterKey) -> DerivedKeys {
    DerivedKeys {
        catalog: derive(master, b"noland-catalog-v1", b""),
        manifest: derive(master, b"noland-manifest-v1", b""),
    }
}

pub fn pack_key(master: &MasterKey, pack_id: &str) -> [u8; 32] {
    derive(master, b"noland-pack-v1", pack_id.as_bytes())
}

fn derive(master: &MasterKey, info: &[u8], salt: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), &master.0);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out).expect("hkdf expand");
    out
}

pub struct EncryptedBlob {
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

pub fn encrypt(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<EncryptedBlob> {
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|e| StateError::Crypto(e.to_string()))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| StateError::Crypto("encrypt failed".into()))?;
    Ok(EncryptedBlob {
        nonce: nonce_bytes,
        ciphertext,
    })
}

pub fn decrypt(key: &[u8; 32], aad: &[u8], blob: &EncryptedBlob) -> Result<Vec<u8>> {
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|e| StateError::Crypto(e.to_string()))?;
    cipher
        .decrypt(
            XNonce::from_slice(&blob.nonce),
            Payload {
                msg: &blob.ciphertext,
                aad,
            },
        )
        .map_err(|_| StateError::Integrity("decrypt/authenticate failed".into()))
}

pub fn wrap_envelope(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let blob = encrypt(key, aad, plaintext)?;
    let mut out = Vec::with_capacity(4 + NONCE_LEN + blob.ciphertext.len());
    out.extend_from_slice(b"NLNE");
    out.extend_from_slice(&blob.nonce);
    out.extend_from_slice(&blob.ciphertext);
    Ok(out)
}

pub fn unwrap_envelope(key: &[u8; 32], aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 4 + NONCE_LEN + 16 || &bytes[..4] != b"NLNE" {
        return Err(StateError::Integrity("invalid envelope".into()));
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[4..4 + NONCE_LEN]);
    decrypt(
        key,
        aad,
        &EncryptedBlob {
            nonce,
            ciphertext: bytes[4 + NONCE_LEN..].to_vec(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrong_key_fails() {
        let master = MasterKey::generate();
        let keys = derive_keys(&master);
        let blob = encrypt(&keys.manifest, b"manifest", b"secret-save").unwrap();
        assert_eq!(
            decrypt(&keys.manifest, b"manifest", &blob).unwrap(),
            b"secret-save"
        );
        let other = MasterKey::generate();
        let other_keys = derive_keys(&other);
        assert!(decrypt(&other_keys.manifest, b"manifest", &blob).is_err());
    }
}
