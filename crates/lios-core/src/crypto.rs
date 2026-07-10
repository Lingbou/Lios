use std::fs;
use std::path::Path;

use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    AeadCore, ChaCha20Poly1305, Key, Nonce,
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::atomic::write_private_atomic_new;
use crate::{LiosError, Result};

type HmacSha256 = Hmac<Sha256>;

const KEY_FILE_V1_ALGORITHM: &str = "XChaCha20Poly1305-compatible-32-byte-key";
const KEY_FILE_V2_ALGORITHM: &str = "XChaCha20-Poly1305";
const KEY_FILE_V2_KDF: &str = "HKDF-SHA256";

#[derive(Clone)]
pub struct KeyFile {
    key: [u8; 32],
}

#[derive(Deserialize)]
struct KeyFileYaml {
    version: u8,
    algorithm: String,
    key: Option<String>,
    kdf: Option<String>,
    master_key: Option<String>,
}

#[derive(Serialize)]
struct KeyFileYamlV1 {
    version: u8,
    algorithm: &'static str,
    key: String,
}

#[allow(dead_code)]
#[derive(Serialize)]
struct KeyFileYamlV2 {
    version: u8,
    kdf: &'static str,
    algorithm: &'static str,
    master_key: String,
}

#[derive(Clone, Copy)]
pub(crate) enum KeyDomainV2 {
    Catalog,
    Chunk,
    Manifest,
    NodeDescriptor,
}

impl KeyDomainV2 {
    fn info(self) -> &'static [u8] {
        match self {
            Self::Catalog => b"lios/v2/catalog",
            Self::Chunk => b"lios/v2/chunk",
            Self::Manifest => b"lios/v2/manifest",
            Self::NodeDescriptor => b"lios/v2/node-descriptor",
        }
    }
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
        let encoded = match parsed.version {
            1 if parsed.algorithm == KEY_FILE_V1_ALGORITHM => parsed.key,
            2 if parsed.algorithm == KEY_FILE_V2_ALGORITHM
                && parsed.kdf.as_deref() == Some(KEY_FILE_V2_KDF) =>
            {
                parsed.master_key
            }
            _ => None,
        }
        .ok_or(LiosError::InvalidKeyFile)?;
        let decoded = STANDARD
            .decode(encoded)
            .map_err(|_| LiosError::InvalidKeyFile)?;
        let key: [u8; 32] = decoded.try_into().map_err(|_| LiosError::InvalidKeyFile)?;
        Ok(Self { key })
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let yaml = KeyFileYamlV1 {
            version: 1,
            algorithm: KEY_FILE_V1_ALGORITHM,
            key: STANDARD.encode(self.key),
        };
        self.write_yaml(path.as_ref(), &yaml)
    }

    #[allow(dead_code)]
    pub(crate) fn save_v2_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let yaml = KeyFileYamlV2 {
            version: 2,
            kdf: KEY_FILE_V2_KDF,
            algorithm: KEY_FILE_V2_ALGORITHM,
            master_key: STANDARD.encode(self.key),
        };
        self.write_yaml(path.as_ref(), &yaml)
    }

    fn write_yaml(&self, path: &Path, yaml: &impl Serialize) -> Result<()> {
        let serialized = serde_yaml::to_string(yaml)?;
        write_private_atomic_new(path, serialized.as_bytes())?;
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

    pub(crate) fn derive_key_v2(&self, domain: KeyDomainV2) -> Result<[u8; 32]> {
        let mut derived = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.key)
            .expand(domain.info(), &mut derived)
            .map_err(|_| LiosError::Crypto)?;
        Ok(derived)
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

#[cfg(test)]
mod tests {
    use super::KeyFile;

    const LEGACY_CATALOG: &[u8] =
        include_bytes!("../tests/fixtures/crypto_v1/legacy_catalog_v1.enc");
    const LEGACY_CATALOG_PLAIN: &[u8] =
        include_bytes!("../tests/fixtures/crypto_v1/legacy_catalog_v1.json");
    const LEGACY_CHUNK: &[u8] = include_bytes!("../tests/fixtures/crypto_v1/legacy_chunk_v1.enc");
    const LEGACY_CHUNK_PLAIN: &[u8] =
        include_bytes!("../tests/fixtures/crypto_v1/legacy_chunk_v1.bin");

    fn legacy_key() -> KeyFile {
        KeyFile::load_from_path(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/crypto_v1/legacy_v1.key"),
        )
        .unwrap()
    }

    #[test]
    fn golden_v1_catalog_ciphertext_remains_decryptable() {
        assert_eq!(
            legacy_key().decrypt(LEGACY_CATALOG).unwrap(),
            LEGACY_CATALOG_PLAIN
        );
    }

    #[test]
    fn golden_v1_chunk_ciphertext_and_deterministic_encryption_remain_stable() {
        let key = legacy_key();
        let compressed = key.decrypt(LEGACY_CHUNK).unwrap();

        assert_eq!(
            key.encrypt_deterministic("chunk", &compressed).unwrap(),
            LEGACY_CHUNK
        );
        assert_eq!(
            zstd::stream::decode_all(compressed.as_slice()).unwrap(),
            LEGACY_CHUNK_PLAIN
        );
    }

    #[test]
    fn v2_hkdf_matches_fixed_sha256_vectors() {
        let key = legacy_key();
        let vectors = [
            (
                super::KeyDomainV2::Catalog,
                "afee2ab2f1ed42e0203875b37ed6b16a4e137efec216426f380065ce0a466db7",
            ),
            (
                super::KeyDomainV2::Chunk,
                "bc43c2f4f1499565f43da259580966b02264e766305e60e1039254bba28af422",
            ),
            (
                super::KeyDomainV2::Manifest,
                "d72309e3aa6788f6ffc2dbc4965592b3d33c7b7261682dd297f4a90019168d7f",
            ),
            (
                super::KeyDomainV2::NodeDescriptor,
                "ea3b3042e5baa29413cee19c11d5572912512a44265853e764e9149fb6ad2a08",
            ),
        ];

        for (domain, expected_hex) in vectors {
            assert_eq!(
                hex::encode(key.derive_key_v2(domain).unwrap()),
                expected_hex
            );
        }
    }

    #[test]
    fn explicit_v2_save_writes_truthful_schema_for_phase_2b() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("phase-2b.key");

        legacy_key().save_v2_to_path(&path).unwrap();

        let yaml: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(yaml["version"].as_u64(), Some(2));
        assert_eq!(yaml["kdf"].as_str(), Some("HKDF-SHA256"));
        assert_eq!(yaml["algorithm"].as_str(), Some("XChaCha20-Poly1305"));
        assert_eq!(yaml["master_key"].as_str().map(str::len), Some(44));
        assert!(yaml.get("key").is_none());
    }
}
