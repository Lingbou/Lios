use std::cmp;
use std::io::{self, Read, Write};
use std::path::Path;

use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    Key, XChaCha20Poly1305, XNonce,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zstd::stream::raw::{Operation, OutBuffer};

use crate::atomic::SiblingTempFile;
use crate::crypto::{KeyDomainV1, KeyFile};
use crate::{LiosError, Result};

pub const MAX_FRAME_PLAINTEXT_V1: usize = 1024 * 1024;
pub const CHUNK_STREAM_HEADER_LEN_V1: usize = 8 + 3 + 4 + 32;
pub const CHUNK_FRAME_HEADER_LEN_V1: usize = 8 + 1 + 4 + 24;

const CHUNK_STREAM_MAGIC_V1: [u8; 8] = *b"LIOSCHK1";
const CHUNK_STREAM_VERSION_V1: u8 = 1;
const XCHACHA20_POLY1305_ID: u8 = 1;
const ZSTD_ID: u8 = 1;
const FINAL_FRAME_FLAG: u8 = 1;
const AEAD_TAG_LEN: usize = 16;
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkIdV1([u8; 32]);

impl ChunkIdV1 {
    pub fn random() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkStreamStatsV1 {
    pub original_bytes: u64,
    pub compressed_bytes: u64,
    pub encoded_bytes: u64,
    pub frames: u64,
    pub original_sha256: [u8; 32],
    pub encoded_sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkDecodeLimitsV1 {
    pub expected_original_bytes: u64,
    pub max_encoded_bytes: u64,
    pub max_frames: u64,
    pub max_zstd_window_log: u32,
}

impl ChunkDecodeLimitsV1 {
    pub fn for_chunk(expected_original_bytes: u64) -> Self {
        let max_encoded_bytes = expected_original_bytes.saturating_add(4 * 1024 * 1024);
        Self {
            expected_original_bytes,
            max_encoded_bytes,
            max_frames: max_encoded_bytes.div_ceil(MAX_FRAME_PLAINTEXT_V1 as u64) + 1,
            max_zstd_window_log: 27,
        }
    }
}

pub fn encode_chunk_stream_v1<R: Read, W: Write>(
    key_file: &KeyFile,
    chunk_id: ChunkIdV1,
    input: R,
    mut output: W,
) -> Result<ChunkStreamStatsV1> {
    let stream_header = chunk_stream_header(chunk_id);
    let mut encoded_writer = HashingWriter::new(&mut output);
    encoded_writer.write_all(&stream_header)?;

    let key = key_file.derive_key_v1(KeyDomainV1::Chunk)?;
    let frame_writer = FrameEncryptWriter::new(encoded_writer, key, stream_header);
    let mut encoder = zstd::stream::write::Encoder::new(frame_writer, ZSTD_LEVEL)?;
    let mut hashing_reader = HashingReader::new(input);
    io::copy(&mut hashing_reader, &mut encoder)?;
    let frame_writer = encoder.finish()?;
    let (encoded_writer, frame_stats) = frame_writer.finish()?;
    let (original_bytes, original_sha256) = hashing_reader.finish();
    let (encoded_bytes, encoded_sha256) = encoded_writer.finish();

    Ok(ChunkStreamStatsV1 {
        original_bytes,
        compressed_bytes: frame_stats.compressed_bytes,
        encoded_bytes,
        frames: frame_stats.frames,
        original_sha256,
        encoded_sha256,
    })
}

/// Internal streaming decoder for temporary files; errors may leave partial plaintext in `output`.
pub(crate) fn decode_chunk_stream_v1<R: Read, W: Write>(
    key_file: &KeyFile,
    expected_chunk_id: ChunkIdV1,
    input: R,
    output: W,
    limits: &ChunkDecodeLimitsV1,
) -> Result<ChunkStreamStatsV1> {
    ensure_encoded_budget(0, CHUNK_STREAM_HEADER_LEN_V1, limits.max_encoded_bytes)?;
    let mut encoded_reader = HashingReader::new(input);
    let mut stream_header = [0u8; CHUNK_STREAM_HEADER_LEN_V1];
    read_exact_v1(
        &mut encoded_reader,
        &mut stream_header,
        "truncated chunk stream header",
    )?;
    parse_chunk_stream_header(&stream_header, expected_chunk_id)?;

    let key = key_file.derive_key_v1(KeyDomainV1::Chunk)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let hashing_writer = HashingWriter::new(output);
    let mut decoder = CompletingZstdDecoder::new(
        hashing_writer,
        limits.expected_original_bytes,
        limits.max_zstd_window_log,
    )?;
    let mut expected_index = 0u64;
    let mut compressed_bytes = 0u64;
    let mut frames = 0u64;

    loop {
        if frames >= limits.max_frames {
            return Err(LiosError::DataCorruption(
                "chunk frame limit exceeded".to_string(),
            ));
        }
        ensure_encoded_budget(
            encoded_reader.bytes_read(),
            CHUNK_FRAME_HEADER_LEN_V1,
            limits.max_encoded_bytes,
        )?;
        let mut frame_header = [0u8; CHUNK_FRAME_HEADER_LEN_V1];
        read_exact_v1(
            &mut encoded_reader,
            &mut frame_header,
            "missing final chunk frame",
        )?;
        let frame = parse_frame_header(&frame_header)?;
        if frame.index != expected_index {
            return Err(LiosError::InvalidV1Format(
                "chunk frame index is out of order",
            ));
        }

        let ciphertext_len = frame
            .plaintext_len
            .checked_add(AEAD_TAG_LEN)
            .ok_or(LiosError::InvalidV1Format("chunk frame length overflow"))?;
        ensure_encoded_budget(
            encoded_reader.bytes_read(),
            ciphertext_len,
            limits.max_encoded_bytes,
        )?;
        let mut ciphertext = vec![0u8; ciphertext_len];
        read_exact_v1(
            &mut encoded_reader,
            &mut ciphertext,
            "truncated chunk frame",
        )?;
        let aad = frame_aad(&stream_header, &frame_header);
        let compressed = cipher
            .decrypt(
                XNonce::from_slice(&frame.nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| LiosError::Crypto)?;
        if compressed.len() != frame.plaintext_len {
            return Err(LiosError::InvalidV1Format(
                "invalid chunk frame plaintext length",
            ));
        }
        decoder.write_compressed(&compressed)?;
        if decoder.is_complete() && !frame.final_frame {
            return Err(LiosError::InvalidV1Format(
                "zstd stream ended before final chunk frame",
            ));
        }

        compressed_bytes += frame.plaintext_len as u64;
        frames += 1;
        expected_index = expected_index
            .checked_add(1)
            .ok_or(LiosError::InvalidV1Format("too many chunk frames"))?;

        if frame.final_frame {
            let mut trailing = [0u8; 1];
            if encoded_reader.read(&mut trailing)? != 0 {
                if encoded_reader.bytes_read() > limits.max_encoded_bytes {
                    return Err(LiosError::DataCorruption(
                        "encoded chunk byte limit exceeded".to_string(),
                    ));
                }
                return Err(LiosError::InvalidV1Format("trailing chunk frames or bytes"));
            }
            break;
        }
    }

    let hashing_writer = decoder.finish()?;
    let (original_bytes, original_sha256) = hashing_writer.finish();
    if original_bytes != limits.expected_original_bytes {
        return Err(LiosError::DataCorruption(
            "decoded chunk size does not match expected size".to_string(),
        ));
    }
    let (encoded_bytes, encoded_sha256) = encoded_reader.finish();
    Ok(ChunkStreamStatsV1 {
        original_bytes,
        compressed_bytes,
        encoded_bytes,
        frames,
        original_sha256,
        encoded_sha256,
    })
}

pub fn decode_chunk_stream_v1_to_path<R: Read>(
    key_file: &KeyFile,
    expected_chunk_id: ChunkIdV1,
    input: R,
    destination: impl AsRef<Path>,
    limits: ChunkDecodeLimitsV1,
) -> Result<ChunkStreamStatsV1> {
    let destination = destination.as_ref();
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", destination.display()),
        )
        .into());
    }

    let mut temp = SiblingTempFile::create(destination, ".lios-part")?;
    let stats =
        decode_chunk_stream_v1(key_file, expected_chunk_id, input, temp.file_mut(), &limits)?;
    temp.persist_new(destination)?;
    Ok(stats)
}

struct CompletingZstdDecoder<W> {
    decoder: zstd::stream::raw::Decoder<'static>,
    output: W,
    buffer: Vec<u8>,
    complete: bool,
    output_bytes: u64,
    max_output_bytes: u64,
}

impl<W: Write> CompletingZstdDecoder<W> {
    fn new(output: W, max_output_bytes: u64, max_window_log: u32) -> io::Result<Self> {
        let mut decoder = zstd::stream::raw::Decoder::new()?;
        decoder.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(max_window_log))?;
        Ok(Self {
            decoder,
            output,
            buffer: vec![0u8; zstd::zstd_safe::DCtx::out_size()],
            complete: false,
            output_bytes: 0,
            max_output_bytes,
        })
    }

    fn write_compressed(&mut self, mut compressed: &[u8]) -> Result<()> {
        if self.complete && !compressed.is_empty() {
            return Err(LiosError::DataCorruption(
                "trailing data after zstd stream".to_string(),
            ));
        }

        while !compressed.is_empty() {
            let status = self
                .decoder
                .run_on_buffers(compressed, &mut self.buffer)
                .map_err(|error| LiosError::DataCorruption(error.to_string()))?;
            if status.bytes_read == 0 && status.bytes_written == 0 {
                return Err(LiosError::DataCorruption(
                    "zstd decoder made no progress".to_string(),
                ));
            }
            self.ensure_output_budget(status.bytes_written)?;
            self.output
                .write_all(&self.buffer[..status.bytes_written])?;
            self.output_bytes += status.bytes_written as u64;
            compressed = &compressed[status.bytes_read..];
            self.complete = status.remaining == 0;
            if self.complete && !compressed.is_empty() {
                return Err(LiosError::DataCorruption(
                    "trailing data after zstd stream".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn finish(mut self) -> Result<W> {
        let (remaining, written) = {
            let mut output = OutBuffer::around(&mut self.buffer);
            let remaining = self
                .decoder
                .finish(&mut output, self.complete)
                .map_err(|error| LiosError::DataCorruption(error.to_string()))?;
            (remaining, output.pos())
        };
        self.ensure_output_budget(written)?;
        self.output.write_all(&self.buffer[..written])?;
        self.output_bytes += written as u64;
        if remaining != 0 {
            return Err(LiosError::DataCorruption(
                "incomplete zstd frame".to_string(),
            ));
        }
        self.output.flush()?;
        Ok(self.output)
    }

    fn ensure_output_budget(&self, additional: usize) -> Result<()> {
        if self
            .output_bytes
            .checked_add(additional as u64)
            .is_none_or(|bytes| bytes > self.max_output_bytes)
        {
            return Err(LiosError::DataCorruption(
                "decoded chunk size exceeds expected size".to_string(),
            ));
        }
        Ok(())
    }
}

fn ensure_encoded_budget(consumed: u64, additional: usize, maximum: u64) -> Result<()> {
    if consumed
        .checked_add(additional as u64)
        .is_none_or(|bytes| bytes > maximum)
    {
        return Err(LiosError::DataCorruption(
            "encoded chunk byte limit exceeded".to_string(),
        ));
    }
    Ok(())
}

fn chunk_stream_header(chunk_id: ChunkIdV1) -> [u8; CHUNK_STREAM_HEADER_LEN_V1] {
    let mut header = [0u8; CHUNK_STREAM_HEADER_LEN_V1];
    header[..8].copy_from_slice(&CHUNK_STREAM_MAGIC_V1);
    header[8] = CHUNK_STREAM_VERSION_V1;
    header[9] = XCHACHA20_POLY1305_ID;
    header[10] = ZSTD_ID;
    header[11..15].copy_from_slice(&(MAX_FRAME_PLAINTEXT_V1 as u32).to_le_bytes());
    header[15..].copy_from_slice(chunk_id.as_bytes());
    header
}

fn parse_chunk_stream_header(
    header: &[u8; CHUNK_STREAM_HEADER_LEN_V1],
    expected_chunk_id: ChunkIdV1,
) -> Result<()> {
    if header[..8] != CHUNK_STREAM_MAGIC_V1 {
        return Err(LiosError::InvalidV1Format("invalid chunk stream magic"));
    }
    if header[8] != CHUNK_STREAM_VERSION_V1 {
        return Err(LiosError::InvalidV1Format("unknown chunk stream version"));
    }
    if header[9] != XCHACHA20_POLY1305_ID {
        return Err(LiosError::InvalidV1Format("unknown chunk stream algorithm"));
    }
    if header[10] != ZSTD_ID {
        return Err(LiosError::InvalidV1Format(
            "unknown chunk stream compression",
        ));
    }
    let frame_limit = u32::from_le_bytes(header[11..15].try_into().unwrap()) as usize;
    if frame_limit != MAX_FRAME_PLAINTEXT_V1 {
        return Err(LiosError::InvalidV1Format("unsupported chunk frame limit"));
    }
    if header[15..] != expected_chunk_id.0 {
        return Err(LiosError::InvalidV1Format("unexpected chunk id"));
    }
    Ok(())
}

struct ParsedFrameHeader {
    index: u64,
    final_frame: bool,
    plaintext_len: usize,
    nonce: [u8; 24],
}

fn parse_frame_header(header: &[u8; CHUNK_FRAME_HEADER_LEN_V1]) -> Result<ParsedFrameHeader> {
    let index = u64::from_le_bytes(header[..8].try_into().unwrap());
    let final_frame = match header[8] {
        0 => false,
        FINAL_FRAME_FLAG => true,
        _ => return Err(LiosError::InvalidV1Format("unknown chunk frame flags")),
    };
    let plaintext_len = u32::from_le_bytes(header[9..13].try_into().unwrap()) as usize;
    if plaintext_len > MAX_FRAME_PLAINTEXT_V1 {
        return Err(LiosError::InvalidV1Format(
            "chunk frame exceeds maximum size",
        ));
    }
    let nonce = header[13..].try_into().unwrap();
    Ok(ParsedFrameHeader {
        index,
        final_frame,
        plaintext_len,
        nonce,
    })
}

fn frame_aad(
    stream_header: &[u8; CHUNK_STREAM_HEADER_LEN_V1],
    frame_header: &[u8; CHUNK_FRAME_HEADER_LEN_V1],
) -> [u8; CHUNK_STREAM_HEADER_LEN_V1 + CHUNK_FRAME_HEADER_LEN_V1] {
    let mut aad = [0u8; CHUNK_STREAM_HEADER_LEN_V1 + CHUNK_FRAME_HEADER_LEN_V1];
    aad[..CHUNK_STREAM_HEADER_LEN_V1].copy_from_slice(stream_header);
    aad[CHUNK_STREAM_HEADER_LEN_V1..].copy_from_slice(frame_header);
    aad
}

fn read_exact_v1<R: Read>(reader: &mut R, buffer: &mut [u8], message: &'static str) -> Result<()> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(LiosError::InvalidV1Format(message))
        }
        Err(error) => Err(error.into()),
    }
}

struct FrameEncryptWriter<W> {
    output: W,
    cipher: XChaCha20Poly1305,
    stream_header: [u8; CHUNK_STREAM_HEADER_LEN_V1],
    buffer: Vec<u8>,
    next_index: u64,
    compressed_bytes: u64,
    frames: u64,
}

struct FrameWriteStats {
    compressed_bytes: u64,
    frames: u64,
}

impl<W: Write> FrameEncryptWriter<W> {
    fn new(output: W, key: [u8; 32], stream_header: [u8; CHUNK_STREAM_HEADER_LEN_V1]) -> Self {
        Self {
            output,
            cipher: XChaCha20Poly1305::new(Key::from_slice(&key)),
            stream_header,
            buffer: Vec::with_capacity(MAX_FRAME_PLAINTEXT_V1),
            next_index: 0,
            compressed_bytes: 0,
            frames: 0,
        }
    }

    fn finish(mut self) -> Result<(W, FrameWriteStats)> {
        self.write_frame(true)?;
        self.output.flush()?;
        let stats = FrameWriteStats {
            compressed_bytes: self.compressed_bytes,
            frames: self.frames,
        };
        Ok((self.output, stats))
    }

    fn write_frame(&mut self, final_frame: bool) -> Result<()> {
        let plaintext_len = self.buffer.len();
        let plaintext_len_u32 = u32::try_from(plaintext_len)
            .map_err(|_| LiosError::InvalidV1Format("chunk frame exceeds maximum size"))?;
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut header = [0u8; CHUNK_FRAME_HEADER_LEN_V1];
        header[..8].copy_from_slice(&self.next_index.to_le_bytes());
        header[8] = u8::from(final_frame);
        header[9..13].copy_from_slice(&plaintext_len_u32.to_le_bytes());
        header[13..].copy_from_slice(&nonce);
        let aad = frame_aad(&self.stream_header, &header);
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &self.buffer,
                    aad: &aad,
                },
            )
            .map_err(|_| LiosError::Crypto)?;

        self.output.write_all(&header)?;
        self.output.write_all(&ciphertext)?;
        self.compressed_bytes += plaintext_len as u64;
        self.frames += 1;
        self.next_index = self
            .next_index
            .checked_add(1)
            .ok_or(LiosError::InvalidV1Format("too many chunk frames"))?;
        self.buffer.clear();
        Ok(())
    }
}

impl<W: Write> Write for FrameEncryptWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut consumed = 0;
        while consumed < bytes.len() {
            if self.buffer.len() == MAX_FRAME_PLAINTEXT_V1 {
                self.write_frame(false).map_err(io::Error::other)?;
            }
            let available = MAX_FRAME_PLAINTEXT_V1 - self.buffer.len();
            let take = cmp::min(available, bytes.len() - consumed);
            self.buffer
                .extend_from_slice(&bytes[consumed..consumed + take]);
            consumed += take;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
}

impl<R> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    fn finish(self) -> (u64, [u8; 32]) {
        (self.bytes, self.hasher.finalize().into())
    }

    fn bytes_read(&self) -> u64 {
        self.bytes
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        self.bytes += read as u64;
        Ok(read)
    }
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    fn finish(self) -> (u64, [u8; 32]) {
        (self.bytes, self.hasher.finalize().into())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use crate::LiosError;

    use super::CompletingZstdDecoder;

    #[derive(Debug)]
    struct RejectWrites;

    impl Write for RejectWrites {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected output failure",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn output_write_failure_remains_io_error() {
        let compressed = zstd::stream::encode_all(b"output error".as_slice(), 3).unwrap();
        let mut decoder = CompletingZstdDecoder::new(RejectWrites, 1024, 27).unwrap();

        let error = match decoder.write_compressed(&compressed) {
            Err(error) => error,
            Ok(()) => decoder.finish().unwrap_err(),
        };

        assert!(matches!(
            error,
            LiosError::Io(ref source) if source.kind() == io::ErrorKind::PermissionDenied
        ));
    }
}
