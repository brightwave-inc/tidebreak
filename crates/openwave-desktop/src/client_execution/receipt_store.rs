//! App-private recovery receipts for native client execution.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use chrono::{DateTime, Utc};
use openwave_core::{CallId, ChatId, HostRootId, RootAttachmentChangeId, MAX_ATTACHMENT_REVISION};
use openwave_host_broker::GrantSubject;
use openwave_host_broker::OperationId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const RECEIPT_VERSION: u32 = 3;
const RECEIPT_DIRECTORY: &str = "client-executions";
const EXECUTOR_FILE: &str = "executor-id";
const MAX_EXECUTOR_BYTES: usize = 128;
const MAX_RECEIPT_BYTES: usize = 128 * 1024;
const MAX_RECEIPTS: usize = 1_024;
const FOLDER_OPERATION_PREFIX: &str = "folder-operation-";
const DELEGATED_FILE_READ_PREFIX: &str = "delegated-file-read-";
const MANUAL_FOLDER_CONNECT_PREFIX: &str = "manual-folder-connect-";
const MAX_SAFE_ROOT_DISPLAY_BYTES: usize = 1_024;

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
    pub(super) registration_operation_id: OperationId,
    pub(super) registration_phase: RegistrationPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) product_sync: Option<ProductRootAttachmentSync>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) resolution: Option<StoredResolution>,
}

/// Durable receipt for one read-only foreground folder operation.
///
/// It intentionally stores no host path or authority. The canonical request is
/// recovered from the server-owned tool call; this receipt preserves only the
/// exact native lease and terminal model result across desktop restarts.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) struct FolderOperationReceipt {
    version: u32,
    pub(super) chat_id: ChatId,
    pub(super) call_id: CallId,
    pub(super) executor_id: Uuid,
    pub(super) lease_token: Uuid,
    #[serde(default)]
    pub(super) phase: FolderOperationPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) resolution: Option<StoredResolution>,
}

/// Native-only recovery state for one exact sandbox delegated-file read.
///
/// Host root and relative path are deliberately absent. They are returned only
/// by the native claim and are revalidated again by the final heartbeat.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) struct DelegatedFileReadReceipt {
    version: u32,
    pub(super) call_id: CallId,
    pub(super) executor_id: Uuid,
    pub(super) lease_token: Uuid,
    #[serde(default)]
    pub(super) phase: FolderOperationPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) resolution: Option<DelegatedFileResolution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DelegatedFileFailureReason {
    NotFound,
    NotUtf8,
    TooLarge,
    PermissionDenied,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DelegatedFileResolution {
    Completed { content: String },
    Failed { reason: DelegatedFileFailureReason },
    Cancelled,
}

/// App-private recovery state for a folder selected from the manual connected
/// folders UI. Absolute paths and broker authority never leave native storage.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) struct ManualFolderConnectReceipt {
    version: u32,
    pub(super) chat_id: ChatId,
    pub(super) subject: GrantSubject,
    pub(super) path: PathBuf,
    pub(super) registration_operation_id: OperationId,
    pub(super) change_id: RootAttachmentChangeId,
    pub(super) cleanup_operation_id: OperationId,
    pub(super) registration_phase: RegistrationPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) product_sync: Option<ProductRootAttachmentSync>,
}

/// Whether a read-only broker request may have been dispatched already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(super) enum FolderOperationPhase {
    /// No host operation has been sent. Exact claim recovery may continue.
    #[default]
    NotStarted,
    /// A final lease heartbeat succeeded and broker dispatch may have begun.
    /// Recovery must terminalize rather than issue another read.
    DispatchStarted,
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

/// Exact product-side attachment identity persisted before begin or broker
/// attachment dispatch. The change id also identifies the broker's idempotent
/// `AttachRoot` mutation, but is independent of both the tool call and the
/// picker registration mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) struct ProductRootAttachmentSync {
    pub(super) change_id: RootAttachmentChangeId,
    pub(super) root_id: HostRootId,
    pub(super) display_name: String,
    pub(super) expected_attachment_revision: i64,
    pub(super) created_at: DateTime<Utc>,
    pub(super) cleanup_operation_id: OperationId,
    #[serde(default)]
    pub(super) cleanup_phase: CleanupPhase,
    #[serde(default)]
    pub(super) attachment_phase: AttachmentPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(super) enum CleanupPhase {
    #[default]
    NotStarted,
    DispatchAttempted,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(super) enum AttachmentPhase {
    /// Product begin has not necessarily committed and no broker attachment
    /// dispatch has been admitted yet.
    #[default]
    Prepared,
    /// The exact broker attachment id may have been dispatched. Recovery must
    /// consult its durable receipt before sending the same id again.
    DispatchAttempted,
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
            .field("registration_operation_id", &self.registration_operation_id)
            .field("registration_phase", &self.registration_phase)
            .field("product_sync", &self.product_sync)
            .field("resolution", &self.resolution)
            .finish()
    }
}

impl std::fmt::Debug for FolderOperationReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FolderOperationReceipt")
            .field("version", &self.version)
            .field("chat_id", &self.chat_id)
            .field("call_id", &self.call_id)
            .field("executor_id", &self.executor_id)
            .field("lease_token", &"[redacted]")
            .field("phase", &self.phase)
            .field("resolution", &self.resolution)
            .finish()
    }
}

impl std::fmt::Debug for DelegatedFileReadReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DelegatedFileReadReceipt")
            .field("version", &self.version)
            .field("call_id", &self.call_id)
            .field("executor_id", &self.executor_id)
            .field("lease_token", &"[redacted]")
            .field("phase", &self.phase)
            .field(
                "resolution",
                &self.resolution.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl std::fmt::Debug for ManualFolderConnectReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManualFolderConnectReceipt")
            .field("version", &self.version)
            .field("chat_id", &self.chat_id)
            .field("subject", &self.subject)
            .field("path", &"[redacted]")
            .field("registration_operation_id", &self.registration_operation_id)
            .field("change_id", &self.change_id)
            .field("cleanup_operation_id", &self.cleanup_operation_id)
            .field("registration_phase", &self.registration_phase)
            .field("product_sync", &self.product_sync)
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
            registration_operation_id: OperationId::new(),
            registration_phase: RegistrationPhase::NotStarted,
            product_sync: None,
            resolution: None,
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != RECEIPT_VERSION
            || self.chat_id.0.is_nil()
            || self.call_id.0.is_nil()
            || self.executor_id.is_nil()
            || self.lease_token.is_nil()
            || self.registration_operation_id.as_uuid() == self.call_id.0
            || matches!(&self.intent, FolderAccessIntent::Selected { path } if !path.is_absolute())
            || matches!(
                (&self.intent, self.registration_phase),
                (FolderAccessIntent::Decline, RegistrationPhase::Attempted)
            )
            || matches!(
                (&self.intent, &self.product_sync),
                (FolderAccessIntent::Decline, Some(_))
            )
            || (self.product_sync.is_some()
                && self.registration_phase != RegistrationPhase::Attempted)
            || self.product_sync.as_ref().is_some_and(|sync| {
                sync.change_id.as_uuid().is_nil()
                    || sync.change_id.as_uuid() == &self.call_id.0
                    || *sync.change_id.as_uuid() == self.registration_operation_id.as_uuid()
                    || sync.display_name.is_empty()
                    || sync.display_name.len() > MAX_SAFE_ROOT_DISPLAY_BYTES
                    || sync.display_name.contains('\0')
                    || sync.cleanup_operation_id.as_uuid() == self.call_id.0
                    || sync.cleanup_operation_id.as_uuid()
                        == self.registration_operation_id.as_uuid()
                    || sync.cleanup_operation_id.as_uuid() == *sync.change_id.as_uuid()
                    || !(0..=MAX_ATTACHMENT_REVISION).contains(&sync.expected_attachment_revision)
            })
        {
            return Err(invalid_data("invalid client-execution receipt"));
        }
        Ok(())
    }
}

impl ManualFolderConnectReceipt {
    pub(super) fn new(chat_id: ChatId, subject: GrantSubject, path: PathBuf) -> Self {
        let registration_operation_id = OperationId::new();
        let mut change_id = RootAttachmentChangeId::new();
        while *change_id.as_uuid() == registration_operation_id.as_uuid() {
            change_id = RootAttachmentChangeId::new();
        }
        let cleanup_operation_id = loop {
            let id = OperationId::new();
            if id.as_uuid() != registration_operation_id.as_uuid()
                && id.as_uuid() != *change_id.as_uuid()
            {
                break id;
            }
        };
        Self {
            version: RECEIPT_VERSION,
            chat_id,
            subject,
            path,
            registration_operation_id,
            change_id,
            cleanup_operation_id,
            registration_phase: RegistrationPhase::NotStarted,
            product_sync: None,
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != RECEIPT_VERSION
            || self.chat_id.as_uuid().is_nil()
            || !self.path.is_absolute()
            || self.registration_operation_id.as_uuid() == *self.change_id.as_uuid()
            || self.registration_operation_id == self.cleanup_operation_id
            || self.cleanup_operation_id.as_uuid() == *self.change_id.as_uuid()
            || (self.registration_phase == RegistrationPhase::NotStarted
                && self.product_sync.is_some())
        {
            return Err(invalid_data("invalid manual folder-connect receipt"));
        }
        if self.subject.kind() == openwave_host_broker::SubjectKind::Conversation
            && self.subject.id() != *self.chat_id.as_uuid()
        {
            return Err(invalid_data("manual folder-connect subject mismatch"));
        }
        if let Some(sync) = &self.product_sync {
            if sync.change_id.as_uuid().is_nil()
                || sync.display_name.is_empty()
                || sync.display_name.len() > MAX_SAFE_ROOT_DISPLAY_BYTES
                || sync.display_name.contains('\0')
                || !(0..=MAX_ATTACHMENT_REVISION).contains(&sync.expected_attachment_revision)
                || sync.change_id != self.change_id
                || sync.cleanup_operation_id != self.cleanup_operation_id
            {
                return Err(invalid_data("manual folder-connect identity mismatch"));
            }
        }
        Ok(())
    }
}

impl FolderOperationReceipt {
    pub(super) fn new(chat_id: ChatId, call_id: CallId, executor_id: Uuid) -> Self {
        Self {
            version: RECEIPT_VERSION,
            chat_id,
            call_id,
            executor_id,
            lease_token: Uuid::new_v4(),
            phase: FolderOperationPhase::NotStarted,
            resolution: None,
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != RECEIPT_VERSION
            || self.chat_id.0.is_nil()
            || self.call_id.0.is_nil()
            || self.executor_id.is_nil()
            || self.lease_token.is_nil()
        {
            return Err(invalid_data("invalid folder-operation receipt"));
        }
        Ok(())
    }
}

impl DelegatedFileReadReceipt {
    pub(super) fn new(call_id: CallId, executor_id: Uuid) -> Self {
        Self {
            version: RECEIPT_VERSION,
            call_id,
            executor_id,
            lease_token: Uuid::new_v4(),
            phase: FolderOperationPhase::NotStarted,
            resolution: None,
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != RECEIPT_VERSION
            || self.call_id.0.is_nil()
            || self.executor_id.is_nil()
            || self.lease_token.is_nil()
            || matches!(
                &self.resolution,
                Some(DelegatedFileResolution::Completed { content })
                    if !delegated_file_content_fits_server(content)
            )
            || (self.resolution.is_some() && self.phase != FolderOperationPhase::DispatchStarted)
        {
            return Err(invalid_data("invalid delegated-file receipt"));
        }
        Ok(())
    }

    pub(super) fn set_bounded_resolution(&mut self, resolution: DelegatedFileResolution) {
        self.resolution = Some(resolution);
        let fits = serde_json::to_vec(self)
            .map(|bytes| bytes.len() <= MAX_RECEIPT_BYTES)
            .unwrap_or(false);
        if !fits {
            self.resolution = Some(DelegatedFileResolution::Failed {
                reason: DelegatedFileFailureReason::TooLarge,
            });
        }
    }
}

pub(super) fn delegated_file_content_fits_server(content: &str) -> bool {
    !content.contains('\0')
        && serde_json::to_string(&serde_json::json!({"content": content}))
            .is_ok_and(|encoded| encoded.len() <= openwave_core::SandboxToolCall::MAX_RESULT_BYTES)
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
        store.load_operations()?;
        store.load_delegated_file_reads()?;
        store.load_manual_connects()?;
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

    pub(super) fn save_operation(&self, receipt: &FolderOperationReceipt) -> io::Result<()> {
        receipt.validate()?;
        let bytes = serde_json::to_vec(receipt).map_err(invalid_data)?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(invalid_data("folder-operation receipt is too large"));
        }
        write_atomically(
            &self.directory,
            &self.operation_receipt_path(receipt.call_id),
            &bytes,
        )
    }

    pub(super) fn save_delegated_file_read(
        &self,
        receipt: &DelegatedFileReadReceipt,
    ) -> io::Result<()> {
        receipt.validate()?;
        let bytes = serde_json::to_vec(receipt).map_err(invalid_data)?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(invalid_data("delegated-file receipt is too large"));
        }
        write_atomically(
            &self.directory,
            &self.delegated_file_read_receipt_path(receipt.call_id),
            &bytes,
        )
    }

    pub(super) fn save_manual_connect(
        &self,
        receipt: &ManualFolderConnectReceipt,
    ) -> io::Result<()> {
        receipt.validate()?;
        let bytes = serde_json::to_vec(receipt).map_err(invalid_data)?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(invalid_data("manual folder-connect receipt is too large"));
        }
        write_atomically(
            &self.directory,
            &self.manual_connect_receipt_path(receipt.change_id),
            &bytes,
        )
    }

    pub(super) fn remove(&self, call_id: CallId) -> io::Result<()> {
        match fs::remove_file(self.receipt_path(call_id)) {
            Ok(()) => sync_directory(&self.directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn remove_operation(&self, call_id: CallId) -> io::Result<()> {
        match fs::remove_file(self.operation_receipt_path(call_id)) {
            Ok(()) => sync_directory(&self.directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn remove_delegated_file_read(&self, call_id: CallId) -> io::Result<()> {
        match fs::remove_file(self.delegated_file_read_receipt_path(call_id)) {
            Ok(()) => sync_directory(&self.directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn remove_manual_connect(
        &self,
        change_id: RootAttachmentChangeId,
    ) -> io::Result<()> {
        match fs::remove_file(self.manual_connect_receipt_path(change_id)) {
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
            if file_name.starts_with(FOLDER_OPERATION_PREFIX) {
                continue;
            }
            if file_name.starts_with(DELEGATED_FILE_READ_PREFIX) {
                continue;
            }
            if file_name.starts_with(MANUAL_FOLDER_CONNECT_PREFIX) {
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

    pub(super) fn load_operations(&self) -> io::Result<Vec<FolderOperationReceipt>> {
        let mut receipts = Vec::new();
        let mut call_ids = HashSet::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                return Err(invalid_data("invalid client-execution receipt name"));
            };
            let Some(call_id) = file_name
                .strip_prefix(FOLDER_OPERATION_PREFIX)
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if receipts.len() >= MAX_RECEIPTS {
                return Err(invalid_data("too many pending client-execution receipts"));
            }
            let call_id = call_id
                .parse::<Uuid>()
                .map(CallId::from)
                .map_err(invalid_data)?;
            validate_private_file(&entry.path(), MAX_RECEIPT_BYTES)?;
            let mut bytes = Vec::new();
            File::open(entry.path())?
                .take((MAX_RECEIPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_RECEIPT_BYTES {
                return Err(invalid_data("folder-operation receipt is too large"));
            }
            let receipt: FolderOperationReceipt =
                serde_json::from_slice(&bytes).map_err(invalid_data)?;
            receipt.validate()?;
            if receipt.call_id != call_id {
                return Err(invalid_data("folder-operation receipt identity mismatch"));
            }
            if !call_ids.insert(call_id) {
                return Err(invalid_data("duplicate folder-operation receipt identity"));
            }
            receipts.push(receipt);
        }
        receipts.sort_by_key(|receipt| receipt.call_id.to_string());
        Ok(receipts)
    }

    pub(super) fn load_delegated_file_reads(&self) -> io::Result<Vec<DelegatedFileReadReceipt>> {
        let mut receipts = Vec::new();
        let mut call_ids = HashSet::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                return Err(invalid_data("invalid client-execution receipt name"));
            };
            let Some(call_id) = file_name
                .strip_prefix(DELEGATED_FILE_READ_PREFIX)
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if receipts.len() >= MAX_RECEIPTS {
                return Err(invalid_data("too many pending delegated-file receipts"));
            }
            let call_id = call_id
                .parse::<Uuid>()
                .map(CallId::from)
                .map_err(invalid_data)?;
            validate_private_file(&entry.path(), MAX_RECEIPT_BYTES)?;
            let mut bytes = Vec::new();
            File::open(entry.path())?
                .take((MAX_RECEIPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_RECEIPT_BYTES {
                return Err(invalid_data("delegated-file receipt is too large"));
            }
            let receipt: DelegatedFileReadReceipt =
                serde_json::from_slice(&bytes).map_err(invalid_data)?;
            receipt.validate()?;
            if receipt.call_id != call_id || !call_ids.insert(call_id) {
                return Err(invalid_data("delegated-file receipt identity mismatch"));
            }
            receipts.push(receipt);
        }
        receipts.sort_by_key(|receipt| receipt.call_id.to_string());
        Ok(receipts)
    }

    pub(super) fn load_manual_connects(&self) -> io::Result<Vec<ManualFolderConnectReceipt>> {
        let mut receipts = Vec::new();
        let mut change_ids = HashSet::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                return Err(invalid_data("invalid client-execution receipt name"));
            };
            let Some(change_id) = file_name
                .strip_prefix(MANUAL_FOLDER_CONNECT_PREFIX)
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if receipts.len() >= MAX_RECEIPTS {
                return Err(invalid_data("too many pending manual folder connects"));
            }
            let change_id = change_id
                .parse::<Uuid>()
                .map_err(invalid_data)
                .and_then(|id| RootAttachmentChangeId::from_uuid(id).map_err(invalid_data))?;
            validate_private_file(&entry.path(), MAX_RECEIPT_BYTES)?;
            let mut bytes = Vec::new();
            File::open(entry.path())?
                .take((MAX_RECEIPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_RECEIPT_BYTES {
                return Err(invalid_data("manual folder-connect receipt is too large"));
            }
            let receipt: ManualFolderConnectReceipt =
                serde_json::from_slice(&bytes).map_err(invalid_data)?;
            receipt.validate()?;
            if receipt.change_id != change_id || !change_ids.insert(change_id) {
                return Err(invalid_data(
                    "manual folder-connect receipt identity mismatch",
                ));
            }
            receipts.push(receipt);
        }
        receipts.sort_by_key(|receipt| receipt.change_id.to_string());
        Ok(receipts)
    }

    fn receipt_path(&self, call_id: CallId) -> PathBuf {
        self.directory.join(format!("{call_id}.json"))
    }

    fn operation_receipt_path(&self, call_id: CallId) -> PathBuf {
        self.directory
            .join(format!("{FOLDER_OPERATION_PREFIX}{call_id}.json"))
    }

    fn delegated_file_read_receipt_path(&self, call_id: CallId) -> PathBuf {
        self.directory
            .join(format!("{DELEGATED_FILE_READ_PREFIX}{call_id}.json"))
    }

    fn manual_connect_receipt_path(&self, change_id: RootAttachmentChangeId) -> PathBuf {
        self.directory
            .join(format!("{MANUAL_FOLDER_CONNECT_PREFIX}{change_id}.json"))
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
        assert_ne!(
            receipt.registration_operation_id.as_uuid(),
            receipt.call_id.0
        );
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
    fn delegated_file_receipts_are_pathless_private_and_never_log_content() {
        let temp = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(temp.path()).unwrap();
        let mut receipt = DelegatedFileReadReceipt::new(CallId::new(), store.executor_id());
        store.save_delegated_file_read(&receipt).unwrap();
        assert_eq!(
            store.load_delegated_file_reads().unwrap(),
            vec![receipt.clone()]
        );

        receipt.phase = FolderOperationPhase::DispatchStarted;
        receipt.resolution = Some(DelegatedFileResolution::Completed {
            content: "private-content-sentinel".to_owned(),
        });
        store.save_delegated_file_read(&receipt).unwrap();
        let debug = format!("{receipt:?}");
        assert!(!debug.contains("private-content-sentinel"));
        assert!(!debug.contains("relative_path"));
        store.remove_delegated_file_read(receipt.call_id).unwrap();
        assert!(store.load_delegated_file_reads().unwrap().is_empty());
    }

    #[test]
    fn product_attachment_identity_and_revision_are_durable_and_independent() {
        let temp = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(temp.path()).unwrap();
        let mut receipt = FolderAccessReceipt::new(
            ChatId::new(),
            CallId::new(),
            store.executor_id(),
            FolderAccessIntent::Selected {
                path: temp.path().join("Documents"),
            },
        );
        receipt.registration_phase = RegistrationPhase::Attempted;
        let sync = ProductRootAttachmentSync {
            change_id: RootAttachmentChangeId::new(),
            root_id: HostRootId::from_uuid(Uuid::new_v4()).unwrap(),
            display_name: "Documents".to_owned(),
            expected_attachment_revision: 7,
            created_at: Utc::now(),
            cleanup_operation_id: OperationId::new(),
            cleanup_phase: CleanupPhase::NotStarted,
            attachment_phase: AttachmentPhase::Prepared,
        };
        assert_ne!(sync.change_id.as_uuid(), &receipt.call_id.0);
        assert_ne!(
            *sync.change_id.as_uuid(),
            receipt.registration_operation_id.as_uuid()
        );
        assert_ne!(sync.cleanup_operation_id.as_uuid(), receipt.call_id.0);
        assert_ne!(
            sync.cleanup_operation_id.as_uuid(),
            receipt.registration_operation_id.as_uuid()
        );
        assert_ne!(
            sync.cleanup_operation_id.as_uuid(),
            *sync.change_id.as_uuid()
        );
        receipt.product_sync = Some(sync.clone());
        store.save(&receipt).unwrap();
        let recovered = store.load_all().unwrap().pop().unwrap();
        assert_eq!(recovered.product_sync, Some(sync));

        receipt.product_sync.as_mut().unwrap().cleanup_phase = CleanupPhase::Completed;
        receipt.resolution = Some(StoredResolution::Failed {
            result: r#"{"status":"unavailable"}"#.to_owned(),
            error_code: "folder_access_product_sync_rejected".to_owned(),
            error_detail: None,
        });
        store.save(&receipt).unwrap();
        assert!(matches!(
            store.load_all().unwrap().pop().unwrap().resolution,
            Some(StoredResolution::Failed { error_code, .. })
                if error_code == "folder_access_product_sync_rejected"
        ));
        store.remove(receipt.call_id).unwrap();
        assert!(store.load_all().unwrap().is_empty());

        let mut reused = receipt.clone();
        reused.product_sync.as_mut().unwrap().change_id =
            RootAttachmentChangeId::from_uuid(receipt.call_id.0).unwrap();
        assert!(store.save(&reused).is_err());
    }

    #[test]
    fn folder_operation_receipts_are_separate_and_keep_lease_tokens_private() {
        let temp = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(temp.path()).unwrap();
        let receipt =
            FolderOperationReceipt::new(ChatId::new(), CallId::new(), store.executor_id());
        store.save_operation(&receipt).unwrap();
        assert_eq!(store.load_operations().unwrap(), vec![receipt.clone()]);
        assert!(store.load_all().unwrap().is_empty());
        let debug = format!("{receipt:?}");
        assert!(!debug.contains(&receipt.lease_token.to_string()));
        store.remove_operation(receipt.call_id).unwrap();
        assert!(store.load_operations().unwrap().is_empty());
    }

    #[test]
    fn manual_connect_receipts_keep_paths_private_and_identities_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(temp.path()).unwrap();
        let chat_id = ChatId::new();
        let subject = GrantSubject::conversation(chat_id.0).unwrap();
        let path = temp.path().join("Documents");
        let mut receipt = ManualFolderConnectReceipt::new(chat_id, subject, path.clone());
        store.save_manual_connect(&receipt).unwrap();
        assert_eq!(store.load_manual_connects().unwrap(), vec![receipt.clone()]);
        assert!(store.load_all().unwrap().is_empty());
        assert!(!format!("{receipt:?}").contains(path.to_str().unwrap()));
        assert_ne!(
            receipt.registration_operation_id.as_uuid(),
            *receipt.change_id.as_uuid()
        );
        assert_ne!(
            receipt.cleanup_operation_id.as_uuid(),
            *receipt.change_id.as_uuid()
        );

        receipt.registration_phase = RegistrationPhase::Attempted;
        receipt.product_sync = Some(ProductRootAttachmentSync {
            change_id: receipt.change_id,
            root_id: HostRootId::from_uuid(Uuid::new_v4()).unwrap(),
            display_name: "Documents".to_owned(),
            expected_attachment_revision: 3,
            created_at: Utc::now(),
            cleanup_operation_id: receipt.cleanup_operation_id,
            cleanup_phase: CleanupPhase::NotStarted,
            attachment_phase: AttachmentPhase::Prepared,
        });
        store.save_manual_connect(&receipt).unwrap();
        assert_eq!(store.load_manual_connects().unwrap(), vec![receipt.clone()]);
        store.remove_manual_connect(receipt.change_id).unwrap();
        assert!(store.load_manual_connects().unwrap().is_empty());
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
