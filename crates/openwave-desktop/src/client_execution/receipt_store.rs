//! App-private recovery receipts for native client execution.

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use openwave_core::{CallId, ChatId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const RECEIPT_VERSION: u32 = 1;
const RECEIPT_DIRECTORY: &str = "client-executions";
const EXECUTOR_FILE: &str = "executor-id";
const MAX_EXECUTOR_BYTES: usize = 128;
const MAX_RECEIPT_BYTES: usize = 64 * 1024;
const MAX_RECEIPTS: usize = 1_024;

pub(crate) struct ReceiptStore {
    directory: PathBuf,
    executor_id: Uuid,
    _lock: File,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) struct FolderAccessReceipt {
    version: u32,
    pub(super) chat_id: ChatId,
    pub(super) call_id: CallId,
    pub(super) executor_id: Uuid,
    pub(super) lease_token: Uuid,
    pub(super) intent: FolderAccessIntent,
    pub(super) registration_phase: RegistrationPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) resolution: Option<StoredResolution>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum FolderAccessIntent {
    Decline,
    Selected { path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RegistrationPhase {
    NotStarted,
    Attempted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum StoredResolution {
    Completed {
        result: String,
    },
    Failed {
        result: String,
        error_code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_detail: Option<String>,
    },
}

impl std::fmt::Debug for FolderAccessReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FolderAccessReceipt")
            .field("version", &self.version)
            .field("chat_id", &self.chat_id)
            .field("call_id", &self.call_id)
            .field("executor_id", &self.executor_id)
            .field("lease_token", &"[redacted]")
            .field("intent", &self.intent)
            .field("registration_phase", &self.registration_phase)
            .field("resolution", &self.resolution)
            .finish()
    }
}

impl std::fmt::Debug for FolderAccessIntent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decline => formatter.write_str("Decline"),
            Self::Selected { .. } => formatter
                .debug_struct("Selected")
                .field("path", &"[redacted]")
                .finish(),
        }
    }
}

impl FolderAccessReceipt {
    pub(super) fn new(
        chat_id: ChatId,
        call_id: CallId,
        executor_id: Uuid,
        intent: FolderAccessIntent,
    ) -> Self {
        Self {
            version: RECEIPT_VERSION,
            chat_id,
            call_id,
            executor_id,
            lease_token: Uuid::new_v4(),
            intent,
            registration_phase: RegistrationPhase::NotStarted,
            resolution: None,
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != RECEIPT_VERSION
            || self.chat_id.0.is_nil()
            || self.call_id.0.is_nil()
            || self.executor_id.is_nil()
            || self.lease_token.is_nil()
            || matches!(&self.intent, FolderAccessIntent::Selected { path } if !path.is_absolute())
            || matches!(
                (&self.intent, self.registration_phase),
                (FolderAccessIntent::Decline, RegistrationPhase::Attempted)
            )
        {
            return Err(invalid_data("invalid client-execution receipt"));
        }
        Ok(())
    }
}

impl ReceiptStore {
    pub(crate) fn open(data_dir: &Path) -> io::Result<Self> {
        let directory = data_dir.join(RECEIPT_DIRECTORY);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(invalid_data(
                    "client-execution receipt directory is not a real directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&directory)?;
            }
            Err(error) => return Err(error),
        }
        let directory = fs::canonicalize(directory)?;
        #[cfg(unix)]
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;

        let mut lock_options = OpenOptions::new();
        lock_options.read(true).write(true).create(true);
        #[cfg(unix)]
        lock_options.mode(0o600);
        let lock = lock_options.open(directory.join("receipts.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another desktop executor owns the receipt directory",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }

        let executor_id = load_or_create_executor_id(&directory)?;
        let store = Self {
            directory,
            executor_id,
            _lock: lock,
        };
        store.load_all()?;
        Ok(store)
    }

    pub(crate) const fn executor_id(&self) -> Uuid {
        self.executor_id
    }

    pub(super) fn save(&self, receipt: &FolderAccessReceipt) -> io::Result<()> {
        receipt.validate()?;
        let bytes = serde_json::to_vec(receipt).map_err(invalid_data)?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(invalid_data("client-execution receipt is too large"));
        }
        write_atomically(&self.directory, &self.receipt_path(receipt.call_id), &bytes)
    }

    pub(super) fn remove(&self, call_id: CallId) -> io::Result<()> {
        match fs::remove_file(self.receipt_path(call_id)) {
            Ok(()) => sync_directory(&self.directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn load_all(&self) -> io::Result<Vec<FolderAccessReceipt>> {
        let mut receipts = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                return Err(invalid_data("invalid client-execution receipt name"));
            };
            if matches!(file_name, EXECUTOR_FILE | "receipts.lock") {
                continue;
            }
            if file_name.starts_with('.') && file_name.ends_with(".tmp") {
                fs::remove_file(entry.path())?;
                continue;
            }
            if receipts.len() >= MAX_RECEIPTS {
                return Err(invalid_data("too many pending client-execution receipts"));
            }
            let call_id = file_name
                .strip_suffix(".json")
                .ok_or_else(|| invalid_data("invalid client-execution receipt name"))?
                .parse::<Uuid>()
                .map(CallId::from)
                .map_err(invalid_data)?;
            validate_private_file(&entry.path(), MAX_RECEIPT_BYTES)?;
            let mut bytes = Vec::new();
            File::open(entry.path())?
                .take((MAX_RECEIPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_RECEIPT_BYTES {
                return Err(invalid_data("client-execution receipt is too large"));
            }
            let receipt: FolderAccessReceipt =
                serde_json::from_slice(&bytes).map_err(invalid_data)?;
            receipt.validate()?;
            if receipt.call_id != call_id {
                return Err(invalid_data("client-execution receipt identity mismatch"));
            }
            receipts.push(receipt);
        }
        receipts.sort_by_key(|receipt| receipt.call_id.to_string());
        Ok(receipts)
    }

    fn receipt_path(&self, call_id: CallId) -> PathBuf {
        self.directory.join(format!("{call_id}.json"))
    }
}

fn load_or_create_executor_id(directory: &Path) -> io::Result<Uuid> {
    let path = directory.join(EXECUTOR_FILE);
    match validate_private_file(&path, MAX_EXECUTOR_BYTES) {
        Ok(()) => {
            let mut value = String::new();
            File::open(&path)?
                .take((MAX_EXECUTOR_BYTES + 1) as u64)
                .read_to_string(&mut value)?;
            if value.len() > MAX_EXECUTOR_BYTES {
                return Err(invalid_data("desktop executor id is too large"));
            }
            let id = Uuid::parse_str(value.trim()).map_err(invalid_data)?;
            if id.is_nil() {
                return Err(invalid_data("desktop executor id must not be nil"));
            }
            Ok(id)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let id = Uuid::new_v4();
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&path)?;
            writeln!(file, "{id}")?;
            file.sync_all()?;
            sync_directory(directory)?;
            Ok(id)
        }
        Err(error) => Err(error),
    }
}

fn validate_private_file(path: &Path, max_bytes: usize) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid_data("private state is not a regular file"));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(invalid_data("private state is too large"));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid_data("private state permissions are too broad"));
    }
    Ok(())
}

fn write_atomically(directory: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, destination)?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(unix)]
fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_file(_temporary: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic client-execution receipt replacement is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_roundtrip_with_stable_executor_and_exact_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(temp.path()).unwrap();
        let executor_id = store.executor_id();
        let mut receipt = FolderAccessReceipt::new(
            ChatId::new(),
            CallId::new(),
            executor_id,
            FolderAccessIntent::Selected {
                path: temp.path().join("Documents"),
            },
        );
        store.save(&receipt).unwrap();
        assert_eq!(store.load_all().unwrap(), vec![receipt.clone()]);
        let debug = format!("{receipt:?}");
        assert!(!debug.contains(&receipt.lease_token.to_string()));
        assert!(!debug.contains(&temp.path().display().to_string()));

        receipt.resolution = Some(StoredResolution::Completed {
            result: r#"{"status":"connected"}"#.into(),
        });
        store.save(&receipt).unwrap();
        assert_eq!(store.load_all().unwrap(), vec![receipt.clone()]);
        store.remove(receipt.call_id).unwrap();
        assert!(store.load_all().unwrap().is_empty());
        drop(store);

        let reopened = ReceiptStore::open(temp.path()).unwrap();
        assert_eq!(reopened.executor_id(), executor_id);
    }

    #[test]
    fn receipt_directory_is_exclusive_and_corruption_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(temp.path()).unwrap();
        assert!(matches!(
            ReceiptStore::open(temp.path()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
        let receipt = FolderAccessReceipt::new(
            ChatId::new(),
            CallId::new(),
            store.executor_id(),
            FolderAccessIntent::Decline,
        );
        store.save(&receipt).unwrap();
        std::fs::write(store.receipt_path(receipt.call_id), b"not json").unwrap();
        assert!(store.load_all().is_err());

        let mut invalid = FolderAccessReceipt::new(
            ChatId::new(),
            CallId::new(),
            store.executor_id(),
            FolderAccessIntent::Decline,
        );
        invalid.registration_phase = RegistrationPhase::Attempted;
        assert!(store.save(&invalid).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn executor_identity_and_receipt_symlinks_fail_closed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let receipt_directory = temp.path().join(RECEIPT_DIRECTORY);
        std::fs::create_dir(&receipt_directory).unwrap();
        let outside = temp.path().join("outside");
        std::fs::write(&outside, Uuid::new_v4().to_string()).unwrap();
        symlink(&outside, receipt_directory.join(EXECUTOR_FILE)).unwrap();
        assert!(ReceiptStore::open(temp.path()).is_err());

        std::fs::remove_file(receipt_directory.join(EXECUTOR_FILE)).unwrap();
        let store = ReceiptStore::open(temp.path()).unwrap();
        let receipt = FolderAccessReceipt::new(
            ChatId::new(),
            CallId::new(),
            store.executor_id(),
            FolderAccessIntent::Decline,
        );
        symlink(&outside, store.receipt_path(receipt.call_id)).unwrap();
        assert!(store.load_all().is_err());
    }
}
