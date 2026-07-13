use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    Key, XChaCha20Poly1305, XNonce,
};

use crate::crypto::{KeyDomainV1, KeyFile};
use crate::{LiosError, Result};

pub const ENVELOPE_MAGIC_V1: [u8; 8] = *b"LIOSENV1";
pub const ENVELOPE_VERSION_V1: u8 = 1;

const ENVELOPE_HEADER_LEN_V1: usize = ENVELOPE_MAGIC_V1.len() + 3 + 8 + 24;
const AEAD_TAG_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnvelopeAlgorithmV1 {
    XChaCha20Poly1305 = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnvelopeKindV1 {
    Catalog = 1,
    Manifest = 3,
    NodeDescriptor = 4,
}

impl EnvelopeKindV1 {
    pub(crate) fn key_domain(self) -> KeyDomainV1 {
        match self {
            Self::Catalog => KeyDomainV1::Catalog,
            Self::Manifest => KeyDomainV1::Manifest,
            Self::NodeDescriptor => KeyDomainV1::NodeDescriptor,
        }
    }

    fn from_id(id: u8) -> Result<Self> {
        match id {
            1 => Ok(Self::Catalog),
            3 => Ok(Self::Manifest),
            4 => Ok(Self::NodeDescriptor),
            _ => Err(LiosError::InvalidV1Format("unknown envelope kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeMetadataV1 {
    pub version: u8,
    pub algorithm: EnvelopeAlgorithmV1,
    pub kind: EnvelopeKindV1,
    pub nonce: [u8; 24],
    pub ciphertext_len: usize,
}

pub fn encrypt_envelope_v1(
    key_file: &KeyFile,
    kind: EnvelopeKindV1,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let key = key_file.derive_key_v1(kind.key_domain())?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext_len = plaintext
        .len()
        .checked_add(AEAD_TAG_LEN)
        .ok_or(LiosError::InvalidV1Format("envelope is too large"))?;
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

pub(crate) fn envelope_encoded_len_v1(plaintext_len: usize) -> Result<u64> {
    let encoded_len = ENVELOPE_HEADER_LEN_V1
        .checked_add(plaintext_len)
        .and_then(|len| len.checked_add(AEAD_TAG_LEN))
        .ok_or(LiosError::InvalidV1Format("envelope is too large"))?;
    u64::try_from(encoded_len).map_err(|_| LiosError::InvalidV1Format("envelope is too large"))
}

pub fn decrypt_envelope_v1(
    key_file: &KeyFile,
    expected_kind: EnvelopeKindV1,
    envelope: &[u8],
) -> Result<Vec<u8>> {
    let metadata = parse_envelope_v1(envelope)?;
    if metadata.kind != expected_kind {
        return Err(LiosError::UnexpectedV1Kind {
            expected: expected_kind as u8,
            actual: metadata.kind as u8,
        });
    }

    let key = key_file.derive_key_v1(metadata.kind.key_domain())?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(
            XNonce::from_slice(&metadata.nonce),
            Payload {
                msg: &envelope[ENVELOPE_HEADER_LEN_V1..],
                aad: &envelope[..ENVELOPE_HEADER_LEN_V1],
            },
        )
        .map_err(|_| LiosError::Crypto)
}

pub fn parse_envelope_v1(envelope: &[u8]) -> Result<EnvelopeMetadataV1> {
    if envelope.len() < ENVELOPE_HEADER_LEN_V1 + AEAD_TAG_LEN {
        return Err(LiosError::InvalidV1Format("truncated envelope"));
    }
    if envelope[..ENVELOPE_MAGIC_V1.len()] != ENVELOPE_MAGIC_V1 {
        return Err(LiosError::InvalidV1Format("invalid envelope magic"));
    }

    let version = envelope[ENVELOPE_MAGIC_V1.len()];
    if version != ENVELOPE_VERSION_V1 {
        return Err(LiosError::InvalidV1Format("unknown envelope version"));
    }
    let algorithm = match envelope[ENVELOPE_MAGIC_V1.len() + 1] {
        1 => EnvelopeAlgorithmV1::XChaCha20Poly1305,
        _ => return Err(LiosError::InvalidV1Format("unknown envelope algorithm")),
    };
    let kind = EnvelopeKindV1::from_id(envelope[ENVELOPE_MAGIC_V1.len() + 2])?;
    let length_offset = ENVELOPE_MAGIC_V1.len() + 3;
    let ciphertext_len = u64::from_le_bytes(
        envelope[length_offset..length_offset + 8]
            .try_into()
            .map_err(|_| LiosError::InvalidV1Format("truncated envelope length"))?,
    );
    let ciphertext_len = usize::try_from(ciphertext_len)
        .map_err(|_| LiosError::InvalidV1Format("envelope length is too large"))?;
    if ciphertext_len < AEAD_TAG_LEN
        || ENVELOPE_HEADER_LEN_V1.checked_add(ciphertext_len) != Some(envelope.len())
    {
        return Err(LiosError::InvalidV1Format("invalid envelope length"));
    }
    let nonce_offset = length_offset + 8;
    let nonce = envelope[nonce_offset..nonce_offset + 24]
        .try_into()
        .map_err(|_| LiosError::InvalidV1Format("truncated envelope nonce"))?;

    Ok(EnvelopeMetadataV1 {
        version,
        algorithm,
        kind,
        nonce,
        ciphertext_len,
    })
}

fn envelope_header(kind: EnvelopeKindV1, ciphertext_len: usize, nonce: &XNonce) -> Result<Vec<u8>> {
    let mut header = Vec::with_capacity(ENVELOPE_HEADER_LEN_V1);
    header.extend_from_slice(&ENVELOPE_MAGIC_V1);
    header.push(ENVELOPE_VERSION_V1);
    header.push(EnvelopeAlgorithmV1::XChaCha20Poly1305 as u8);
    header.push(kind as u8);
    header.extend_from_slice(
        &u64::try_from(ciphertext_len)
            .map_err(|_| LiosError::InvalidV1Format("envelope is too large"))?
            .to_le_bytes(),
    );
    header.extend_from_slice(nonce);
    Ok(header)
}
