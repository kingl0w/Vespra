use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::Argon2;
use rand::RngCore;

use crate::error::{AppError, AppResult};

const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;

fn derive_key(password: &[u8], salt: &[u8]) -> AppResult<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password, salt, &mut key)
        .map_err(|e| AppError::Encryption(format!("Key derivation failed: {e}")))?;
    Ok(key)
}

pub fn encrypt_key(private_key: &[u8], master_password: &str) -> AppResult<String> {
    let mut rng = rand::thread_rng();
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce_bytes);

    let key = derive_key(master_password.as_bytes(), &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| AppError::Encryption(format!("Cipher init failed: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, private_key)
        .map_err(|e| AppError::Encryption(format!("Encryption failed: {e}")))?;

    let mut blob = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);

    Ok(hex::encode(blob))
}

///decrypt a private key from hex-encoded blob.
pub fn decrypt_key(encrypted_hex: &str, master_password: &str) -> AppResult<Vec<u8>> {
    let blob = hex::decode(encrypted_hex)
        .map_err(|e| AppError::Decryption(format!("Invalid hex: {e}")))?;

    if blob.len() < SALT_LEN + NONCE_LEN + 1 {
        return Err(AppError::Decryption("Encrypted blob too short".into()));
    }

    let salt = &blob[..SALT_LEN];
    let nonce_bytes = &blob[SALT_LEN..SALT_LEN + NONCE_LEN];
    let ciphertext = &blob[SALT_LEN + NONCE_LEN..];

    let key = derive_key(master_password.as_bytes(), salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| AppError::Decryption(format!("Cipher init failed: {e}")))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| AppError::Decryption("Decryption failed — wrong master password?".into()))?;

    Ok(plaintext)
}

pub fn zeroize_bytes(data: &mut [u8]) {
    for byte in data.iter_mut() {
        *byte = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let private_key = b"this_is_a_32_byte_fake_priv_key!";
        let password = "test-master-password-2024";
        let encrypted = encrypt_key(private_key, password).unwrap();
        let decrypted = decrypt_key(&encrypted, password).unwrap();
        assert_eq!(decrypted, private_key);
    }

    #[test]
    fn test_wrong_password_fails() {
        let private_key = b"this_is_a_32_byte_fake_priv_key!";
        let encrypted = encrypt_key(private_key, "correct-password").unwrap();
        assert!(decrypt_key(&encrypted, "wrong-password").is_err());
    }
}
