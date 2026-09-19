use std::fs;
use std::io::Write;
use std::path::Path;

use super::paths::validate_target_path;

use crate::error::{Error, Result};
use crate::model::FileChange;

pub(crate) fn write_if_changed(path: &Path, contents: &[u8]) -> Result<FileChange> {
    validate_target_path(path)?;
    match fs::read(path) {
        Ok(existing) if existing == contents => Ok(FileChange::Unchanged),
        Ok(_) => {
            write_atomic(path, contents)?;
            Ok(FileChange::Updated)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_atomic(path, contents)?;
            Ok(FileChange::Created)
        }
        Err(source) => Err(Error::io("read file", path, source)),
    }
}

/// Stage beside the destination so replacement cannot expose a partially written file.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    validate_target_path(path)?;
    let parent = path.parent().ok_or_else(|| Error::PathHasNoParent {
        path: path.to_path_buf(),
    })?;
    fs::create_dir_all(parent)
        .map_err(|source| Error::io("create parent directory for", parent, source))?;
    let permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(Error::io("inspect file", path, source)),
    };
    if permissions
        .as_ref()
        .is_some_and(std::fs::Permissions::readonly)
    {
        return Err(Error::io(
            "replace file",
            path,
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "refusing to replace a read-only file",
            ),
        ));
    }
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|source| Error::io("create temporary file for", path, source))?;
    staged
        .write_all(contents)
        .map_err(|source| Error::io("write file", path, source))?;
    if let Some(permissions) = permissions {
        staged
            .as_file()
            .set_permissions(permissions)
            .map_err(|source| Error::io("set file permissions", path, source))?;
    }
    staged
        .as_file()
        .sync_all()
        .map_err(|source| Error::io("sync file", path, source))?;
    staged
        .persist(path)
        .map_err(|error| Error::io("replace file", path, error.error))?;
    Ok(())
}

pub(crate) fn remove_file_if_exists(path: &Path) -> Result<FileChange> {
    validate_target_path(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(FileChange::Removed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileChange::Absent),
        Err(source) => Err(Error::io("remove file", path, source)),
    }
}

pub(crate) fn file_exists(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::io("inspect file", path, source)),
    }
}

/// Discovery may follow symlinks, but must not read devices, sockets, or pipes.
/// Mutation callers validate their stricter non-symlink policy separately.
pub(crate) fn read_file_if_exists(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(Error::io(
                "read file",
                path,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "expected a regular file"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::io("inspect file", path, source)),
    }
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::io("read file", path, source)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::write_if_changed;
    use crate::model::FileChange;

    #[test]
    fn write_if_changed_distinguishes_created_updated_and_unchanged() {
        let temp_root = crate::tests::temp_dir("write-if-changed");
        let target = temp_root.join("file.txt");

        let created = write_if_changed(&target, b"one").expect("create should succeed");
        let unchanged = write_if_changed(&target, b"one").expect("unchanged write should succeed");
        let updated = write_if_changed(&target, b"two").expect("update should succeed");

        assert_eq!(created, FileChange::Created);
        assert_eq!(unchanged, FileChange::Unchanged);
        assert_eq!(updated, FileChange::Updated);
        assert_eq!(fs::read(&target).expect("target should exist"), b"two");
    }
}

#[cfg(all(test, unix))]
mod atomic_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn replacing_a_file_preserves_permissions_and_does_not_truncate_other_links() {
        let root = crate::tests::temp_dir("atomic-write");
        let path = root.join("rc");
        let old_link = root.join("old");
        fs::write(&path, b"old contents").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        fs::hard_link(&path, &old_link).unwrap();
        assert_eq!(
            write_if_changed(&path, b"new contents").unwrap(),
            FileChange::Updated
        );
        assert_eq!(fs::read(&old_link).unwrap(), b"old contents");
        assert_eq!(fs::read(&path).unwrap(), b"new contents");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn failed_replacement_keeps_the_target_and_removes_the_temporary_file() {
        let root = crate::tests::temp_dir("atomic-write-failure");
        let path = root.join("directory");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"keep").unwrap();
        assert!(write_atomic(&path, b"replacement").is_err());
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(root).unwrap().count(), 1);
    }

    #[test]
    fn special_files_are_rejected_before_io() {
        assert!(matches!(
            write_if_changed(Path::new("/dev/null"), b"data"),
            Err(Error::InvalidTargetPath { .. })
        ));
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn discovery_distinguishes_absent_files_from_invalid_ancestors() {
        let root = crate::tests::temp_dir("file-discovery");
        let file = root.join("file");
        assert!(!file_exists(&file).unwrap());
        assert_eq!(read_file_if_exists(&file).unwrap(), None);
        fs::write(&file, b"contents").unwrap();
        assert!(file_exists(&file).unwrap());
        assert!(file_exists(&file.join("child")).is_err());
        assert!(read_file_if_exists(&file.join("child")).is_err());
        assert!(read_file_if_exists(&root).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_discovery_rejects_a_fifo_before_reading_it() {
        let root = crate::tests::temp_dir("startup-fifo");
        let fifo = root.join(".bashrc");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let env = crate::infra::env::Environment::test().with_var("HOME", &root);
        let error = env.read_file_if_exists(&fifo).unwrap_err();
        assert!(
            matches!(error, Error::Io { source, .. } if source.kind() == std::io::ErrorKind::InvalidInput)
        );
    }
}

#[cfg(test)]
mod readonly_tests {
    use super::*;

    #[test]
    fn atomic_replacement_respects_readonly_files() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("profile");
        fs::write(&target, b"keep").unwrap();
        let original_permissions = fs::metadata(&target).unwrap().permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&target, readonly).unwrap();
        let same = write_if_changed(&target, b"keep");
        let changed = write_if_changed(&target, b"replace");
        // Restore permissions before assertions so Windows can also clean up the directory.
        fs::set_permissions(&target, original_permissions).unwrap();
        assert_eq!(same.unwrap(), FileChange::Unchanged);
        assert!(
            matches!(changed, Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::PermissionDenied)
        );
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
