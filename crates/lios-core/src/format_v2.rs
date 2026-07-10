use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    Key, XChaCha20Poly1305, XNonce,
};

use crate::crypto::{KeyDomainV2, KeyFile};
use crate::{LiosError, Result};

pub const ENVELOPE_MAGIC_V2: [u8; 8] = *b"LIOSENV2";
pub const ENVELOPE_VERSION_V2: u8 = 2;

const ENVELOPE_HEADER_LEN_V2: usize = ENVELOPE_MAGIC_V2.len() + 3 + 8 + 24;
const AEAD_TAG_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnvelopeAlgorithmV2 {
    XChaCha20Poly1305 = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnvelopeKindV2 {
    Catalog = 1,
    Manifest = 3,
    NodeDescriptor = 4,
}

impl EnvelopeKindV2 {
    pub(crate) fn key_domain(self) -> KeyDomainV2 {
        match self {
            Self::Catalog => KeyDomainV2::Catalog,
            Self::Manifest => KeyDomainV2::Manifest,
            Self::NodeDescriptor => KeyDomainV2::NodeDescriptor,
        }
    }

    fn from_id(id: u8) -> Result<Self> {
        match id {
            1 => Ok(Self::Catalog),
            3 => Ok(Self::Manifest),
            4 => Ok(Self::NodeDescriptor),
            _ => Err(LiosError::InvalidV2Format("unknown envelope kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeMetadataV2 {
    pub version: u8,
    pub algorithm: EnvelopeAlgorithmV2,
    pub kind: EnvelopeKindV2,
    pub nonce: [u8; 24],
    pub ciphertext_len: usize,
}

pub fn encrypt_envelope_v2(
    key_file: &KeyFile,
    kind: EnvelopeKindV2,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let key = key_file.derive_key_v2(kind.key_domain())?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext_len = plaintext
        .len()
        .checked_add(AEAD_TAG_LEN)
        .ok_or(LiosError::InvalidV2Format("envelope is too large"))?;
    let header = envelope_header(kind, ciphertext_len, &nonce)?;
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &header,
            },
        )
        .map_err(|_| LiosError::Crypto)?;
    let mut envelope = Vec::with_capacity(header.len() + ciphertext.len());
    envelope.extend_from_slice(&header);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

pub fn decrypt_envelope_v2(
    key_file: &KeyFile,
    expected_kind: EnvelopeKindV2,
    envelope: &[u8],
) -> Result<Vec<u8>> {
    let metadata = parse_envelope_v2(envelope)?;
    if metadata.kind != expected_kind {
        return Err(LiosError::UnexpectedV2Kind {
            expected: expected_kind as u8,
            actual: metadata.kind as u8,
        });
    }

    let key = key_file.derive_key_v2(metadata.kind.key_domain())?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(
            XNonce::from_slice(&metadata.nonce),
            Payload {
                msg: &envelope[ENVELOPE_HEADER_LEN_V2..],
                aad: &envelope[..ENVELOPE_HEADER_LEN_V2],
            },
        )
        .map_err(|_| LiosError::Crypto)
}

pub fn decrypt_compatible_v1_or_v2(
    key_file: &KeyFile,
    expected_kind: EnvelopeKindV2,
    encrypted: &[u8],
) -> Result<Vec<u8>> {
    // Exact magic commits to v2; the negligible legacy nonce collision is an accepted no-downgrade tradeoff.
    if encrypted.starts_with(&ENVELOPE_MAGIC_V2) {
        decrypt_envelope_v2(key_file, expected_kind, encrypted)
    } else {
        key_file.decrypt(encrypted)
    }
}

pub fn parse_envelope_v2(envelope: &[u8]) -> Result<EnvelopeMetadataV2> {
    if envelope.len() < ENVELOPE_HEADER_LEN_V2 + AEAD_TAG_LEN {
        return Err(LiosError::InvalidV2Format("truncated envelope"));
    }
    if envelope[..ENVELOPE_MAGIC_V2.len()] != ENVELOPE_MAGIC_V2 {
        return Err(LiosError::InvalidV2Format("invalid envelope magic"));
    }

    let version = envelope[ENVELOPE_MAGIC_V2.len()];
    if version != ENVELOPE_VERSION_V2 {
        return Err(LiosError::InvalidV2Format("unknown envelope version"));
    }
    let algorithm = match envelope[ENVELOPE_MAGIC_V2.len() + 1] {
        1 => EnvelopeAlgorithmV2::XChaCha20Poly1305,
        _ => return Err(LiosError::InvalidV2Format("unknown envelope algorithm")),
    };
    let kind = EnvelopeKindV2::from_id(envelope[ENVELOPE_MAGIC_V2.len() + 2])?;
    let length_offset = ENVELOPE_MAGIC_V2.len() + 3;
    let ciphertext_len = u64::from_le_bytes(
        envelope[length_offset..length_offset + 8]
            .try_into()
            .map_err(|_| LiosError::InvalidV2Format("truncated envelope length"))?,
    );
    let ciphertext_len = usize::try_from(ciphertext_len)
        .map_err(|_| LiosError::InvalidV2Format("envelope length is too large"))?;
    if ciphertext_len < AEAD_TAG_LEN
        || ENVELOPE_HEADER_LEN_V2.checked_add(ciphertext_len) != Some(envelope.len())
    {
        return Err(LiosError::InvalidV2Format("invalid envelope length"));
    }
    let nonce_offset = length_offset + 8;
    let nonce = envelope[nonce_offset..nonce_offset + 24]
        .try_into()
        .map_err(|_| LiosError::InvalidV2Format("truncated envelope nonce"))?;

    Ok(EnvelopeMetadataV2 {
        version,
        algorithm,
        kind,
        nonce,
        ciphertext_len,
    })
}

fn envelope_header(kind: EnvelopeKindV2, ciphertext_len: usize, nonce: &XNonce) -> Result<Vec<u8>> {
    let mut header = Vec::with_capacity(ENVELOPE_HEADER_LEN_V2);
    header.extend_from_slice(&ENVELOPE_MAGIC_V2);
    header.push(ENVELOPE_VERSION_V2);
    header.push(EnvelopeAlgorithmV2::XChaCha20Poly1305 as u8);
    header.push(kind as u8);
    header.extend_from_slice(
        &u64::try_from(ciphertext_len)
            .map_err(|_| LiosError::InvalidV2Format("envelope is too large"))?
            .to_le_bytes(),
    );
    header.extend_from_slice(nonce);
    Ok(header)
}
