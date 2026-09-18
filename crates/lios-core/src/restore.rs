use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct RestoreOptions {
    pub output_dir: PathBuf,
}
