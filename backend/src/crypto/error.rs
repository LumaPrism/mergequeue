//! Errors at the cryptographic boundary — ChaCha20-Poly1305 of the
//! `github_app` secret columns. Distinct from config errors (a missing key
//! is a config problem, surfaced in `config.rs`).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("invalid key length: expected {expected} bytes, got {actual}")]
    InvalidKeyLength { expected: usize, actual: usize },

    #[error("decryption failed: ciphertext could not be authenticated")]
    Decryption,

    #[error("encryption failed")]
    Encryption,

    #[error("failed to hex-decode ciphertext: {0}")]
    HexDecode(#[from] hex::FromHexError),

    #[error("decrypted payload is not valid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}
