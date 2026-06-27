//! At-rest encryption of the `github_app` secret columns. ChaCha20-Poly1305
//! (AEAD): 256-bit key, random 96-bit nonce prepended to the ciphertext, the
//! whole envelope hex-encoded. Ported from Pica `common/src/crypto`, trimmed to
//! the AEAD core (no KMS versions, no HMAC/token helpers).

mod error;

pub use error::CryptoError;

use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use secrecy::{ExposeSecret, SecretBox};

const NONCE_LEN: usize = 12;

pub struct SecretCrypto {
    key: SecretBox<[u8; 32]>,
}

impl SecretCrypto {
    pub fn new(key: &[u8]) -> Result<Self, CryptoError> {
        let key: [u8; 32] = key.try_into().map_err(|_| CryptoError::InvalidKeyLength {
            expected: 32,
            actual: key.len(),
        })?;
        Ok(Self {
            key: SecretBox::new(Box::new(key)),
        })
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        let cipher = ChaCha20Poly1305::new(self.key.expose_secret().into());
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| CryptoError::Encryption)?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ct);
        Ok(hex::encode(out))
    }

    pub fn decrypt(&self, hexed: &str) -> Result<String, CryptoError> {
        let raw = hex::decode(hexed)?;
        if raw.len() < NONCE_LEN {
            return Err(CryptoError::Decryption);
        }
        let (nonce, ciphertext) = raw.split_at(NONCE_LEN);
        let cipher = ChaCha20Poly1305::new(self.key.expose_secret().into());
        let plaintext = cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| CryptoError::Decryption)?;
        Ok(String::from_utf8(plaintext)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"SaorcGejM8KgmKFsYjxKh22K5DhE2YO1"; // 32 bytes

    #[test]
    fn roundtrips() {
        let c = SecretCrypto::new(KEY).unwrap();
        let enc = c.encrypt("lorem_ipsum-dolor_sit-amet").unwrap();
        assert_eq!(c.decrypt(&enc).unwrap(), "lorem_ipsum-dolor_sit-amet");
    }

    #[test]
    fn wrong_key_fails() {
        let enc = SecretCrypto::new(KEY).unwrap().encrypt("secret").unwrap();
        let other = SecretCrypto::new(b"SaorcGejM8KgmKFsYjxKh22K5DhE2YO2").unwrap();
        assert!(other.decrypt(&enc).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let enc = SecretCrypto::new(KEY).unwrap().encrypt("secret").unwrap();
        let mut bytes = hex::decode(&enc).unwrap();
        bytes[NONCE_LEN] ^= 0xff; // flip a ciphertext byte past the nonce
        assert!(
            SecretCrypto::new(KEY)
                .unwrap()
                .decrypt(&hex::encode(bytes))
                .is_err()
        );
    }

    #[test]
    fn rejects_wrong_key_length() {
        assert!(SecretCrypto::new(b"too-short").is_err());
    }
}
