//! Structured, privacy-preserving audit records for host operations.
//!
//! Audit targets contain opaque root identities and bounded root-relative paths,
//! never absolute host paths or file contents. The durable sink appends one
//! fsync'd JSON record at a time and rotates within a fixed local retention bound.

use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
    time::Duration,
};

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use chrono::{DateTime, Utc};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    Capability, ErrorCode, ExecutionContext, GrantId, GrantSubject, OperationId, RelativePath,
    RequestId, RootId,
};

const AUDIT_FILE_NAME: &str = "host-broker-audit.jsonl";
const AUDIT_ARCHIVE_NAME: &str = "host-broker-audit.previous.jsonl";
const AUDIT_WRITE_ATTEMPTS: usize = 3;
const AUDIT_RETRY_BACKOFF: Duration = Duration::from_millis(5);
const MAX_AUDIT_RECORD_BYTES: usize = 4 * 1024;
const MAX_AUDIT_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_AUDIT_LABEL_BYTES: usize = 1024;
const MAX_MEMORY_EVENTS: usize = 4096;
const MAX_AUDIT_DIRECTORY_ENTRIES: usize = 4096;

/// An audit record could not be appended durably.
#[derive(Debug, Error)]
pub enum AuditError {
    #[error("audit record exceeds its size limit")]
    RecordTooLarge,
    #[error("audit serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("audit storage failed: {0}")]
    Io(#[from] io::Error),
    #[error("audit writer lock was poisoned")]
    Poisoned,
    #[error("audit storage is unavailable until restart")]
    Unavailable,
    #[error("audit publication is ambiguous; restart is required")]
    PublicationAmbiguous,
}

/// Stable operation name recorded in the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOperation {
    RegisterRoot,
    RevokeRoot,
    ListRoots,
    ListDirectory,
    ReadFile,
    ProtocolVersionMismatch,
}

/// Whether the broker performed, refused, or failed an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Allowed,
    Denied,
    Failed,
}

/// Trusted product identity responsible for a host operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditActor {
    Control {
        subject: GrantSubject,
        conversation_id: Option<Uuid>,
    },
    Operation {
        context: ExecutionContext,
    },
}

/// De-sensitized operation target. Absolute paths cannot be represented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditTarget {
    Subject,
    SelectedFolder {
        display_name: AuditLabel,
    },
    Root {
        root_id: RootId,
    },
    Path {
        root_id: RootId,
        relative: RelativePath,
    },
}

/// Bounded picker-result leaf name used in audit and management UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct AuditLabel(String);

impl AuditLabel {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_leaf(value: &str) -> Self {
        let sanitized = value
            .chars()
            .map(|character| {
                if matches!(character, '/' | '\\' | '\0') {
                    '\u{fffd}'
                } else {
                    character
                }
            })
            .collect::<String>();
        let sanitized = if sanitized.is_empty() {
            "Selected folder"
        } else {
            &sanitized
        };
        Self(truncate_utf8(sanitized, MAX_AUDIT_LABEL_BYTES))
    }
}

impl<'de> Deserialize<'de> for AuditLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.len() > MAX_AUDIT_LABEL_BYTES
            || value.contains(['/', '\\', '\0'])
            || value.is_empty()
        {
            return Err(D::Error::custom("invalid audit display label"));
        }
        Ok(Self(value))
    }
}

impl AuditTarget {
    pub(crate) fn selected_folder(path: &Path) -> Self {
        let display_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Selected folder".to_owned());
        Self::SelectedFolder {
            display_name: AuditLabel::from_leaf(&display_name),
        }
    }

    pub(crate) fn path(root_id: RootId, path: &RelativePath) -> Self {
        Self::Path {
            root_id,
            relative: path.clone(),
        }
    }
}

/// One bounded, structured machine-boundary audit event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEvent {
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub request_id: RequestId,
    pub operation_id: Option<OperationId>,
    pub actor: AuditActor,
    pub operation: AuditOperation,
    pub target: AuditTarget,
    pub outcome: AuditOutcome,
    pub capability: Option<Capability>,
    pub grant_id: Option<GrantId>,
    pub error_code: Option<ErrorCode>,
    pub item_count: Option<usize>,
    pub bytes: Option<usize>,
}

/// Append-only destination for broker audit events.
pub trait AuditSink: Send + Sync {
    /// Record one event durably enough for the sink's deployment tier.
    fn record(&self, event: &AuditEvent) -> Result<(), AuditError>;
}

/// File-backed local audit with bounded two-generation retention.
pub(crate) struct JsonlAuditSink {
    path: PathBuf,
    archive: PathBuf,
    max_file_bytes: u64,
    writer: Mutex<WriterState>,
}

struct WriterState {
    available: bool,
    #[cfg(test)]
    fail_after_write_once: bool,
    #[cfg(test)]
    fail_rotation_after_archive_once: bool,
}

impl JsonlAuditSink {
    pub(crate) fn open(data_dir: &Path) -> Result<Self, AuditError> {
        fs::create_dir_all(data_dir)?;
        #[cfg(unix)]
        fs::set_permissions(data_dir, fs::Permissions::from_mode(0o700))?;
        let sink = Self {
            path: data_dir.join(AUDIT_FILE_NAME),
            archive: data_dir.join(AUDIT_ARCHIVE_NAME),
            max_file_bytes: MAX_AUDIT_FILE_BYTES,
            writer: Mutex::new(WriterState {
                available: true,
                #[cfg(test)]
                fail_after_write_once: false,
                #[cfg(test)]
                fail_rotation_after_archive_once: false,
            }),
        };
        sink.recover()?;
        Ok(sink)
    }

    fn recover(&self) -> io::Result<()> {
        self.remove_stale_temporaries()?;
        if !self.path.exists() {
            self.create_active()?;
        }
        self.enforce_private_permissions()?;
        self.repair_incomplete_tail()?;
        match self.archive.metadata() {
            Ok(metadata) if metadata.len() > self.max_file_bytes => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "audit archive exceeds its retention bound",
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn enforce_private_permissions(&self) -> io::Result<()> {
        #[cfg(unix)]
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    fn repair_incomplete_tail(&self) -> io::Result<()> {
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        let mut file = options.open(&self.path)?;
        let length = file.metadata()?.len();
        if length > self.max_file_bytes + MAX_AUDIT_RECORD_BYTES as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "audit file exceeds its retention bound",
            ));
        }
        let mut bytes = Vec::with_capacity(length as usize);
        file.read_to_end(&mut bytes)?;
        if bytes.last().is_none_or(|byte| *byte == b'\n') {
            return Ok(());
        }
        let repaired_length = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        file.set_len(repaired_length as u64)?;
        file.sync_all()?;
        Ok(())
    }

    fn append_once(&self, line: &[u8], writer: &mut WriterState) -> Result<(), AppendFailure> {
        if self
            .path
            .metadata()
            .map_err(AppendFailure::safe)?
            .len()
            .saturating_add(line.len() as u64)
            > self.max_file_bytes
        {
            self.rotate(writer).map_err(AppendFailure::ambiguous)?;
        }
        let mut options = OpenOptions::new();
        options.append(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&self.path).map_err(AppendFailure::safe)?;
        let original_length = file.metadata().map_err(AppendFailure::safe)?.len();
        let write_result = file.write_all(line).and_then(|()| {
            #[cfg(test)]
            if std::mem::take(&mut writer.fail_after_write_once) {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            file.sync_all()
        });
        if let Err(source) = write_result {
            return match file.set_len(original_length).and_then(|()| file.sync_all()) {
                Ok(()) => Err(AppendFailure::safe(source)),
                Err(_) => Err(AppendFailure::ambiguous(source)),
            };
        }
        Ok(())
    }

    fn rotate(&self, writer: &mut WriterState) -> io::Result<()> {
        #[cfg(not(test))]
        let _ = writer;
        let temporary = self.create_empty_temporary()?;
        let result = (|| {
            replace_file(&self.path, &self.archive)?;
            #[cfg(test)]
            if std::mem::take(&mut writer.fail_rotation_after_archive_once) {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            replace_file(&temporary, &self.path)?;
            sync_directory(self.path.parent().expect("audit file has a parent"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn create_active(&self) -> io::Result<()> {
        let temporary = self.create_empty_temporary()?;
        let result = replace_file(&temporary, &self.path)
            .and_then(|()| sync_directory(self.path.parent().expect("audit file has a parent")));
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn create_empty_temporary(&self) -> io::Result<PathBuf> {
        let temporary = self
            .path
            .parent()
            .expect("audit file has a parent")
            .join(format!(".{AUDIT_FILE_NAME}.{}.tmp", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&temporary)?;
        file.sync_all()?;
        Ok(temporary)
    }

    fn remove_stale_temporaries(&self) -> io::Result<()> {
        let directory = self.path.parent().expect("audit file has a parent");
        let prefix = format!(".{AUDIT_FILE_NAME}.");
        for (index, entry) in fs::read_dir(directory)?.enumerate() {
            if index >= MAX_AUDIT_DIRECTORY_ENTRIES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "audit directory contains too many entries",
                ));
            }
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && name.ends_with(".tmp") {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn with_max_file_bytes(mut self, max_file_bytes: u64) -> Self {
        self.max_file_bytes = max_file_bytes;
        self
    }
}

impl AuditSink for JsonlAuditSink {
    fn record(&self, event: &AuditEvent) -> Result<(), AuditError> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        if line.len() > MAX_AUDIT_RECORD_BYTES {
            return Err(AuditError::RecordTooLarge);
        }
        let mut writer = self.writer.lock().map_err(|_| AuditError::Poisoned)?;
        if !writer.available {
            return Err(AuditError::Unavailable);
        }
        let mut last_error = None;
        for attempt in 0..AUDIT_WRITE_ATTEMPTS {
            match self.append_once(&line, &mut writer) {
                Ok(()) => return Ok(()),
                Err(error) if error.safe_to_retry => {
                    let retryable = transient_io_kind(error.source.kind());
                    last_error = Some(error.source);
                    if !retryable || attempt + 1 == AUDIT_WRITE_ATTEMPTS {
                        break;
                    }
                    thread::sleep(AUDIT_RETRY_BACKOFF);
                }
                Err(_) => {
                    writer.available = false;
                    return Err(AuditError::PublicationAmbiguous);
                }
            }
        }
        Err(last_error
            .expect("at least one append attempt was made")
            .into())
    }
}

struct AppendFailure {
    source: io::Error,
    safe_to_retry: bool,
}

impl AppendFailure {
    fn safe(source: io::Error) -> Self {
        Self {
            source,
            safe_to_retry: true,
        }
    }

    fn ambiguous(source: io::Error) -> Self {
        Self {
            source,
            safe_to_retry: false,
        }
    }
}

pub(crate) struct UnavailableAuditSink;

impl AuditSink for UnavailableAuditSink {
    fn record(&self, _event: &AuditEvent) -> Result<(), AuditError> {
        Err(AuditError::Unavailable)
    }
}

/// Bounded in-memory audit used by an explicitly ephemeral broker.
pub(crate) struct MemoryAuditSink {
    events: Mutex<VecDeque<AuditEvent>>,
}

impl MemoryAuditSink {
    pub(crate) fn new() -> Self {
        Self {
            events: Mutex::new(VecDeque::new()),
        }
    }
}

impl AuditSink for MemoryAuditSink {
    fn record(&self, event: &AuditEvent) -> Result<(), AuditError> {
        let mut events = self.events.lock().map_err(|_| AuditError::Poisoned)?;
        if events.len() == MAX_MEMORY_EVENTS {
            events.pop_front();
        }
        events.push_back(event.clone());
        Ok(())
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

fn transient_io_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
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
            source.as_ptr(),
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
fn replace_file(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable audit rotation is unsupported on this platform",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(target: AuditTarget) -> AuditEvent {
        AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            request_id: RequestId::new(),
            operation_id: None,
            actor: AuditActor::Operation {
                context: ExecutionContext::standalone(Uuid::new_v4()).unwrap(),
            },
            operation: AuditOperation::ReadFile,
            target,
            outcome: AuditOutcome::Allowed,
            capability: Some(Capability::ReadFiles),
            grant_id: Some(GrantId::new()),
            error_code: None,
            item_count: None,
            bytes: Some(12),
        }
    }

    #[test]
    fn audit_targets_never_include_an_absolute_path() {
        let target = AuditTarget::selected_folder(Path::new("/Users/thet/Documents/private"));
        let encoded = serde_json::to_string(&event(target)).unwrap();
        assert!(encoded.contains("private"));
        assert!(!encoded.contains("Users"));
        assert!(!encoded.contains("Documents"));
    }

    #[test]
    fn relative_targets_are_bounded_on_utf8_boundaries() {
        let path = RelativePath::parse(&"é".repeat(512)).unwrap();
        let AuditTarget::Path { relative, .. } = AuditTarget::path(RootId::new(), &path) else {
            panic!("unexpected target")
        };
        assert!(relative.as_str().len() <= 1024);
        assert!(relative.as_str().is_char_boundary(relative.as_str().len()));
    }

    #[test]
    fn jsonl_sink_appends_private_records() {
        let temp = tempfile::tempdir().unwrap();
        let sink = JsonlAuditSink::open(temp.path()).unwrap();
        let first = event(AuditTarget::Subject);
        let second = event(AuditTarget::Root {
            root_id: RootId::new(),
        });
        sink.record(&first).unwrap();
        sink.record(&second).unwrap();
        let contents = fs::read_to_string(temp.path().join(AUDIT_FILE_NAME)).unwrap();
        let decoded = contents
            .lines()
            .map(|line| serde_json::from_str::<AuditEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(decoded, [first, second]);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(temp.path().join(AUDIT_FILE_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn memory_sink_retains_a_bounded_tail() {
        let sink = MemoryAuditSink::new();
        for _ in 0..=MAX_MEMORY_EVENTS {
            sink.record(&event(AuditTarget::Subject)).unwrap();
        }
        assert_eq!(sink.events.lock().unwrap().len(), MAX_MEMORY_EVENTS);
    }

    #[test]
    fn jsonl_sink_rotates_with_bounded_retention() {
        let temp = tempfile::tempdir().unwrap();
        let sink = JsonlAuditSink::open(temp.path())
            .unwrap()
            .with_max_file_bytes((MAX_AUDIT_RECORD_BYTES + 100) as u64);
        for _ in 0..20 {
            sink.record(&event(AuditTarget::Subject)).unwrap();
        }
        let current = fs::metadata(temp.path().join(AUDIT_FILE_NAME))
            .unwrap()
            .len();
        let previous = fs::metadata(temp.path().join(AUDIT_ARCHIVE_NAME))
            .unwrap()
            .len();
        assert!(current <= sink.max_file_bytes);
        assert!(previous <= sink.max_file_bytes);
    }

    #[test]
    fn transient_post_write_failure_rolls_back_before_retry() {
        let temp = tempfile::tempdir().unwrap();
        let sink = JsonlAuditSink::open(temp.path()).unwrap();
        sink.writer.lock().unwrap().fail_after_write_once = true;
        let expected = event(AuditTarget::Subject);
        sink.record(&expected).unwrap();

        let contents = fs::read_to_string(temp.path().join(AUDIT_FILE_NAME)).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            serde_json::from_str::<AuditEvent>(lines[0]).unwrap(),
            expected
        );
    }

    #[test]
    fn startup_repairs_an_incomplete_jsonl_tail() {
        let temp = tempfile::tempdir().unwrap();
        let first = event(AuditTarget::Subject);
        let sink = JsonlAuditSink::open(temp.path()).unwrap();
        sink.record(&first).unwrap();
        drop(sink);
        let mut file = OpenOptions::new()
            .append(true)
            .open(temp.path().join(AUDIT_FILE_NAME))
            .unwrap();
        file.write_all(br#"{"partial":"#).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let sink = JsonlAuditSink::open(temp.path()).unwrap();
        let second = event(AuditTarget::Subject);
        sink.record(&second).unwrap();
        let decoded = fs::read_to_string(temp.path().join(AUDIT_FILE_NAME))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<AuditEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(decoded, [first, second]);
    }

    #[test]
    fn interrupted_rotation_degrades_until_restart_then_recovers() {
        let temp = tempfile::tempdir().unwrap();
        let first = event(AuditTarget::Subject);
        let line_bytes = serde_json::to_vec(&first).unwrap().len() as u64 + 1;
        let sink = JsonlAuditSink::open(temp.path())
            .unwrap()
            .with_max_file_bytes(line_bytes);
        sink.record(&first).unwrap();
        sink.writer.lock().unwrap().fail_rotation_after_archive_once = true;
        assert!(matches!(
            sink.record(&event(AuditTarget::Subject)),
            Err(AuditError::PublicationAmbiguous)
        ));
        assert!(matches!(
            sink.record(&event(AuditTarget::Subject)),
            Err(AuditError::Unavailable)
        ));
        drop(sink);

        let sink = JsonlAuditSink::open(temp.path()).unwrap();
        let second = event(AuditTarget::Subject);
        sink.record(&second).unwrap();
        let archive = fs::read_to_string(temp.path().join(AUDIT_ARCHIVE_NAME)).unwrap();
        let current = fs::read_to_string(temp.path().join(AUDIT_FILE_NAME)).unwrap();
        assert_eq!(
            serde_json::from_str::<AuditEvent>(archive.trim()).unwrap(),
            first
        );
        assert_eq!(
            serde_json::from_str::<AuditEvent>(current.trim()).unwrap(),
            second
        );
    }
}
