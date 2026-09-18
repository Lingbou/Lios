#![allow(dead_code)]

use std::path::PathBuf;

use lios_core::catalog::{Catalog, ConflictResolution, PackOutcome, PackReport};
use lios_core::crypto::KeyFile;
use lios_core::pack::{PackOptions, PackProgress};
use lios_core::{LiosError, Result};

fn into_catalog(outcome: PackOutcome) -> Result<Catalog> {
    match outcome {
        PackOutcome::Packed { catalog, report } => {
            report.ensure_no_skipped_paths()?;
            Ok(catalog)
        }
        PackOutcome::Skipped { report } => {
            report.ensure_no_skipped_paths()?;
            Err(LiosError::Unsupported(
                "packing produced no catalog".to_string(),
            ))
        }
    }
}

pub trait CatalogPackTestExt {
    fn pack(source: PathBuf, key: &KeyFile, options: PackOptions) -> Result<Catalog> {
        Self::pack_with_progress_and_report(source, key, options, |_| {}).and_then(into_catalog)
    }

    fn pack_with_report(
        source: PathBuf,
        key: &KeyFile,
        options: PackOptions,
    ) -> Result<PackOutcome> {
        Self::pack_with_progress_and_report(source, key, options, |_| {})
    }

    fn pack_with_progress(
        source: PathBuf,
        key: &KeyFile,
        options: PackOptions,
        on_progress: impl FnMut(PackProgress),
    ) -> Result<Catalog> {
        Self::pack_with_progress_and_report(source, key, options, on_progress)
            .and_then(into_catalog)
    }

    fn pack_with_progress_and_report(
        source: PathBuf,
        key: &KeyFile,
        options: PackOptions,
        on_progress: impl FnMut(PackProgress),
    ) -> Result<PackOutcome>;
}

impl CatalogPackTestExt for Catalog {
    fn pack_with_progress_and_report(
        source: PathBuf,
        key: &KeyFile,
        options: PackOptions,
        on_progress: impl FnMut(PackProgress),
    ) -> Result<PackOutcome> {
        Catalog::pack_with_progress_and_report(source, key, options, on_progress)
    }
}

pub trait CatalogFolderTestExt {
    fn add_paths_to_folder(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
    ) -> Result<()>;

    fn add_paths_to_folder_with_report(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
    ) -> Result<PackReport>;

    fn add_paths_to_folder_with_progress(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        on_progress: impl FnMut(PackProgress),
    ) -> Result<()>;

    fn add_paths_to_folder_with_progress_and_report(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        on_progress: impl FnMut(PackProgress),
    ) -> Result<PackReport>;
}

impl CatalogFolderTestExt for Catalog {
    fn add_paths_to_folder(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
    ) -> Result<()> {
        self.add_paths_to_folder_with_report(parent_id, paths, resolutions, key, options)?
            .ensure_no_skipped_paths()
    }

    fn add_paths_to_folder_with_report(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
    ) -> Result<PackReport> {
        self.add_paths_to_folder_with_progress_and_report(
            parent_id,
            paths,
            resolutions,
            key,
            options,
            |_| {},
        )
    }

    fn add_paths_to_folder_with_progress(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        on_progress: impl FnMut(PackProgress),
    ) -> Result<()> {
        self.add_paths_to_folder_with_progress_and_report(
            parent_id,
            paths,
            resolutions,
            key,
            options,
            on_progress,
        )?
        .ensure_no_skipped_paths()
    }

    fn add_paths_to_folder_with_progress_and_report(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        on_progress: impl FnMut(PackProgress),
    ) -> Result<PackReport> {
        self.add_paths_to_folder_with_remote_inventory_and_progress_and_report(
            parent_id,
            paths,
            resolutions,
            key,
            options,
            &[],
            on_progress,
        )
    }
}
