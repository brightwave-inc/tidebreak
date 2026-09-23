//! The downloaded update, staged on disk until you restart to install it.
//!
//! An update archive runs to hundreds of megabytes, and it can wait hours or
//! days for the restart. Holding it in memory all that time would pin that
//! much memory, so the updater writes the verified archive to a file in the
//! app's cache directory and keeps only the file's path, length, and SHA-256
//! digest. The install reads the file back and refuses it unless it holds
//! exactly the bytes whose signature the updater verified.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The folder under the app's cache directory that holds staged updates.
/// Tidebreak owns everything in it, so a file there that is not the update
/// staged in this run is left over from an earlier one.
pub(crate) const STAGING_DIRECTORY: &str = "updates";

const STAGED_EXTENSION: &str = "update";

/// A verified update archive written to disk. It holds where the archive is
/// and what it must read back as, never the archive's bytes.
#[derive(Debug)]
pub(crate) struct StagedArchive {
    path: PathBuf,
    len: u64,
    digest: [u8; 32],
}

impl StagedArchive {
    /// Write `bytes` to a new file in `directory`. The caller has already
    /// verified the archive's signature, so what lands on disk is what the
    /// install may trust once [`Self::read`] proves it unchanged.
    ///
    /// Every staging gets a fresh file, so a newer release never overwrites
    /// the file an older staged update still points at.
    pub(crate) fn write(directory: &Path, bytes: &[u8]) -> io::Result<Self> {
        let path = directory.join(format!("{}.{STAGED_EXTENSION}", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let written = options.open(&path).and_then(|mut file| {
            file.write_all(bytes)?;
            file.flush()
        });
        if let Err(error) = written {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self {
            path,
            len: bytes.len() as u64,
            digest: Sha256::digest(bytes).into(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Read the archive back to install it. Refuses a file that is gone, or
    /// that no longer holds exactly the bytes that were verified and staged:
    /// the install itself checks no signature, so this is the check that
    /// stands between a changed file and the app bundle.
    pub(crate) fn read(&self) -> io::Result<Vec<u8>> {
        let file = File::open(&self.path)?;
        let capacity = usize::try_from(self.len).unwrap_or_default();
        let mut bytes = Vec::with_capacity(capacity);
        // One byte past the staged length is enough to tell the file grew,
        // without reading an arbitrarily large replacement into memory.
        file.take(self.len.saturating_add(1))
            .read_to_end(&mut bytes)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if bytes.len() as u64 != self.len || digest != self.digest {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the staged update changed after it was verified",
            ));
        }
        Ok(bytes)
    }

    /// Delete the file. A file that is already gone counts as deleted.
    pub(crate) fn remove(&self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// Create the staging folder, and delete every file an earlier run left in
/// it: an update that run installed, or one it never got to. Returns how many
/// files it deleted. Call it before anything is staged in this run, or it
/// deletes that too.
pub(crate) fn prepare_directory(directory: &Path) -> io::Result<usize> {
    ensure_directory(directory)?;
    let mut removed = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            continue;
        }
        match remove_entry(&entry.path(), file_type) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => eprintln!(
                "tidebreak-desktop: could not delete stale staged update {}: {error}",
                entry.path().display()
            ),
        }
    }
    Ok(removed)
}

/// Make sure the staging folder exists as a real folder that only you can
/// open. A symlink, or anything else, at its path is removed as an entry of
/// its own, and a real folder takes its place: following a link would lock
/// down and empty whatever it points to.
pub(crate) fn ensure_directory(directory: &Path) -> io::Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(metadata) => remove_entry(directory, metadata.file_type())?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Prove that `directory` takes a new file, before a download spends its
/// bandwidth on an archive that would have nowhere to go.
pub(crate) fn probe_writable(directory: &Path) -> io::Result<()> {
    let probe = directory.join(format!(".probe-{}", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = options
        .open(&probe)
        .and_then(|mut file| file.write_all(b"."));
    let _ = fs::remove_file(&probe);
    written
}

/// Remove the entry at `path` itself. A symlink goes, and what it points to
/// stays. Windows keeps a link to a folder as a folder entry, so it leaves
/// through `remove_dir`, which deletes the link and not the folder behind it.
#[cfg_attr(not(windows), allow(unused_variables))]
fn remove_entry(path: &Path, file_type: fs::FileType) -> io::Result<()> {
    #[cfg(windows)]
    if file_type.is_symlink() {
        return fs::remove_dir(path).or_else(|_| fs::remove_file(path));
    }
    fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive_bytes() -> Vec<u8> {
        (0..64 * 1024).map(|index| (index % 251) as u8).collect()
    }

    #[test]
    fn staging_writes_the_archive_to_disk_and_reads_it_back() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = archive_bytes();

        let staged = StagedArchive::write(directory.path(), &bytes).unwrap();

        assert_eq!(staged.path().parent(), Some(directory.path()));
        assert_eq!(fs::read(staged.path()).unwrap(), bytes);
        assert_eq!(staged.read().unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(staged.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "the staged file must be private");
        }
    }

    /// The staged handle keeps no copy of the archive: once the file is gone
    /// or changed, there is nothing left to install. A handle that kept the
    /// bytes in memory would still hand them back here.
    #[test]
    fn a_staged_archive_reads_from_disk_not_from_memory() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = archive_bytes();
        let staged = StagedArchive::write(directory.path(), &bytes).unwrap();

        let mut changed = bytes.clone();
        changed[100] ^= 0xff;
        fs::write(staged.path(), &changed).unwrap();
        assert_eq!(
            staged.read().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let mut grown = bytes.clone();
        grown.push(0);
        fs::write(staged.path(), &grown).unwrap();
        assert_eq!(
            staged.read().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        fs::write(staged.path(), &bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(
            staged.read().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        staged.remove().unwrap();
        assert_eq!(staged.read().unwrap_err().kind(), io::ErrorKind::NotFound);
        // Deleting twice is still a success.
        staged.remove().unwrap();
    }

    #[test]
    fn a_newer_staging_never_overwrites_the_older_file() {
        let directory = tempfile::tempdir().unwrap();
        let older = StagedArchive::write(directory.path(), b"older").unwrap();
        let newer = StagedArchive::write(directory.path(), b"newer").unwrap();

        assert_ne!(older.path(), newer.path());
        assert_eq!(older.read().unwrap(), b"older");
        older.remove().unwrap();
        assert_eq!(newer.read().unwrap(), b"newer");
    }

    #[test]
    fn preparing_the_directory_deletes_files_an_earlier_run_left() {
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join(STAGING_DIRECTORY);

        assert_eq!(prepare_directory(&directory).unwrap(), 0);
        let earlier = StagedArchive::write(&directory, b"earlier run").unwrap();
        fs::write(directory.join("partial.update"), b"interrupted write").unwrap();

        assert_eq!(prepare_directory(&directory).unwrap(), 2);
        assert!(!earlier.path().exists());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&directory).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    /// A link where the folder belongs is removed as a link. Following it
    /// would lock down and empty the folder it points to.
    #[cfg(unix)]
    #[test]
    fn preparing_the_directory_replaces_a_symlink_without_touching_its_target() {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let directory = parent.path().join(STAGING_DIRECTORY);
        std::os::unix::fs::symlink(&target, &directory).unwrap();

        prepare_directory(&directory).unwrap();

        let metadata = fs::symlink_metadata(&directory).unwrap();
        assert!(
            metadata.file_type().is_dir(),
            "the link became a real folder"
        );
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");
        let mode = fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "the link's target keeps its mode");
    }

    #[test]
    fn preparing_the_directory_replaces_a_file_in_its_place() {
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join(STAGING_DIRECTORY);
        fs::write(&directory, b"not a folder").unwrap();

        prepare_directory(&directory).unwrap();

        assert!(fs::symlink_metadata(&directory).unwrap().is_dir());
        probe_writable(&directory).unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    }

    #[test]
    fn a_folder_that_cannot_exist_fails_before_any_download() {
        let parent = tempfile::tempdir().unwrap();
        // A file stands where the cache folder should be, so nothing can be
        // created under it.
        let cache = parent.path().join("cache");
        fs::write(&cache, b"not a folder").unwrap();

        assert!(prepare_directory(&cache.join(STAGING_DIRECTORY)).is_err());
        assert!(probe_writable(&cache.join(STAGING_DIRECTORY)).is_err());
    }
}
