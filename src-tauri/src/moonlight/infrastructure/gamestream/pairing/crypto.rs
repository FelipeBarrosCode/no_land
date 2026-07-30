use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use rand_core::{OsRng, RngCore};
use rsa::{
    pkcs1v15::{Signature as RsaSignature, SigningKey, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey},
    signature::{RandomizedSigner, SignatureEncoding, Verifier},
    RsaPrivateKey, RsaPublicKey,
};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use x509_parser::{pem::parse_x509_pem, prelude::parse_x509_certificate};

use crate::moonlight::domain::MoonlightError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingHashAlgorithm {
    Sha1,
    Sha256,
}

impl PairingHashAlgorithm {
    pub fn for_server_major_version(major: u32) -> Self {
        if major >= 7 {
            Self::Sha256
        } else {
            Self::Sha1
        }
    }

    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha1 => Sha1::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
        }
    }

    pub fn digest_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
}

pub fn generate_random_bytes(length: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; length];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

pub fn salt_pin(salt: &[u8], pin: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(salt.len() + pin.len());
    output.extend_from_slice(salt);
    output.extend_from_slice(pin.as_bytes());
    output
}

pub fn derive_aes_key(salt: &[u8], pin: &str, hash: PairingHashAlgorithm) -> [u8; 16] {
    let digest = hash.digest(&salt_pin(salt, pin));
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

pub fn aes_ecb_encrypt(plaintext: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, MoonlightError> {
    if plaintext.len() % 16 != 0 {
        return Err(MoonlightError::Validation(
            "AES-ECB plaintext must be block aligned".to_string(),
        ));
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut output = plaintext.to_vec();
    for chunk in output.chunks_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
    }
    Ok(output)
}

pub fn aes_ecb_decrypt(ciphertext: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, MoonlightError> {
    if ciphertext.len() % 16 != 0 {
        return Err(MoonlightError::Validation(
            "AES-ECB ciphertext must be block aligned".to_string(),
        ));
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut output = ciphertext.to_vec();
    for chunk in output.chunks_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }
    Ok(output)
}

pub fn cert_signature_from_pem(pem: &str) -> Result<Vec<u8>, MoonlightError> {
    let (_, pem) = parse_x509_pem(pem.as_bytes())
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    let (_, cert) = parse_x509_certificate(&pem.contents)
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    Ok(cert.signature_value.data.to_vec())
}

pub fn sign_with_private_key_sha256(
    private_key_pem: &str,
    message: &[u8],
) -> Result<Vec<u8>, MoonlightError> {
    let private_key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    let signing_key = SigningKey::<Sha256>::new(private_key);
    let mut rng = OsRng;
    Ok(signing_key.sign_with_rng(&mut rng, message).to_vec())
}

pub fn verify_signature_sha256(
    server_certificate_pem: &str,
    data: &[u8],
    signature: &[u8],
) -> Result<bool, MoonlightError> {
    let (_, pem) = parse_x509_pem(server_certificate_pem.as_bytes())
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    let (_, cert) = parse_x509_certificate(&pem.contents)
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    let spki_der = cert.public_key().raw.to_vec();
    let public_key = RsaPublicKey::from_public_key_der(&spki_der)
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    let verifying_key = VerifyingKey::<Sha256>::new(public_key);
    let signature = RsaSignature::try_from(signature)
        .map_err(|error| MoonlightError::Validation(error.to_string()))?;
    Ok(verifying_key.verify(data, &signature).is_ok())
}

pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        aes_ecb_decrypt, aes_ecb_encrypt, derive_aes_key, generate_random_bytes,
        PairingHashAlgorithm,
    };

    #[test]
    fn aes_roundtrip() {
        let key = derive_aes_key(&[1; 16], "1234", PairingHashAlgorithm::Sha256);
        let data = generate_random_bytes(16);
        let enc = aes_ecb_encrypt(&data, &key).unwrap();
        let dec = aes_ecb_decrypt(&enc, &key).unwrap();
        assert_eq!(data, dec);
    }
}
