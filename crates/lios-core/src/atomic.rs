use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

pub(crate) struct SiblingTempFile {
    file: Option<File>,
    path: PathBuf,
}

impl SiblingTempFile {
    pub(crate) fn create(destination: &Path, suffix: &str) -> io::Result<Self> {
        Self::create_with_privacy(destination, suffix, false)
    }

    fn create_private(destination: &Path, suffix: &str) -> io::Result<Self> {
        Self::create_with_privacy(destination, suffix, true)
    }

    fn create_with_privacy(destination: &Path, suffix: &str, private: bool) -> io::Result<Self> {
        let parent = destination_parent(destination);
        fs::create_dir_all(parent)?;
        let file_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");

        loop {
            let path = parent.join(format!(
                ".{file_name}.{}.{}",
                Uuid::new_v4().simple(),
                suffix.trim_start_matches('.')
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            if private {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    if private {
                        if let Err(error) = set_private_permissions(&file) {
                            drop(file);
                            let _ = fs::remove_file(&path);
                            return Err(error);
                        }
                    }
                    return Ok(Self {
                        file: Some(file),
                        path,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("temporary file is still open")
    }

    pub(crate) fn persist_replace(self, destination: &Path) -> io::Result<()> {
        self.persist_with(
            destination,
            Self::sync_and_close,
            replace_file,
            |path| fs::remove_file(path),
            sync_parent,
        )
    }

    pub(crate) fn persist_new(self, destination: &Path) -> io::Result<()> {
        self.persist_with(
            destination,
            Self::sync_and_close,
            publish_new_file,
            |path| fs::remove_file(path),
            sync_parent,
        )
    }

    fn sync_and_close(&mut self) -> io::Result<()> {
        let mut file = self.file.take().expect("temporary file is still open");
        file.flush()?;
        file.sync_all()?;
        drop(file);
        Ok(())
    }

    fn persist_with<Prepare, Publish, Cleanup, SyncParent>(
        mut self,
        destination: &Path,
        prepare: Prepare,
        publish: Publish,
        cleanup: Cleanup,
        sync_parent: SyncParent,
    ) -> io::Result<()>
    where
        Prepare: FnOnce(&mut Self) -> io::Result<()>,
        Publish: FnOnce(&Path, &Path) -> io::Result<()>,
        Cleanup: FnOnce(&Path) -> io::Result<()>,
        SyncParent: FnOnce(&Path) -> io::Result<()>,
    {
        prepare(&mut self)?;
        publish(&self.path, destination)?;
        // Publication is the commit point; later durability cleanup cannot roll it back.
        let _ = cleanup(&self.path);
        let _ = sync_parent(destination);
        Ok(())
    }
}

impl Drop for SiblingTempFile {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut temp = SiblingTempFile::create(path, ".lios-tmp")?;
    temp.file_mut().write_all(contents)?;
    temp.persist_replace(path)
}

pub(crate) fn write_atomic_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", path.display()),
        ));
    }
    let mut temp = SiblingTempFile::create(path, ".lios-tmp")?;
    temp.file_mut().write_all(contents)?;
    temp.persist_new(path)
}

pub(crate) fn write_private_atomic_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", path.display()),
        ));
    }
    let mut temp = SiblingTempFile::create_private(path, ".lios-tmp")?;
    temp.file_mut().write_all(contents)?;
    temp.persist_new(path)
}

pub(crate) fn write_atomic_immutable(path: &Path, contents: &[u8]) -> io::Result<()> {
    match fs::read(path) {
        Ok(existing) if existing == contents => return Ok(()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("immutable destination already exists: {}", path.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    match write_atomic_new(path, contents) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path)?;
            if existing == contents {
                Ok(())
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn publish_staged_new(source: &Path, destination: &Path) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(source)?
        .sync_all()?;
    publish_new_file(source, destination)?;
    let _ = fs::remove_file(source);
    let _ = sync_parent(destination);
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    move_file(source, destination, true)
}

#[cfg(not(windows))]
fn publish_new_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(source, destination)
}

#[cfg(windows)]
fn publish_new_file(source: &Path, destination: &Path) -> io::Result<()> {
    move_file(source, destination, false)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(destination_parent(path))?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn destination_parent(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

#[cfg(unix)]
fn set_private_permissions(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn move_file(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut flags = MOVEFILE_WRITE_THROUGH;
    if replace {
        flags |= MOVEFILE_REPLACE_EXISTING;
    }

    let moved = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::io::{self, Write};
    use std::path::Path;

    use tempfile::tempdir;

    use super::SiblingTempFile;

    #[test]
    fn bare_relative_destination_uses_current_directory_parent() {
        assert_eq!(
            super::destination_parent(Path::new("catalog.enc")),
            Path::new(".")
        );
        assert_eq!(
            super::destination_parent(Path::new("nested/catalog.enc")),
            Path::new("nested")
        );
    }

    #[test]
    fn preparation_failure_does_not_publish_destination() {
        let tmp = tempdir().unwrap();
        let destination = tmp.path().join("final.bin");
        let mut temp = SiblingTempFile::create(&destination, ".test").unwrap();
        temp.file_mut().write_all(b"unpublished").unwrap();
        let temp_path = temp.path.clone();
        let publish_called = Cell::new(false);

        let result = temp.persist_with(
            &destination,
            |_| Err(io::Error::other("injected preparation failure")),
            |_, _| {
                publish_called.set(true);
                Ok(())
            },
            |_| Ok(()),
            |_| Ok(()),
        );

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Other);
        assert!(!publish_called.get());
        assert!(!destination.exists());
        assert!(!temp_path.exists());
    }

    #[test]
    fn post_publication_failures_return_success_with_only_final_output() {
        let tmp = tempdir().unwrap();
        let destination = tmp.path().join("final.bin");
        let mut temp = SiblingTempFile::create(&destination, ".test").unwrap();
        temp.file_mut().write_all(b"published").unwrap();
        let temp_path = temp.path.clone();
        let cleanup_called = Cell::new(false);
        let sync_called = Cell::new(false);

        let result = temp.persist_with(
            &destination,
            SiblingTempFile::sync_and_close,
            |source, destination| fs::hard_link(source, destination),
            |_| {
                cleanup_called.set(true);
                Err(io::Error::other("injected cleanup failure"))
            },
            |_| {
                sync_called.set(true);
                Err(io::Error::other("injected parent sync failure"))
            },
        );

        assert!(result.is_ok());
        assert!(cleanup_called.get());
        assert!(sync_called.get());
        assert_eq!(fs::read(&destination).unwrap(), b"published");
        assert!(!temp_path.exists());
        let entries = fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![destination.file_name().unwrap()]);
    }

    #[test]
    fn atomic_new_does_not_clobber_existing_destination_on_disk() {
        let tmp = tempdir().unwrap();
        let destination = tmp.path().join("final.bin");
        fs::write(&destination, b"existing").unwrap();

        let result = super::write_atomic_new(&destination, b"replacement");

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[test]
    fn persist_new_does_not_clobber_destination_created_before_publish() {
        let tmp = tempdir().unwrap();
        let destination = tmp.path().join("final.bin");
        let mut temp = SiblingTempFile::create(&destination, ".test").unwrap();
        temp.file_mut().write_all(b"replacement").unwrap();
        let temp_path = temp.path.clone();
        fs::write(&destination, b"existing").unwrap();

        let result = temp.persist_new(&destination);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        assert!(!temp_path.exists());
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
}
