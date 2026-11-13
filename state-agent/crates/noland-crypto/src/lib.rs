//! XChaCha20-Poly1305 + HKDF. Master key never persisted on the disposable VM.

use chacha20poly1305::aead::{Aead, AeadInPlace, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use noland_state_core::{Result, StateError};
use rand::RngCore;
use sha2::Sha256;

pub const MASTER_KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
pub const ENVELOPE_HEADER_LEN: usize = 4 + NONCE_LEN;

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
    Decryptor::new(key)?.decrypt(&blob.nonce, aad, &blob.ciphertext)
}

/// Reusable cipher for independently authenticated records under one derived key.
pub struct Decryptor(XChaCha20Poly1305);

impl Decryptor {
    pub fn new(key: &[u8; 32]) -> Result<Self> {
        Ok(Self(
            XChaCha20Poly1305::new_from_slice(key)
                .map_err(|e| StateError::Crypto(e.to_string()))?,
        ))
    }

    /// Borrowed input requires only the output allocation, not a ciphertext clone.
    pub fn decrypt(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        self.0
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| StateError::Integrity("decrypt/authenticate failed".into()))
    }

    /// Authenticate the complete record before returning plaintext. The returned
    /// slice excludes the tag; on failure the supplied ciphertext buffer is cleared.
    pub fn decrypt_in_place<'a>(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        ciphertext: &'a mut [u8],
    ) -> Result<&'a mut [u8]> {
        let Some(length) = ciphertext.len().checked_sub(TAG_LEN) else {
            ciphertext.fill(0);
            return Err(StateError::Integrity("truncated authentication tag".into()));
        };
        let (message, tag) = ciphertext.split_at_mut(length);
        if self
            .0
            .decrypt_in_place_detached(
                XNonce::from_slice(nonce),
                aad,
                message,
                chacha20poly1305::Tag::from_slice(tag),
            )
            .is_err()
        {
            message.fill(0);
            tag.fill(0);
            return Err(StateError::Integrity("decrypt/authenticate failed".into()));
        }
        Ok(message)
    }
}

/// Seal a buffer whose first ENVELOPE_HEADER_LEN bytes are reserved for NLNE.
/// Producers can serialize/compress directly after that prefix without a copy.
pub fn seal_envelope_in_place(key: &[u8; 32], aad: &[u8], buffer: &mut Vec<u8>) -> Result<()> {
    if buffer.len() < ENVELOPE_HEADER_LEN {
        return Err(StateError::Invalid(
            "missing reserved envelope header".into(),
        ));
    }
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|e| StateError::Crypto(e.to_string()))?;
    let mut nonce = [0; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let tag = cipher
        .encrypt_in_place_detached(
            XNonce::from_slice(&nonce),
            aad,
            &mut buffer[ENVELOPE_HEADER_LEN..],
        )
        .map_err(|_| StateError::Crypto("encrypt failed".into()))?;
    buffer[..4].copy_from_slice(b"NLNE");
    buffer[4..ENVELOPE_HEADER_LEN].copy_from_slice(&nonce);
    buffer.extend_from_slice(&tag);
    Ok(())
}

pub fn wrap_envelope_owned(key: &[u8; 32], aad: &[u8], mut plaintext: Vec<u8>) -> Result<Vec<u8>> {
    let length = plaintext.len();
    plaintext.reserve(ENVELOPE_HEADER_LEN + TAG_LEN);
    plaintext.resize(length + ENVELOPE_HEADER_LEN, 0);
    plaintext.copy_within(..length, ENVELOPE_HEADER_LEN);
    seal_envelope_in_place(key, aad, &mut plaintext)?;
    Ok(plaintext)
}

pub fn wrap_envelope(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(ENVELOPE_HEADER_LEN + plaintext.len() + TAG_LEN);
    out.resize(ENVELOPE_HEADER_LEN, 0);
    out.extend_from_slice(plaintext);
    seal_envelope_in_place(key, aad, &mut out)?;
    Ok(out)
}

pub fn unwrap_envelope(key: &[u8; 32], aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    let nonce = envelope_nonce(bytes)?;
    Decryptor::new(key)?.decrypt(&nonce, aad, &bytes[ENVELOPE_HEADER_LEN..])
}

fn envelope_nonce(bytes: &[u8]) -> Result<[u8; NONCE_LEN]> {
    if bytes.len() < ENVELOPE_HEADER_LEN + TAG_LEN || &bytes[..4] != b"NLNE" {
        return Err(StateError::Integrity("invalid envelope".into()));
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[4..ENVELOPE_HEADER_LEN]);
    Ok(nonce)
}

pub fn unwrap_envelope_in_place<'a>(
    key: &[u8; 32],
    aad: &[u8],
    bytes: &'a mut [u8],
) -> Result<&'a mut [u8]> {
    let nonce = envelope_nonce(bytes)?;
    Decryptor::new(key)?.decrypt_in_place(&nonce, aad, &mut bytes[ENVELOPE_HEADER_LEN..])
}

pub fn unwrap_envelope_owned(key: &[u8; 32], aad: &[u8], mut bytes: Vec<u8>) -> Result<Vec<u8>> {
    let length = unwrap_envelope_in_place(key, aad, &mut bytes)?.len();
    bytes.copy_within(ENVELOPE_HEADER_LEN..ENVELOPE_HEADER_LEN + length, 0);
    bytes.truncate(length);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_and_in_place_envelopes_preserve_legacy_bytes() {
        let key = [7; 32];
        for plain in [b"".as_slice(), b"legacy document".as_slice()] {
            let blob = encrypt(&key, b"metadata", plain).unwrap();
            let legacy = [b"NLNE".as_slice(), &blob.nonce, &blob.ciphertext].concat();
            assert_eq!(
                unwrap_envelope_owned(&key, b"metadata", legacy).unwrap(),
                plain
            );
            let mut buffer = Vec::with_capacity(128);
            buffer.resize(ENVELOPE_HEADER_LEN, 0);
            buffer.extend_from_slice(plain);
            let address = buffer.as_ptr();
            seal_envelope_in_place(&key, b"metadata", &mut buffer).unwrap();
            assert_eq!(buffer.as_ptr(), address);
            let nonce = envelope_nonce(&buffer).unwrap();
            // Match the original AEAD algorithm byte for byte for the chosen nonce.
            let expected = XChaCha20Poly1305::new_from_slice(&key)
                .unwrap()
                .encrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: plain,
                        aad: b"metadata",
                    },
                )
                .unwrap();
            assert_eq!(&buffer[ENVELOPE_HEADER_LEN..], expected);
            assert_eq!(
                unwrap_envelope_in_place(&key, b"metadata", &mut buffer).unwrap(),
                plain
            );
        }
    }

    #[test]
    fn corrupt_tags_wrong_keys_and_aad_never_release_plaintext() {
        let key = [9; 32];
        let original = wrap_envelope_owned(&key, b"right", b"secret".to_vec()).unwrap();
        for case in 0..3 {
            let mut bytes = original.clone();
            if case == 0 {
                *bytes.last_mut().unwrap() ^= 1;
            }
            let key = if case == 1 { [8; 32] } else { key };
            let aad = if case == 2 {
                b"wrong".as_slice()
            } else {
                b"right".as_slice()
            };
            assert!(unwrap_envelope_in_place(&key, aad, &mut bytes).is_err());
            assert!(bytes[ENVELOPE_HEADER_LEN..].iter().all(|byte| *byte == 0));
        }
        for length in 0..ENVELOPE_HEADER_LEN + TAG_LEN {
            assert!(
                unwrap_envelope(&key, b"right", &original[..length.min(original.len())]).is_err()
            );
        }
    }

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
