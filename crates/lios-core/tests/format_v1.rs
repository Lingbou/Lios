use std::path::Path;

use lios_core::{
    crypto::KeyFile,
    format_v1::{
        decrypt_envelope_v1, encrypt_envelope_v1, parse_envelope_v1, EnvelopeAlgorithmV1,
        EnvelopeKindV1, ENVELOPE_MAGIC_V1, ENVELOPE_VERSION_V1,
    },
    LiosError,
};
use tempfile::tempdir;

fn fixed_key(path: &Path, byte: u8) -> KeyFile {
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [byte; 32]);
    std::fs::write(
        path,
        format!(
            "version: 1\nkdf: HKDF-SHA256\nalgorithm: XChaCha20-Poly1305\nmaster_key: {encoded}\n"
        ),
    )
    .unwrap();
    KeyFile::load_from_path(path).unwrap()
}

#[test]
fn envelope_roundtrips_each_domain_and_exposes_metadata() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 7);
    let plaintext = b"v1 envelope payload";

    for kind in [
        EnvelopeKindV1::Catalog,
        EnvelopeKindV1::Manifest,
        EnvelopeKindV1::NodeDescriptor,
    ] {
        let encrypted = encrypt_envelope_v1(&key, kind, plaintext).unwrap();
        let metadata = parse_envelope_v1(&encrypted).unwrap();

        assert_eq!(metadata.version, ENVELOPE_VERSION_V1);
        assert_eq!(metadata.algorithm, EnvelopeAlgorithmV1::XChaCha20Poly1305);
        assert_eq!(metadata.kind, kind);
        assert_eq!(metadata.ciphertext_len, plaintext.len() + 16);
        assert_eq!(
            decrypt_envelope_v1(&key, kind, &encrypted).unwrap(),
            plaintext
        );
    }
}

#[test]
fn envelope_randomizes_nonce_and_rejects_wrong_key_or_kind() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 8);
    let wrong_key = fixed_key(&tmp.path().join("wrong-key"), 9);
    let first = encrypt_envelope_v1(&key, EnvelopeKindV1::Catalog, b"same").unwrap();
    let second = encrypt_envelope_v1(&key, EnvelopeKindV1::Catalog, b"same").unwrap();

    assert_ne!(first, second);
    assert!(decrypt_envelope_v1(&wrong_key, EnvelopeKindV1::Catalog, &first).is_err());
    assert!(decrypt_envelope_v1(&key, EnvelopeKindV1::Manifest, &first).is_err());
}

#[test]
fn envelope_rejects_malformed_truncated_and_unknown_headers() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 10);
    let valid = encrypt_envelope_v1(&key, EnvelopeKindV1::Catalog, b"payload").unwrap();

    for length in [0, ENVELOPE_MAGIC_V1.len(), valid.len() - 1] {
        assert!(parse_envelope_v1(&valid[..length]).is_err());
        assert!(decrypt_envelope_v1(&key, EnvelopeKindV1::Catalog, &valid[..length]).is_err());
    }

    let mut unknown_version = valid.clone();
    unknown_version[ENVELOPE_MAGIC_V1.len()] = 99;
    assert!(parse_envelope_v1(&unknown_version).is_err());

    let mut unknown_algorithm = valid.clone();
    unknown_algorithm[ENVELOPE_MAGIC_V1.len() + 1] = 99;
    assert!(parse_envelope_v1(&unknown_algorithm).is_err());

    let mut unknown_kind = valid.clone();
    unknown_kind[ENVELOPE_MAGIC_V1.len() + 2] = 99;
    assert!(parse_envelope_v1(&unknown_kind).is_err());

    let mut framed_chunk_kind = valid.clone();
    framed_chunk_kind[ENVELOPE_MAGIC_V1.len() + 2] = 2;
    assert!(matches!(
        parse_envelope_v1(&framed_chunk_kind),
        Err(LiosError::InvalidV1Format("unknown envelope kind"))
    ));

    let mut malformed_v1 = ENVELOPE_MAGIC_V1.to_vec();
    malformed_v1.extend_from_slice(&[0u8; 64]);
    assert!(decrypt_envelope_v1(&key, EnvelopeKindV1::Catalog, &malformed_v1).is_err());
}
