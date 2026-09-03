//! Private product defaults for folders attached to future conversations.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tidebreak_core::{replace_file, sync_directory};
use tidebreak_host_broker::RootId;
use uuid::Uuid;

const DIRECTORY: &str = "host-access";
const FILE: &str = "trusted-folders.json";
const VERSION: u8 = 1;
const MAX_BYTES: u64 = 256 * 1024;

pub(crate) struct TrustedFolderStore {
    directory: PathBuf,
    path: PathBuf,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTrustedFolders {
    version: u8,
    root_ids: Vec<RootId>,
}

impl TrustedFolderStore {
    pub(crate) fn open(data_dir: &Path) -> io::Result<Self> {
        let directory = data_dir.join(DIRECTORY);
        fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            path: directory.join(FILE),
            directory,
        })
    }

    pub(crate) fn list(&self) -> io::Result<HashSet<RootId>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
            Err(error) => return Err(error),
        };
        let metadata = fs::metadata(&self.path)?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trusted folder defaults are invalid",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "trusted folder defaults have broad permissions",
                ));
            }
        }
        let stored: StoredTrustedFolders = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if stored.version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trusted folder defaults use an unsupported version",
            ));
        }
        Ok(stored.root_ids.into_iter().collect())
    }

    pub(crate) fn set(&self, root_id: RootId, trusted: bool) -> io::Result<bool> {
        let mut roots = self.list()?;
        let changed = if trusted {
            roots.insert(root_id)
        } else {
            roots.remove(&root_id)
        };
        if !changed {
            return Ok(false);
        }
        let mut root_ids = roots.into_iter().collect::<Vec<_>>();
        root_ids.sort_by_key(|root_id| root_id.to_string());
        let bytes = serde_json::to_vec_pretty(&StoredTrustedFolders {
            version: VERSION,
            root_ids,
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        write_atomically(&self.directory, &self.path, &bytes)?;
        Ok(true)
    }
}

fn write_atomically(directory: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = directory.join(format!(".trusted-folders-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, destination)?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_removes_opaque_folder_defaults() {
        let data = tempfile::tempdir().unwrap();
        let store = TrustedFolderStore::open(data.path()).unwrap();
        let first = RootId::new();
        let second = RootId::new();

        assert!(store.list().unwrap().is_empty());
        assert!(store.set(first, true).unwrap());
        assert!(store.set(second, true).unwrap());
        assert!(!store.set(first, true).unwrap());
        assert_eq!(store.list().unwrap(), HashSet::from([first, second]));
        assert!(store.set(first, false).unwrap());
        assert_eq!(store.list().unwrap(), HashSet::from([second]));
    }
}
