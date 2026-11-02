use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use rand::Rng;

#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("Encryption key not configured")]
    KeyNotConfigured,
    #[error("Encryption failed")]
    EncryptionFailed,
    #[error("Decryption failed")]
    DecryptionFailed,
    #[error("Invalid ciphertext format")]
    InvalidFormat,
}

const NONCE_SIZE: usize = 12;

/// Encrypt data using AES-256-GCM
/// Returns: nonce || ciphertext || tag (concatenated)
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, EncryptionError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| EncryptionError::EncryptionFailed)?;

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);

    // Encrypt
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| EncryptionError::EncryptionFailed)?;

    // Concatenate: nonce || ciphertext
    let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);

    Ok(result)
}

/// Decrypt data encrypted with encrypt()
pub fn decrypt(key: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>, EncryptionError> {
    if ciphertext.len() < NONCE_SIZE + 16 {
        // Minimum: nonce + auth tag
        return Err(EncryptionError::InvalidFormat);
    }

    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| EncryptionError::DecryptionFailed)?;

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    nonce_bytes.copy_from_slice(&ciphertext[..NONCE_SIZE]);
    let nonce = Nonce::from(nonce_bytes);
    let encrypted = &ciphertext[NONCE_SIZE..];

    cipher
        .decrypt(&nonce, encrypted)
        .map_err(|_| EncryptionError::DecryptionFailed)
}

/// Text as it goes in a column beside an `encrypted` flag.
#[derive(Debug, PartialEq, Eq)]
pub struct Sealed {
    pub value: String,
    pub encrypted: i32,
}

/// Encrypt `value` when the instance has a key, or keep it as plaintext: the
/// key is optional, and a store shouldn't refuse a write that worked without it.
pub fn seal(key: Option<&[u8; 32]>, value: &str) -> Result<Sealed, EncryptionError> {
    Ok(match key {
        Some(key) => Sealed {
            value: BASE64.encode(encrypt(key, value.as_bytes())?),
            encrypted: 1,
        },
        None => Sealed {
            value: value.to_string(),
            encrypted: 0,
        },
    })
}

/// The text of a stored value, or `None` if it's ciphertext this instance can't
/// open.
pub fn open(key: Option<&[u8; 32]>, stored: &str, encrypted: bool) -> Option<String> {
    if !encrypted {
        return Some(stored.to_string());
    }
    let bytes = BASE64.decode(stored).ok()?;
    String::from_utf8(decrypt(key?, &bytes).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 32];
        let plaintext = b"hello world";

        let ciphertext = encrypt(&key, plaintext).unwrap();
        assert_ne!(&ciphertext[NONCE_SIZE..], plaintext);

        let decrypted = decrypt(&key, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_different_nonces() {
        let key = [0x42u8; 32];
        let plaintext = b"hello world";

        let ct1 = encrypt(&key, plaintext).unwrap();
        let ct2 = encrypt(&key, plaintext).unwrap();

        // Same plaintext should produce different ciphertext (different nonces)
        assert_ne!(ct1, ct2);

        // Both should decrypt correctly
        assert_eq!(decrypt(&key, &ct1).unwrap(), plaintext);
        assert_eq!(decrypt(&key, &ct2).unwrap(), plaintext);
    }

    #[test]
    fn test_invalid_ciphertext() {
        let key = [0x42u8; 32];

        // Too short
        assert!(matches!(
            decrypt(&key, &[0u8; 10]),
            Err(EncryptionError::InvalidFormat)
        ));

        // Corrupted
        let mut ciphertext = encrypt(&key, b"hello").unwrap();
        ciphertext[NONCE_SIZE] ^= 0xFF;
        assert!(matches!(
            decrypt(&key, &ciphertext),
            Err(EncryptionError::DecryptionFailed)
        ));
    }
}
