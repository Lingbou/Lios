use std::io::{self, Cursor, Write};
use std::ops::Range;
use std::path::Path;

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    Key, XChaCha20Poly1305, XNonce,
};
use hkdf::Hkdf;
use lios_core::{
    crypto::KeyFile,
    framed_v1::{
        decode_chunk_stream_v1_to_path, encode_chunk_stream_v1, ChunkDecodeLimitsV1, ChunkIdV1,
        ChunkStreamStatsV1, CHUNK_FRAME_HEADER_LEN_V1, CHUNK_STREAM_HEADER_LEN_V1,
        MAX_FRAME_PLAINTEXT_V1,
    },
    LiosError,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const TAG_LEN: usize = 16;

fn authenticated_single_frame_stream(
    master_key_byte: u8,
    chunk_id: ChunkIdV1,
    compressed: &[u8],
) -> Vec<u8> {
    authenticated_stream(master_key_byte, chunk_id, &[(compressed.to_vec(), true)])
}

fn authenticated_stream(
    master_key_byte: u8,
    chunk_id: ChunkIdV1,
    frames: &[(Vec<u8>, bool)],
) -> Vec<u8> {
    let mut stream_header = [0u8; CHUNK_STREAM_HEADER_LEN_V1];
    stream_header[..8].copy_from_slice(b"LIOSCHK1");
    stream_header[8] = 1;
    stream_header[9] = 1;
    stream_header[10] = 1;
    stream_header[11..15].copy_from_slice(&(MAX_FRAME_PLAINTEXT_V1 as u32).to_le_bytes());
    stream_header[15..].copy_from_slice(chunk_id.as_bytes());

    let mut derived_key = [0u8; 32];
    Hkdf::<Sha256>::new(None, &[master_key_byte; 32])
        .expand(b"lios/v1/chunk", &mut derived_key)
        .unwrap();
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&derived_key));
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&stream_header);
    for (index, (compressed, final_frame)) in frames.iter().enumerate() {
        let nonce = [0x5a ^ index as u8; 24];
        let mut frame_header = [0u8; CHUNK_FRAME_HEADER_LEN_V1];
        frame_header[..8].copy_from_slice(&(index as u64).to_le_bytes());
        frame_header[8] = u8::from(*final_frame);
        frame_header[9..13].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        frame_header[13..].copy_from_slice(&nonce);
        let mut aad = Vec::with_capacity(stream_header.len() + frame_header.len());
        aad.extend_from_slice(&stream_header);
        aad.extend_from_slice(&frame_header);
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: compressed,
                    aad: &aad,
                },
            )
            .unwrap();
        encoded.extend_from_slice(&frame_header);
        encoded.extend_from_slice(&ciphertext);
    }
    encoded
}

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

fn incompressible(len: usize) -> Vec<u8> {
    let mut state = 0x6a09_e667_f3bc_c909u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn frame_ranges(encoded: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut offset = CHUNK_STREAM_HEADER_LEN_V1;
    while offset < encoded.len() {
        let length_offset = offset + 9;
        let plaintext_len = u32::from_le_bytes(
            encoded[length_offset..length_offset + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let end = offset + CHUNK_FRAME_HEADER_LEN_V1 + plaintext_len + TAG_LEN;
        ranges.push(offset..end);
        offset = end;
    }
    assert_eq!(offset, encoded.len());
    ranges
}

fn assert_no_transactional_output(directory: &Path, destination: &Path) {
    assert!(!destination.exists());
    let leftovers = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".lios-part"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "leftover partials: {leftovers:?}");
}

fn decode_transactionally(
    key: &KeyFile,
    chunk_id: ChunkIdV1,
    encoded: &[u8],
    expected_original_bytes: u64,
) -> std::result::Result<(ChunkStreamStatsV1, Vec<u8>), LiosError> {
    let tmp = tempdir().unwrap();
    let destination = tmp.path().join("decoded.bin");
    let stats = decode_chunk_stream_v1_to_path(
        key,
        chunk_id,
        Cursor::new(encoded),
        &destination,
        ChunkDecodeLimitsV1::for_chunk(expected_original_bytes),
    )?;
    Ok((stats, std::fs::read(destination).unwrap()))
}

fn roundtrip(key: &KeyFile, chunk_id: ChunkIdV1, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut encoded = Vec::new();
    let encode_stats =
        encode_chunk_stream_v1(key, chunk_id, Cursor::new(input), &mut encoded).unwrap();
    let (decode_stats, decoded) =
        decode_transactionally(key, chunk_id, &encoded, input.len() as u64).unwrap();
    let expected_hash: [u8; 32] = Sha256::digest(input).into();
    let expected_encoded_hash: [u8; 32] = Sha256::digest(&encoded).into();

    assert_eq!(decoded, input);
    assert_eq!(encode_stats.original_bytes, input.len() as u64);
    assert_eq!(decode_stats.original_bytes, input.len() as u64);
    assert_eq!(encode_stats.original_sha256, expected_hash);
    assert_eq!(decode_stats.original_sha256, expected_hash);
    assert_eq!(encode_stats.encoded_sha256, expected_encoded_hash);
    assert_eq!(decode_stats.encoded_sha256, expected_encoded_hash);
    assert_eq!(encode_stats.compressed_bytes, decode_stats.compressed_bytes);
    assert_eq!(encode_stats.frames, decode_stats.frames);
    assert_eq!(encode_stats.encoded_bytes, encoded.len() as u64);
    assert_eq!(decode_stats.encoded_bytes, encoded.len() as u64);
    (encoded, decoded)
}

#[test]
fn chunk_stream_roundtrips_empty_and_small_inputs() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 21);
    let chunk_id = ChunkIdV1::from_bytes([3; 32]);

    roundtrip(&key, chunk_id, b"");
    roundtrip(&key, chunk_id, b"small chunk payload");
}

#[test]
fn chunk_stream_roundtrips_multiframe_incompressible_input_with_exact_hash() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 22);
    let input = incompressible(MAX_FRAME_PLAINTEXT_V1 * 2 + 321_123);
    let chunk_id = ChunkIdV1::from_bytes([4; 32]);
    let (encoded, _) = roundtrip(&key, chunk_id, &input);
    let ranges = frame_ranges(&encoded);

    assert!(ranges.len() >= 3);
    assert_eq!(
        ranges
            .iter()
            .filter(|range| encoded[range.start + 8] == 1)
            .count(),
        1
    );
    assert_eq!(encoded[ranges.last().unwrap().start + 8], 1);
    assert!(ranges.iter().all(|range| {
        let length_offset = range.start + 9;
        let len = u32::from_le_bytes(
            encoded[length_offset..length_offset + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        len <= MAX_FRAME_PLAINTEXT_V1
    }));
}

struct RejectOversizedWrites {
    bytes: Vec<u8>,
    max: usize,
}

impl Write for RejectOversizedWrites {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized write",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn chunk_stream_never_writes_an_oversized_encrypted_frame_payload() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 23);
    let input = incompressible(MAX_FRAME_PLAINTEXT_V1 * 2 + 99);
    let mut writer = RejectOversizedWrites {
        bytes: Vec::new(),
        max: MAX_FRAME_PLAINTEXT_V1 + TAG_LEN,
    };

    let stats = encode_chunk_stream_v1(
        &key,
        ChunkIdV1::from_bytes([5; 32]),
        Cursor::new(input),
        &mut writer,
    )
    .unwrap();

    assert!(stats.frames >= 3);
}

#[test]
fn chunk_stream_ciphertext_differs_across_runs() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 24);
    let chunk_id = ChunkIdV1::from_bytes([6; 32]);
    let input = b"same chunk payload";
    let mut first = Vec::new();
    let mut second = Vec::new();

    encode_chunk_stream_v1(&key, chunk_id, Cursor::new(input), &mut first).unwrap();
    encode_chunk_stream_v1(&key, chunk_id, Cursor::new(input), &mut second).unwrap();

    assert_ne!(first, second);
}

#[test]
fn chunk_stream_rejects_tampering_and_context_transplant() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 25);
    let wrong_key = fixed_key(&tmp.path().join("wrong-key"), 27);
    let chunk_id = ChunkIdV1::from_bytes([7; 32]);
    let mut encoded = Vec::new();
    encode_chunk_stream_v1(
        &key,
        chunk_id,
        Cursor::new(b"authenticated chunk"),
        &mut encoded,
    )
    .unwrap();

    let mut tampered = encoded.clone();
    *tampered.last_mut().unwrap() ^= 0x80;
    assert!(decode_transactionally(&key, chunk_id, &tampered, 19).is_err());
    assert!(decode_transactionally(&wrong_key, chunk_id, &encoded, 19).is_err());
    assert!(decode_transactionally(&key, ChunkIdV1::from_bytes([8; 32]), &encoded, 19).is_err());

    let other_chunk_id = ChunkIdV1::from_bytes([10; 32]);
    let mut other = Vec::new();
    encode_chunk_stream_v1(
        &key,
        other_chunk_id,
        Cursor::new(b"authenticated chunk"),
        &mut other,
    )
    .unwrap();
    let mut transplanted = encoded[..CHUNK_STREAM_HEADER_LEN_V1].to_vec();
    transplanted.extend_from_slice(&other[CHUNK_STREAM_HEADER_LEN_V1..]);
    assert!(decode_transactionally(&key, chunk_id, &transplanted, 19).is_err());
}

#[test]
fn chunk_stream_rejects_reordered_duplicate_truncated_and_trailing_frames() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 26);
    let chunk_id = ChunkIdV1::from_bytes([9; 32]);
    let input = incompressible(MAX_FRAME_PLAINTEXT_V1 * 2 + 7654);
    let mut encoded = Vec::new();
    let expected_original_bytes = input.len() as u64;
    encode_chunk_stream_v1(&key, chunk_id, Cursor::new(input), &mut encoded).unwrap();
    let ranges = frame_ranges(&encoded);
    assert!(ranges.len() >= 3);

    let mut reordered = encoded[..CHUNK_STREAM_HEADER_LEN_V1].to_vec();
    reordered.extend_from_slice(&encoded[ranges[1].clone()]);
    reordered.extend_from_slice(&encoded[ranges[0].clone()]);
    for range in ranges.iter().skip(2) {
        reordered.extend_from_slice(&encoded[range.clone()]);
    }

    let mut duplicate = encoded[..CHUNK_STREAM_HEADER_LEN_V1].to_vec();
    duplicate.extend_from_slice(&encoded[ranges[0].clone()]);
    duplicate.extend_from_slice(&encoded[ranges[0].clone()]);
    duplicate.extend_from_slice(&encoded[ranges[1].start..]);

    let mut truncated = encoded.clone();
    truncated.pop();
    let mut trailing = encoded.clone();
    trailing.push(0);
    let mut missing_final = encoded.clone();
    missing_final[ranges.last().unwrap().start + 8] = 0;

    for invalid in [reordered, duplicate, truncated, trailing, missing_final] {
        assert!(decode_transactionally(&key, chunk_id, &invalid, expected_original_bytes).is_err());
    }
}

#[test]
fn chunk_stream_rejects_authenticated_truncated_zstd_stream() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 29);
    let chunk_id = ChunkIdV1::from_bytes([11; 32]);
    let mut compressed =
        zstd::stream::encode_all(b"zstd completion must be checked".as_slice(), 3).unwrap();
    compressed.pop();
    let encoded = authenticated_single_frame_stream(29, chunk_id, &compressed);

    let error = decode_transactionally(
        &key,
        chunk_id,
        &encoded,
        b"zstd completion must be checked".len() as u64,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        LiosError::DataCorruption(ref message) if message.contains("incomplete")
    ));
}

#[test]
fn transactional_decode_publishes_valid_output_without_clobbering() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 33);
    let chunk_id = ChunkIdV1::from_bytes([12; 32]);
    let plaintext = b"transactional decode";
    let destination = tmp.path().join("decoded.bin");
    let mut encoded = Vec::new();
    encode_chunk_stream_v1(&key, chunk_id, Cursor::new(plaintext), &mut encoded).unwrap();

    let stats = decode_chunk_stream_v1_to_path(
        &key,
        chunk_id,
        Cursor::new(&encoded),
        &destination,
        ChunkDecodeLimitsV1::for_chunk(plaintext.len() as u64),
    )
    .unwrap();

    assert_eq!(std::fs::read(&destination).unwrap(), plaintext);
    assert_eq!(stats.original_bytes, plaintext.len() as u64);
    assert!(std::fs::read_dir(tmp.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".lios-part")));

    std::fs::write(&destination, b"existing").unwrap();
    let error = decode_chunk_stream_v1_to_path(
        &key,
        chunk_id,
        Cursor::new(encoded),
        &destination,
        ChunkDecodeLimitsV1::for_chunk(plaintext.len() as u64),
    )
    .unwrap_err();
    assert!(
        matches!(error, LiosError::Io(ref source) if source.kind() == io::ErrorKind::AlreadyExists)
    );
    assert_eq!(std::fs::read(destination).unwrap(), b"existing");
}

#[test]
fn transactional_decode_failure_leaves_no_final_or_partial_output() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 34);
    let chunk_id = ChunkIdV1::from_bytes([13; 32]);
    let plaintext = incompressible(MAX_FRAME_PLAINTEXT_V1 + 1234);
    let mut encoded = Vec::new();
    encode_chunk_stream_v1(&key, chunk_id, Cursor::new(&plaintext), &mut encoded).unwrap();

    let mut tampered = encoded.clone();
    *tampered.last_mut().unwrap() ^= 0x80;
    let mut truncated = encoded.clone();
    truncated.pop();
    let mut trailing = encoded;
    trailing.push(0);

    for (index, invalid) in [tampered, truncated, trailing].into_iter().enumerate() {
        let destination = tmp.path().join(format!("failed-{index}.bin"));
        assert!(decode_chunk_stream_v1_to_path(
            &key,
            chunk_id,
            Cursor::new(invalid),
            &destination,
            ChunkDecodeLimitsV1::for_chunk(plaintext.len() as u64),
        )
        .is_err());
        assert_no_transactional_output(tmp.path(), &destination);
    }
}

#[test]
fn decode_limits_reject_authenticated_expansion_beyond_expected_size() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 35);
    let chunk_id = ChunkIdV1::from_bytes([14; 32]);
    let plaintext = vec![0u8; 2 * 1024 * 1024];
    let destination = tmp.path().join("expanded.bin");
    let mut encoded = Vec::new();
    encode_chunk_stream_v1(&key, chunk_id, Cursor::new(plaintext), &mut encoded).unwrap();

    let error = decode_chunk_stream_v1_to_path(
        &key,
        chunk_id,
        Cursor::new(encoded),
        &destination,
        ChunkDecodeLimitsV1::for_chunk(1024),
    )
    .unwrap_err();

    assert!(matches!(error, LiosError::DataCorruption(ref message) if message.contains("size")));
    assert_no_transactional_output(tmp.path(), &destination);
}

#[test]
fn decode_limits_reject_too_many_authenticated_frames_including_zero_length() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 36);
    let chunk_id = ChunkIdV1::from_bytes([15; 32]);
    let plaintext = b"frame limit";
    let compressed = zstd::stream::encode_all(plaintext.as_slice(), 3).unwrap();
    let encoded = authenticated_stream(36, chunk_id, &[(Vec::new(), false), (compressed, true)]);
    let destination = tmp.path().join("too-many-frames.bin");
    let mut limits = ChunkDecodeLimitsV1::for_chunk(plaintext.len() as u64);
    limits.max_frames = 1;

    let error =
        decode_chunk_stream_v1_to_path(&key, chunk_id, Cursor::new(encoded), &destination, limits)
            .unwrap_err();

    assert!(matches!(error, LiosError::DataCorruption(ref message) if message.contains("frame")));
    assert_no_transactional_output(tmp.path(), &destination);
}

#[test]
fn decode_limits_reject_encoded_byte_overrun_before_success() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 37);
    let chunk_id = ChunkIdV1::from_bytes([16; 32]);
    let plaintext = b"encoded byte limit";
    let destination = tmp.path().join("encoded-limit.bin");
    let mut encoded = Vec::new();
    encode_chunk_stream_v1(&key, chunk_id, Cursor::new(plaintext), &mut encoded).unwrap();
    let mut limits = ChunkDecodeLimitsV1::for_chunk(plaintext.len() as u64);
    limits.max_encoded_bytes = encoded.len() as u64 - 1;

    let error =
        decode_chunk_stream_v1_to_path(&key, chunk_id, Cursor::new(encoded), &destination, limits)
            .unwrap_err();

    assert!(matches!(error, LiosError::DataCorruption(ref message) if message.contains("encoded")));
    assert_no_transactional_output(tmp.path(), &destination);
}

#[test]
fn decode_limits_reject_authenticated_zstd_window_above_maximum() {
    let tmp = tempdir().unwrap();
    let key = fixed_key(&tmp.path().join("key"), 38);
    let chunk_id = ChunkIdV1::from_bytes([17; 32]);
    let plaintext = incompressible(2 * 1024 * 1024);
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    encoder.window_log(20).unwrap();
    encoder.include_contentsize(false).unwrap();
    encoder.write_all(&plaintext).unwrap();
    let compressed = encoder.finish().unwrap();
    let compressed_frames = compressed
        .chunks(MAX_FRAME_PLAINTEXT_V1)
        .enumerate()
        .map(|(index, bytes)| {
            (
                bytes.to_vec(),
                (index + 1) * MAX_FRAME_PLAINTEXT_V1 >= compressed.len(),
            )
        })
        .collect::<Vec<_>>();
    let encoded = authenticated_stream(38, chunk_id, &compressed_frames);
    let destination = tmp.path().join("window-limit.bin");
    let mut limits = ChunkDecodeLimitsV1::for_chunk(plaintext.len() as u64);
    limits.max_zstd_window_log = 19;

    let error =
        decode_chunk_stream_v1_to_path(&key, chunk_id, Cursor::new(encoded), &destination, limits)
            .unwrap_err();

    assert!(
        matches!(error, LiosError::DataCorruption(ref message) if message.contains("window") || message.contains("memory"))
    );
    assert_no_transactional_output(tmp.path(), &destination);
}
