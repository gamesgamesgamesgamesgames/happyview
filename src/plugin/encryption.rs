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

/// The keys stored values open with: `TOKEN_ENCRYPTION_KEY`, which also seals
/// every new value, then `TOKEN_ENCRYPTION_KEY_PREVIOUS` while a rotation is
/// under way. There are no key ids: AES-GCM authenticates each value, so one
/// sealed under the other key fails cleanly and the next key is tried.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Keyring {
    current: [u8; 32],
    previous: Option<[u8; 32]>,
}

impl Keyring {
    /// A previous key equal to the current one adds nothing and is dropped.
    pub fn new(current: [u8; 32], previous: Option<[u8; 32]>) -> Self {
        Self {
            current,
            previous: previous.filter(|p| *p != current),
        }
    }

    /// The key that seals.
    pub fn current(&self) -> &[u8; 32] {
        &self.current
    }

    pub fn has_previous(&self) -> bool {
        self.previous.is_some()
    }

    /// Decrypt with the current key, then the previous one.
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        match decrypt(&self.current, ciphertext) {
            Err(EncryptionError::DecryptionFailed) => match &self.previous {
                Some(previous) => decrypt(previous, ciphertext),
                None => Err(EncryptionError::DecryptionFailed),
            },
            result => result,
        }
    }
}

impl Keyring {
    /// One key and no rotation under way.
    pub const fn single(current: [u8; 32]) -> Self {
        Self {
            current,
            previous: None,
        }
    }
}

impl From<[u8; 32]> for Keyring {
    fn from(current: [u8; 32]) -> Self {
        Self::single(current)
    }
}

impl std::fmt::Debug for Keyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyring")
            .field("previous", &self.previous.is_some())
            .finish_non_exhaustive()
    }
}

/// Text as it goes in a column beside an `encrypted` flag.
#[derive(Debug, PartialEq, Eq)]
pub struct Sealed {
    pub value: String,
    pub encrypted: i32,
}

/// Encrypt `value` when the instance has a key, or keep it as plaintext: the
/// key is optional, and a store shouldn't refuse a write that worked without it.
pub fn seal(keys: Option<&Keyring>, value: &str) -> Result<Sealed, EncryptionError> {
    Ok(match keys {
        Some(keys) => Sealed {
            value: BASE64.encode(encrypt(keys.current(), value.as_bytes())?),
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
pub fn open(keys: Option<&Keyring>, stored: &str, encrypted: bool) -> Option<String> {
    if !encrypted {
        return Some(stored.to_string());
    }
    let bytes = BASE64.decode(stored).ok()?;
    String::from_utf8(keys?.decrypt(&bytes).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: [u8; 32] = [0x11; 32];
    const NEW: [u8; 32] = [0x22; 32];

    #[test]
    fn a_keyring_opens_with_the_current_key_then_the_previous_one() {
        let keys = Keyring::new(NEW, Some(OLD));
        let under_old = encrypt(&OLD, b"old").unwrap();
        let under_new = encrypt(&NEW, b"new").unwrap();
        assert_eq!(keys.decrypt(&under_old).unwrap(), b"old");
        assert_eq!(keys.decrypt(&under_new).unwrap(), b"new");

        let other = encrypt(&[0x33; 32], b"other").unwrap();
        assert!(matches!(
            keys.decrypt(&other),
            Err(EncryptionError::DecryptionFailed)
        ));
        assert!(Keyring::single(NEW).decrypt(&under_old).is_err());
    }

    #[test]
    fn a_keyring_seals_with_the_current_key_only() {
        let keys = Keyring::new(NEW, Some(OLD));
        let sealed = seal(Some(&keys), "value").unwrap();
        assert_eq!(
            open(Some(&Keyring::single(NEW)), &sealed.value, true).as_deref(),
            Some("value")
        );
        assert_eq!(open(Some(&Keyring::single(OLD)), &sealed.value, true), None);
    }

    /// A malformed value is not a wrong key, so the previous key isn't tried.
    #[test]
    fn a_keyring_reports_a_malformed_value_as_such() {
        let keys = Keyring::new(NEW, Some(OLD));
        assert!(matches!(
            keys.decrypt(&[0u8; 10]),
            Err(EncryptionError::InvalidFormat)
        ));
    }

    #[test]
    fn a_previous_key_equal_to_the_current_one_is_dropped() {
        assert!(!Keyring::new(NEW, Some(NEW)).has_previous());
        assert!(Keyring::new(NEW, Some(OLD)).has_previous());
    }

    #[test]
    fn a_keyring_never_prints_its_keys() {
        let printed = format!("{:?}", Keyring::new(NEW, Some(OLD)));
        assert!(
            !printed.contains("34") && !printed.contains("17"),
            "{printed}"
        );
        assert!(printed.contains("previous: true"), "{printed}");
    }

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
