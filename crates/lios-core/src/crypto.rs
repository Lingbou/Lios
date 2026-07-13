use std::fs;
use std::path::Path;

use base64::{engine::general_purpose::STANDARD, Engine};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::atomic::write_private_atomic_new;
use crate::{LiosError, Result};

const KEY_FILE_V1_ALGORITHM: &str = "XChaCha20-Poly1305";
const KEY_FILE_V1_KDF: &str = "HKDF-SHA256";

#[derive(Clone)]
pub struct KeyFile {
    key: [u8; 32],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFileYaml {
    version: u8,
    algorithm: String,
    kdf: String,
    master_key: String,
}

#[derive(Serialize)]
struct KeyFileYamlV1 {
    version: u8,
    kdf: &'static str,
    algorithm: &'static str,
    master_key: String,
}

#[derive(Clone, Copy)]
pub(crate) enum KeyDomainV1 {
    Catalog,
    Chunk,
    Manifest,
    NodeDescriptor,
}

impl KeyDomainV1 {
    fn info(self) -> &'static [u8] {
        match self {
            Self::Catalog => b"lios/v1/catalog",
            Self::Chunk => b"lios/v1/chunk",
            Self::Manifest => b"lios/v1/manifest",
            Self::NodeDescriptor => b"lios/v1/node-descriptor",
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
        let parsed: KeyFileYaml =
            serde_yaml::from_str(&text).map_err(|_| LiosError::InvalidKeyFile)?;
        if parsed.version != 1
            || parsed.algorithm != KEY_FILE_V1_ALGORITHM
            || parsed.kdf != KEY_FILE_V1_KDF
        {
            return Err(LiosError::InvalidKeyFile);
        }
        let decoded = STANDARD
            .decode(parsed.master_key)
            .map_err(|_| LiosError::InvalidKeyFile)?;
        let key: [u8; 32] = decoded.try_into().map_err(|_| LiosError::InvalidKeyFile)?;
        Ok(Self { key })
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let yaml = KeyFileYamlV1 {
            version: 1,
            kdf: KEY_FILE_V1_KDF,
            algorithm: KEY_FILE_V1_ALGORITHM,
            master_key: STANDARD.encode(self.key),
        };
        self.write_yaml(path.as_ref(), &yaml)
    }

    pub fn same_material(&self, other: &Self) -> bool {
        self.key == other.key
    }

    fn write_yaml(&self, path: &Path, yaml: &impl Serialize) -> Result<()> {
        let serialized = serde_yaml::to_string(yaml)?;
        write_private_atomic_new(path, serialized.as_bytes())?;
        Ok(())
    }

    pub(crate) fn derive_key_v1(&self, domain: KeyDomainV1) -> Result<[u8; 32]> {
        let mut derived = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.key)
            .expand(domain.info(), &mut derived)
            .map_err(|_| LiosError::Crypto)?;
        Ok(derived)
    }
}

#[cfg(test)]
mod tests {
    use super::KeyFile;

    #[test]
    fn v1_hkdf_matches_fixed_sha256_vectors() {
        let key = KeyFile { key: [0x42; 32] };
        let vectors = [
            (
                super::KeyDomainV1::Catalog,
                "988ca28971ba8edab0142f707e9ac6316210d7ea837861d03bb0e551f14ffe1d",
            ),
            (
                super::KeyDomainV1::Chunk,
                "bac24771718e060f0873d8c9d8378a14c7fe7f0e3c8394ad6c3230a59440cebc",
            ),
            (
                super::KeyDomainV1::Manifest,
                "46f342c43b731dae16f55824402360d44529bcd21a04ff4aa73f155aa963e862",
            ),
            (
                super::KeyDomainV1::NodeDescriptor,
                "b1cbd3f311e87b673ee46e46939c14e5ddb72561785d0a875a7cf2938e035921",
            ),
        ];

        for (domain, expected_hex) in vectors {
            assert_eq!(
                hex::encode(key.derive_key_v1(domain).unwrap()),
                expected_hex
            );
        }
    }

    #[test]
    fn v1_save_writes_the_only_supported_key_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let generated = tmp.path().join("generated.key");
        let saved = tmp.path().join("saved.key");

        let key = KeyFile::generate_to_path(&generated).unwrap();
        key.save_to_path(&saved).unwrap();

        let yaml: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(saved).unwrap()).unwrap();
        assert_eq!(yaml["version"].as_u64(), Some(1));
        assert_eq!(yaml["kdf"].as_str(), Some("HKDF-SHA256"));
        assert_eq!(yaml["algorithm"].as_str(), Some("XChaCha20-Poly1305"));
        assert_eq!(yaml["master_key"].as_str().map(str::len), Some(44));
        assert!(yaml.get("key").is_none());
    }
}
