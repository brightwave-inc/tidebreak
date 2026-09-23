//! Your choice of whether Tidebreak downloads updates without asking.
//!
//! The choice lives in a small file in the app data directory. An
//! organization's managed policy outranks it: when the OS policy sets
//! `DownloadUpdatesAutomatically`, that value applies and Settings shows the
//! setting as managed. The updater combines the two before every check.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tidebreak_core::{replace_file, sync_directory};
use uuid::Uuid;

const FILE: &str = "update-preferences.json";
const VERSION: u8 = 1;
const MAX_BYTES: u64 = 4 * 1024;

#[derive(Debug, Deserialize, Serialize)]
struct StoredUpdatePreferences {
    version: u8,
    automatic_downloads: bool,
}

/// Whether you chose to download updates automatically. On unless you
/// turned it off: a missing file is the default, and so is one that cannot be
/// read, which is logged.
pub(crate) fn automatic_downloads(data_dir: &Path) -> bool {
    match read(data_dir) {
        Ok(Some(stored)) => stored.automatic_downloads,
        Ok(None) => true,
        Err(error) => {
            eprintln!("tidebreak-desktop: update preferences are unreadable: {error}");
            true
        }
    }
}

/// Record whether you want updates downloaded automatically.
pub(crate) fn set_automatic_downloads(data_dir: &Path, enabled: bool) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(&StoredUpdatePreferences {
        version: VERSION,
        automatic_downloads: enabled,
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    write_atomically(data_dir, &bytes)
}

fn read(data_dir: &Path) -> io::Result<Option<StoredUpdatePreferences>> {
    let file = match fs::File::open(data_dir.join(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "update preferences are too large",
        ));
    }
    let stored: StoredUpdatePreferences = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stored.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "update preferences use an unsupported version",
        ));
    }
    Ok(Some(stored))
}

/// Publish the file through a unique temporary sibling, so a crash leaves the
/// old choice or the new one, never a torn file.
fn write_atomically(data_dir: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(data_dir)?;
    let temporary = data_dir.join(format!(".update-preferences-{}.tmp", Uuid::new_v4()));
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
        replace_file(&temporary, &data_dir.join(FILE))?;
        sync_directory(data_dir)
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
    fn automatic_downloads_are_on_until_you_turn_them_off() {
        let data = tempfile::tempdir().unwrap();
        assert!(automatic_downloads(data.path()));

        set_automatic_downloads(data.path(), false).unwrap();
        assert!(!automatic_downloads(data.path()));

        set_automatic_downloads(data.path(), true).unwrap();
        assert!(automatic_downloads(data.path()));
    }

    #[test]
    fn an_unreadable_choice_reads_as_the_default() {
        let data = tempfile::tempdir().unwrap();
        for broken in [
            &b"not json"[..],
            br#"{"version": 9, "automatic_downloads": false}"#,
        ] {
            fs::write(data.path().join(FILE), broken).unwrap();
            assert!(automatic_downloads(data.path()));
        }
    }
}
