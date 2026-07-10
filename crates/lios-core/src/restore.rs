use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum RestoreConflictPolicy {
    Rename,
}

#[derive(Clone, Debug)]
pub struct RestoreOptions {
    pub output_dir: PathBuf,
    pub conflict_policy: RestoreConflictPolicy,
}
