use ring::hmac;

// Re-export for downstream users
pub use ring::hmac::Key as MicKey;

use crate::packet::{AUTH_TAG_SIZE, HEADER_SIZE, HEADER_WITH_AUTH};

/// Derive a per-session authentication key using HKDF-SHA256.
pub fn derive_mic_key(session_secret: &[u8], session_id: u64) -> hmac::Key {
    let mut salt = Vec::with_capacity(8 + 22);
    salt.extend_from_slice(&session_id.to_be_bytes());
    salt.extend_from_slice(b"noland-microphone-v1");

    let salt_key = hmac::Key::new(hmac::HMAC_SHA256, &salt);
    let tag = hmac::sign(&salt_key, session_secret);
    hmac::Key::new(hmac::HMAC_SHA256, tag.as_ref())
}

/// Authenticate a packet by computing HMAC-SHA256 over the header (minus the
/// auth tag) and the Opus payload.
///
/// The computed tag (truncated to 16 bytes) is written into `auth_tag`.
pub fn authenticate_packet(
    key: &hmac::Key,
    header_without_auth: &[u8],
    opus_payload: &[u8],
    auth_tag: &mut [u8; AUTH_TAG_SIZE],
) {
    let mut ctx = hmac::Context::with_key(key);
    ctx.update(header_without_auth);
    ctx.update(opus_payload);
    let tag = ctx.sign();
    auth_tag.copy_from_slice(&tag.as_ref()[..AUTH_TAG_SIZE]);
}

/// Verify the authentication tag on a received packet.
pub fn verify_packet(
    key: &hmac::Key,
    header_without_auth: &[u8],
    opus_payload: &[u8],
    expected_tag: &[u8; AUTH_TAG_SIZE],
) -> bool {
    let mut ctx = hmac::Context::with_key(key);
    ctx.update(header_without_auth);
    ctx.update(opus_payload);
    let computed = ctx.sign();
    let computed_bytes = computed.as_ref();
    if computed_bytes.len() < AUTH_TAG_SIZE {
        return false;
    }
    // Constant-time comparison: XOR all bytes and check result is zero
    let mut acc: u8 = 0;
    for i in 0..AUTH_TAG_SIZE {
        acc |= computed_bytes[i] ^ expected_tag[i];
    }
    acc == 0
}

/// Parse and authenticate a raw packet buffer.
pub fn verify_raw_packet<'a>(key: &hmac::Key, buf: &'a [u8]) -> Option<&'a [u8]> {
    if buf.len() < HEADER_WITH_AUTH {
        return None;
    }

    let header = &buf[..HEADER_SIZE];
    let auth_tag: &[u8; AUTH_TAG_SIZE] = buf[HEADER_SIZE..HEADER_WITH_AUTH].try_into().ok()?;
    let payload = &buf[HEADER_WITH_AUTH..];

    if verify_packet(key, header, payload, auth_tag) {
        Some(buf)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_auth() {
        let secret = b"super-secret-session-key-1234";
        let session_id: u64 = 0xBEEF;
        let key = derive_mic_key(secret, session_id);

        let header = b"aaaaaaaaaaaaaaaaaaaaaaaaaa"; // 26 bytes
        let payload = b"opus-data-here";

        let mut tag = [0u8; AUTH_TAG_SIZE];
        authenticate_packet(&key, header, payload, &mut tag);

        assert!(verify_packet(&key, header, payload, &tag));
        assert!(!verify_packet(&key, header, b"wrong-payload", &tag));

        let mut tampered_header = *header;
        tampered_header[0] ^= 1;
        assert!(!verify_packet(&key, &tampered_header, payload, &tag));
    }

    #[test]
    fn different_session_different_key() {
        let secret = b"secret";
        let key1 = derive_mic_key(secret, 1);
        let key2 = derive_mic_key(secret, 2);

        let header = [0u8; HEADER_SIZE];
        let payload = b"data";

        let mut tag = [0u8; AUTH_TAG_SIZE];
        authenticate_packet(&key1, &header, payload, &mut tag);

        assert!(!verify_packet(&key2, &header, payload, &tag));
    }
}
