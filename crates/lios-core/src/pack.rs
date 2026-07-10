use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum PackSource {
    Path(PathBuf),
}

#[derive(Clone, Debug)]
pub struct PackOptions {
    pub chunk_size: usize,
    pub staging_dir: PathBuf,
}

impl PackOptions {
    pub const DEFAULT_CHUNK_SIZE: usize = 128 * 1024 * 1024;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackProgress {
    pub completed_chunks: u64,
    pub total_chunks: u64,
    pub completed_bytes: u64,
    pub total_bytes: u64,
}
