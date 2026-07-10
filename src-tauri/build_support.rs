use std::path::Path;

pub fn resource_archive_link_arg(target_os: Option<&str>, out_dir: &Path) -> Option<String> {
    if target_os != Some("windows") {
        return None;
    }
    let archive = out_dir.join("libresource.a");
    archive.is_file().then(|| archive.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::resource_archive_link_arg;

    #[test]
    fn resource_link_requires_windows_target_and_existing_archive() {
        let temp = tempdir().unwrap();
        assert_eq!(resource_archive_link_arg(Some("linux"), temp.path()), None);
        assert_eq!(
            resource_archive_link_arg(Some("windows"), temp.path()),
            None
        );

        let archive = temp.path().join("libresource.a");
        fs::write(&archive, b"archive").unwrap();

        assert_eq!(
            resource_archive_link_arg(Some("windows"), temp.path()),
            Some(archive.display().to_string())
        );
    }
}
