use std::fs;
use std::path::Path;

use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    AeadCore, ChaCha20Poly1305, Key, Nonce,
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::atomic::write_private_atomic_new;
use crate::{LiosError, Result};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct KeyFile {
    key: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct KeyFileYaml {
    version: u8,
    algorithm: String,
    key: String,
}

impl KeyFile {
    pub fn generate_to_path(path: impl AsRef<Path>) -> Result<Self> {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        let key_file = Self { key };
        key_file.save_to_path(path)?;
        Ok(key_file)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        let parsed: KeyFileYaml = serde_yaml::from_str(&text)?;
        if parsed.version != 1 || parsed.algorithm != "XChaCha20Poly1305-compatible-32-byte-key" {
            return Err(LiosError::InvalidKeyFile);
        }
        let decoded = STANDARD
            .decode(parsed.key)
            .map_err(|_| LiosError::InvalidKeyFile)?;
        let key: [u8; 32] = decoded.try_into().map_err(|_| LiosError::InvalidKeyFile)?;
        Ok(Self { key })
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let yaml = KeyFileYaml {
            version: 1,
            algorithm: "XChaCha20Poly1305-compatible-32-byte-key".to_string(),
            key: STANDARD.encode(self.key),
        };
        let serialized = serde_yaml::to_string(&yaml)?;
        write_private_atomic_new(path.as_ref(), serialized.as_bytes())?;
        Ok(())
    }

    pub(crate) fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| LiosError::Crypto)?;
        let mut output = Vec::with_capacity(nonce.len() + ciphertext.len());
        output.extend_from_slice(&nonce);
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    pub(crate) fn encrypt_deterministic(&self, domain: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        let digest = self.keyed_digest(domain.as_bytes(), plaintext)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        let nonce = Nonce::from_slice(&digest[..12]);
        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| LiosError::Crypto)?;
        let mut output = Vec::with_capacity(nonce.len() + ciphertext.len());
        output.extend_from_slice(nonce);
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    pub(crate) fn stable_id(&self, domain: &str, bytes: &[u8]) -> Result<String> {
        Ok(hex::encode(self.keyed_digest(domain.as_bytes(), bytes)?))
    }

    pub(crate) fn decrypt(&self, encrypted: &[u8]) -> Result<Vec<u8>> {
        if encrypted.len() < 12 {
            return Err(LiosError::Crypto);
        }
        let (nonce, ciphertext) = encrypted.split_at(12);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| LiosError::Crypto)
    }

    fn keyed_digest(&self, domain: &[u8], bytes: &[u8]) -> Result<[u8; 32]> {
        let mut mac =
            <HmacSha256 as Mac>::new_from_slice(&self.key).map_err(|_| LiosError::Crypto)?;
        mac.update(b"lios-v1");
        mac.update(&[0]);
        mac.update(domain);
        mac.update(&[0]);
        mac.update(bytes);
        Ok(mac.finalize().into_bytes().into())
    }
}
