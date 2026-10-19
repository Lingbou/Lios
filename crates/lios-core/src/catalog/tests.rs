#[allow(unused_imports)]
use super::*;

#[cfg(windows)]
#[test]
fn windows_reparse_tags_distinguish_links_from_other_tags() {
    use std::io;

    const REPARSE_ATTRIBUTE: u32 = 0x400;
    const SYMLINK_TAG: u32 = 0xA000000C;
    const MOUNT_POINT_TAG: u32 = 0xA0000003;
    const CLOUD_TAG: u32 = 0x9000001A;

    assert!(super::classify_windows_reparse_tag(REPARSE_ATTRIBUTE, || Ok(SYMLINK_TAG)).unwrap());
    assert!(
        super::classify_windows_reparse_tag(REPARSE_ATTRIBUTE, || Ok(MOUNT_POINT_TAG)).unwrap()
    );
    assert!(!super::classify_windows_reparse_tag(REPARSE_ATTRIBUTE, || Ok(CLOUD_TAG)).unwrap());
    let error = super::classify_windows_reparse_tag(REPARSE_ATTRIBUTE, || {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected tag query failure",
        ))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn source_kind_revalidation_rejects_path_changed_into_link() {
    use std::fs;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let expected = super::packable_path_kind(&source).unwrap().unwrap();
    fs::remove_dir(&source).unwrap();
    create_directory_link(&outside, &source);

    let error = super::ensure_packable_path_kind(&source, expected).unwrap_err();
    assert!(error.to_string().contains("source path changed"));
}

#[cfg(unix)]
fn create_directory_link(target: &std::path::Path, link: &std::path::Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn create_directory_link(target: &std::path::Path, link: &std::path::Path) {
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to create junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
