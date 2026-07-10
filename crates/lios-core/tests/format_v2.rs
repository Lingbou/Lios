use std::path::Path;

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use lios_core::{
    crypto::KeyFile,
    format_v2::{
        decrypt_compatible_v1_or_v2, decrypt_envelope_v2, encrypt_envelope_v2, parse_envelope_v2,
        EnvelopeAlgorithmV2, EnvelopeKindV2, ENVELOPE_MAGIC_V2, ENVELOPE_VERSION_V2,
    },
    LiosError,
};
use tempfile::tempdir;

fn fixed_key(path: &Path, byte: u8) -> KeyFile {
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [byte; 32]);
    std::fs::write(
        path,
        format!(
            "version: 2\nkdf: HKDF-SHA256\nalgorithm: XChaCha20-Poly1305\nmaster_key: {encoded}\n"
        ),
    )
    .unwrap();
    KeyFile::load_from_path(path).unwrap()
}

fn legacy_ciphertext_with_nonce(key_byte: u8, nonce: [u8; 12], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&[key_byte; 32]));
    let mut encrypted = nonce.to_vec();
    encrypted.extend_from_slice(
        &cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .unwrap(),
    );
    encrypted
}

#[test]
fn envelope_roundtrips_each_domain_and_exposes_metadata() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 7);
    let plaintext = b"v2 envelope payload";

    for kind in [
        EnvelopeKindV2::Catalog,
        EnvelopeKindV2::Manifest,
        EnvelopeKindV2::NodeDescriptor,
    ] {
        let encrypted = encrypt_envelope_v2(&key, kind, plaintext).unwrap();
        let metadata = parse_envelope_v2(&encrypted).unwrap();

        assert_eq!(metadata.version, ENVELOPE_VERSION_V2);
        assert_eq!(metadata.algorithm, EnvelopeAlgorithmV2::XChaCha20Poly1305);
        assert_eq!(metadata.kind, kind);
        assert_eq!(metadata.ciphertext_len, plaintext.len() + 16);
        assert_eq!(
            decrypt_envelope_v2(&key, kind, &encrypted).unwrap(),
            plaintext
        );
    }
}

#[test]
fn envelope_randomizes_nonce_and_rejects_wrong_key_or_kind() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 8);
    let wrong_key = fixed_key(&tmp.path().join("wrong-key"), 9);
    let first = encrypt_envelope_v2(&key, EnvelopeKindV2::Catalog, b"same").unwrap();
    let second = encrypt_envelope_v2(&key, EnvelopeKindV2::Catalog, b"same").unwrap();

    assert_ne!(first, second);
    assert!(decrypt_envelope_v2(&wrong_key, EnvelopeKindV2::Catalog, &first).is_err());
    assert!(decrypt_envelope_v2(&key, EnvelopeKindV2::Manifest, &first).is_err());
}

#[test]
fn envelope_rejects_malformed_truncated_and_unknown_headers() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 10);
    let valid = encrypt_envelope_v2(&key, EnvelopeKindV2::Catalog, b"payload").unwrap();

    for length in [0, ENVELOPE_MAGIC_V2.len(), valid.len() - 1] {
        assert!(parse_envelope_v2(&valid[..length]).is_err());
        assert!(decrypt_envelope_v2(&key, EnvelopeKindV2::Catalog, &valid[..length]).is_err());
    }

    let mut unknown_version = valid.clone();
    unknown_version[ENVELOPE_MAGIC_V2.len()] = 99;
    assert!(parse_envelope_v2(&unknown_version).is_err());

    let mut unknown_algorithm = valid.clone();
    unknown_algorithm[ENVELOPE_MAGIC_V2.len() + 1] = 99;
    assert!(parse_envelope_v2(&unknown_algorithm).is_err());

    let mut unknown_kind = valid.clone();
    unknown_kind[ENVELOPE_MAGIC_V2.len() + 2] = 99;
    assert!(parse_envelope_v2(&unknown_kind).is_err());

    let mut framed_chunk_kind = valid.clone();
    framed_chunk_kind[ENVELOPE_MAGIC_V2.len() + 2] = 2;
    assert!(matches!(
        parse_envelope_v2(&framed_chunk_kind),
        Err(LiosError::InvalidV2Format("unknown envelope kind"))
    ));

    let mut malformed_v2 = ENVELOPE_MAGIC_V2.to_vec();
    malformed_v2.extend_from_slice(&[0u8; 64]);
    assert!(decrypt_envelope_v2(&key, EnvelopeKindV2::Catalog, &malformed_v2).is_err());
}

#[test]
fn compatibility_dispatch_decrypts_legacy_without_magic_and_valid_v2() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let legacy_key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let legacy_ciphertext = std::fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap();
    let legacy_plaintext = std::fs::read(fixtures.join("legacy_catalog_v1.json")).unwrap();
    assert!(!legacy_ciphertext.starts_with(&ENVELOPE_MAGIC_V2));
    assert_eq!(
        decrypt_compatible_v1_or_v2(&legacy_key, EnvelopeKindV2::Catalog, &legacy_ciphertext,)
            .unwrap(),
        legacy_plaintext
    );

    let tmp = tempdir().unwrap();
    let v2_key = fixed_key(&tmp.path().join("v2.key"), 31);
    let v2_ciphertext =
        encrypt_envelope_v2(&v2_key, EnvelopeKindV2::Manifest, b"v2 manifest").unwrap();
    assert_eq!(
        decrypt_compatible_v1_or_v2(&v2_key, EnvelopeKindV2::Manifest, &v2_ciphertext,).unwrap(),
        b"v2 manifest"
    );
}

#[test]
fn compatibility_dispatch_never_falls_back_when_exact_v2_magic_is_present() {
    let tmp = tempdir().unwrap();
    let key_byte = 32;
    let key = fixed_key(&tmp.path().join("key"), key_byte);
    let plaintext = [0x41; 96];
    let malformed_headers = [
        [99, 0, 0, 0],
        [ENVELOPE_VERSION_V2, 99, EnvelopeKindV2::Catalog as u8, 0],
        [ENVELOPE_VERSION_V2, 1, 99, 0],
    ];

    for suffix in malformed_headers {
        let mut nonce = [0u8; 12];
        nonce[..ENVELOPE_MAGIC_V2.len()].copy_from_slice(&ENVELOPE_MAGIC_V2);
        nonce[ENVELOPE_MAGIC_V2.len()..].copy_from_slice(&suffix);
        let legacy = legacy_ciphertext_with_nonce(key_byte, nonce, &plaintext);
        let independently_decrypted = ChaCha20Poly1305::new(Key::from_slice(&[key_byte; 32]))
            .decrypt(Nonce::from_slice(&nonce), &legacy[12..])
            .unwrap();
        assert_eq!(independently_decrypted, plaintext);
        assert!(decrypt_compatible_v1_or_v2(&key, EnvelopeKindV2::Catalog, &legacy).is_err());
    }

    let mut corrupted_v2 =
        encrypt_envelope_v2(&key, EnvelopeKindV2::Catalog, b"authenticated").unwrap();
    *corrupted_v2.last_mut().unwrap() ^= 0x80;
    assert!(decrypt_compatible_v1_or_v2(&key, EnvelopeKindV2::Catalog, &corrupted_v2).is_err());
}
