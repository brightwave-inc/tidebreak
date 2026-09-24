//! The Data and privacy surface: where a profile lives, how much disk each
//! part of it takes, a backup archive of it, and conversation exports.
//!
//! These live on the server so the CLI reaches them as well as the desktop
//! (decision 7). Decision 100 leaves PostgreSQL backups to the operator who
//! runs the database, so a backup here copies a local SQLite profile and
//! refuses anything else.

use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use tidebreak_core::{Chat, ChatTranscriptSnapshot, MessageId, Role, SessionId};
use zip::write::SimpleFileOptions;

use crate::error::ServerError;
use crate::scoped_store::ScopedStore;

/// Where the local SQLite database lives inside a desktop data directory.
pub const DATABASE_FILE: &str = "tidebreak.db";
const BLOBS_DIRECTORY: &str = "blobs";
const SCRATCH_DIRECTORY: &str = "scratch";
const OUTPUTS_DIRECTORY: &str = "outputs";
const BACKUPS_DIRECTORY: &str = "backups";

/// The version of the backup archive's layout, written into its manifest.
pub const BACKUP_FORMAT: u32 = 1;
/// The version of the JSON conversation export's shape.
pub const CONVERSATION_EXPORT_FORMAT: u32 = 1;

const BACKUP_MANIFEST: &str = "tidebreak-backup.json";
const BACKUP_README: &str = "README.txt";
const BACKUP_README_TEXT: &str = "\
This is a Tidebreak backup. It holds the database with your conversations,
memory, and settings, the files you attached, the files Tidebreak made, your
skills and plugins, folder permissions, and coding session files.
tidebreak-backup.json lists what it holds and what it leaves out.

It leaves out your keys, which stay in this computer's keychain. On another
computer, enter them again in Settings. It also leaves out logs, downloaded
engine tools, earlier backups, working files, and worktrees from before
version 0.59, which are Git checkouts.

To restore it, quit Tidebreak and rename your data folder. Do not delete it.
Create an empty folder with the old name and extract this archive into it.
Copy back anything from the renamed folder that the archive leaves out and you
still want, then open Tidebreak.
";

/// Top-level entries of the data folder a backup leaves out: the live
/// database, which the archive holds as a consistent copy instead, earlier
/// backups, logs, engine tools a feature downloads again, and files that only
/// mean something to the process that wrote them.
const BACKUP_SKIPS: [&str; 15] = [
    DATABASE_FILE,
    "tidebreak.db-wal",
    "tidebreak.db-shm",
    "tidebreak.db-journal",
    BACKUPS_DIRECTORY,
    "logs",
    "boot-failures.log",
    "tools",
    "tidebreak.lock",
    "host-broker.lock",
    "blob-locks",
    "file-preview-temp",
    "computer-use-control",
    "running.json",
    "listen.json",
];
/// Inside `code/`, the worktrees from before version 0.59. They are Git
/// checkouts; Git is what backs them up.
const LEGACY_WORKTREES_DIRECTORY: &str = "worktrees";
const CODE_DIRECTORY: &str = "code";

/// What a backup's manifest says it holds.
const BACKUP_CONTENTS: [&str; 2] = [
    "tidebreak.db, a consistent copy of the database",
    "the rest of the data folder, except what excludes names",
];
/// What a backup's manifest says it leaves out.
const BACKUP_EXCLUDES: [&str; 7] = [
    "keys, which stay in the keychain",
    "logs/ and boot-failures.log",
    "tools/, which a feature downloads again",
    "backups/",
    "scratch/<conversation>/ working files, except outputs/",
    "code/worktrees/, worktrees from before version 0.59",
    "lock files and files that describe the running app",
];

/// What `GET /data` answers: where the profile lives and what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct DataOverview {
    /// The profile's data directory, on the machine the server runs on.
    pub data_dir: String,
    /// Which database holds conversations.
    pub storage: DataStorage,
    /// Disk use by category, in a fixed order, zero-byte categories included.
    pub usage: Vec<DataUsage>,
    /// The sum of every category.
    pub total_bytes: u64,
    /// Why `POST /data/backup` cannot run on this server. Absent when it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub backup_unavailable: Option<String>,
}

/// The database a profile keeps its conversations in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum DataStorage {
    /// A local SQLite file inside the data directory.
    Sqlite,
    /// A PostgreSQL database its operator runs and backs up.
    Postgres,
}

/// How much disk one category uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct DataUsage {
    pub category: DataCategory,
    pub bytes: u64,
}

/// The parts of a data directory the settings page names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum DataCategory {
    /// `tidebreak.db` and its write-ahead log.
    Database,
    /// `blobs/`: the bytes of every file attached to a conversation.
    Attachments,
    /// `scratch/<chat>/outputs/`: the files Tidebreak produced.
    Outputs,
    /// `logs/`.
    Logs,
    /// `tools/`: the helper, runtime, and coding engine installs.
    EngineTools,
    /// `backups/`: copies taken before updates and profiles set aside.
    Backups,
    /// Everything else: working files, code session files, and state files.
    Other,
}

impl DataCategory {
    /// Every category, in the order the overview lists them.
    pub const ALL: [Self; 7] = [
        Self::Database,
        Self::Attachments,
        Self::Outputs,
        Self::Logs,
        Self::EngineTools,
        Self::Backups,
        Self::Other,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Database => 0,
            Self::Attachments => 1,
            Self::Outputs => 2,
            Self::Logs => 3,
            Self::EngineTools => 4,
            Self::Backups => 5,
            Self::Other => 6,
        }
    }
}

/// Measure a data directory by category. Symbolic links count as nothing and
/// are never followed, so a link out of the profile cannot inflate it.
#[must_use]
pub fn disk_usage(data_dir: &Path) -> Vec<DataUsage> {
    let mut totals = [0_u64; DataCategory::ALL.len()];
    let mut add = |category: DataCategory, bytes: u64| {
        totals[category.index()] = totals[category.index()].saturating_add(bytes);
    };
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            match name.to_str() {
                Some("tidebreak.db" | "tidebreak.db-wal" | "tidebreak.db-shm") => {
                    add(DataCategory::Database, tree_size(&path));
                }
                Some(BLOBS_DIRECTORY) => add(DataCategory::Attachments, tree_size(&path)),
                Some(SCRATCH_DIRECTORY) => {
                    let (outputs, rest) = scratch_sizes(&path);
                    add(DataCategory::Outputs, outputs);
                    add(DataCategory::Other, rest);
                }
                Some("logs" | "boot-failures.log") => add(DataCategory::Logs, tree_size(&path)),
                Some("tools") => add(DataCategory::EngineTools, tree_size(&path)),
                Some(BACKUPS_DIRECTORY) => add(DataCategory::Backups, tree_size(&path)),
                _ => add(DataCategory::Other, tree_size(&path)),
            }
        }
    }
    DataCategory::ALL
        .iter()
        .map(|category| DataUsage {
            category: *category,
            bytes: totals[category.index()],
        })
        .collect()
}

/// Bytes under `path`, which may be a file. Links are not followed.
fn tree_size(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| tree_size(&entry.path()))
        .fold(0, u64::saturating_add)
}

/// Split `scratch/` into the outputs each conversation kept and the rest.
fn scratch_sizes(scratch: &Path) -> (u64, u64) {
    let mut outputs = 0_u64;
    let mut rest = 0_u64;
    let Ok(chats) = std::fs::read_dir(scratch) else {
        return (0, 0);
    };
    for chat in chats.flatten() {
        let path = chat.path();
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir());
        if !is_dir {
            rest = rest.saturating_add(tree_size(&path));
            continue;
        }
        let Ok(children) = std::fs::read_dir(&path) else {
            continue;
        };
        for child in children.flatten() {
            let size = tree_size(&child.path());
            if child.file_name() == OUTPUTS_DIRECTORY {
                outputs = outputs.saturating_add(size);
            } else {
                rest = rest.saturating_add(size);
            }
        }
    }
    (outputs, rest)
}

/// A finished backup archive: an open, unnamed file rewound to its start.
pub struct BackupArchive {
    pub file: File,
    pub bytes: u64,
    /// Files the archive holds, the manifest and README included.
    pub files: u64,
}

#[derive(Serialize)]
struct BackupManifest {
    tidebreak_backup: u32,
    created_at: DateTime<Utc>,
    tidebreak_version: &'static str,
    contents: [&'static str; 2],
    excludes: [&'static str; 7],
}

/// Build a backup of the SQLite profile in `data_dir` whose database file is
/// `database`: a consistent copy of the database, taken with SQLite's own
/// `VACUUM INTO` while Tidebreak keeps running, plus the rest of the data
/// folder except [`BACKUP_SKIPS`], as one `.tar.gz`.
///
/// The copy and the archive are built under `backups/` in the data directory,
/// on the same disk as the profile. The copy is removed once it is in the
/// archive; the archive is an unnamed file that disappears when the returned
/// handle closes.
///
/// The database copy is taken first. A file added after it is harmless, and
/// one removed while the archive is written, which only happens to bytes
/// nothing points at any more, is left out.
pub async fn build_backup(database: &Path, data_dir: &Path) -> Result<BackupArchive, ServerError> {
    let backups = data_dir.join(BACKUPS_DIRECTORY);
    tidebreak_core::db::backup::create_directory(&backups).map_err(|error| {
        backup_failed(&format!("could not create {}: {error}", backups.display()))
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".backup-")
        .tempdir_in(&backups)
        .map_err(|error| backup_failed(&format!("could not make a working folder: {error}")))?;
    let snapshot = staging.path().join(DATABASE_FILE);
    tidebreak_core::db::backup::snapshot_sqlite(database, &snapshot)
        .await
        .map_err(|error| backup_failed(&error.to_string()))?;
    let archive = tempfile::tempfile_in(&backups)
        .map_err(|error| backup_failed(&format!("could not start the archive: {error}")))?;
    let data_dir = data_dir.to_owned();
    let written = tokio::task::spawn_blocking(move || {
        let written = write_backup_archive(archive, &snapshot, &data_dir, Utc::now());
        drop(staging);
        written
    })
    .await
    .map_err(|_| backup_failed("the archive worker stopped"))?
    .map_err(|error| backup_failed(&format!("could not write the archive: {error}")))?;
    Ok(written)
}

fn backup_failed(cause: &str) -> ServerError {
    ServerError::internal(format!("Tidebreak could not back up this profile: {cause}"))
}

fn write_backup_archive(
    out: File,
    snapshot: &Path,
    data_dir: &Path,
    now: DateTime<Utc>,
) -> io::Result<BackupArchive> {
    let mut tar = tar::Builder::new(GzEncoder::new(out, Compression::fast()));
    tar.follow_symlinks(false);
    let manifest = BackupManifest {
        tidebreak_backup: BACKUP_FORMAT,
        created_at: now,
        tidebreak_version: tidebreak_core::VERSION,
        contents: BACKUP_CONTENTS,
        excludes: BACKUP_EXCLUDES,
    };
    let manifest = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    append_bytes(&mut tar, BACKUP_MANIFEST, &manifest, now)?;
    append_bytes(&mut tar, BACKUP_README, BACKUP_README_TEXT.as_bytes(), now)?;
    let mut files = 2_u64;
    if append_file(&mut tar, snapshot, Path::new(DATABASE_FILE))? {
        files += 1;
    } else {
        return Err(io::Error::other("the database copy disappeared"));
    }
    for name in sorted_children(data_dir)? {
        let Some(text) = name.to_str() else {
            files += append_tree(&mut tar, &data_dir.join(&name), Path::new(&name))?;
            continue;
        };
        if BACKUP_SKIPS.contains(&text) {
            continue;
        }
        let path = data_dir.join(&name);
        files += match text {
            SCRATCH_DIRECTORY => append_outputs(&mut tar, &path)?,
            CODE_DIRECTORY => append_tree_except(
                &mut tar,
                &path,
                Path::new(CODE_DIRECTORY),
                LEGACY_WORKTREES_DIRECTORY,
            )?,
            _ => append_tree(&mut tar, &path, Path::new(&name))?,
        };
    }
    let mut out = tar.into_inner()?.finish()?;
    out.flush()?;
    out.sync_all()?;
    let bytes = out.seek(SeekFrom::End(0))?;
    out.seek(SeekFrom::Start(0))?;
    Ok(BackupArchive {
        file: out,
        bytes,
        files,
    })
}

fn append_bytes<W: Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
    now: DateTime<Utc>,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(u64::try_from(now.timestamp()).unwrap_or(0));
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
}

/// Add one regular file under `name`. `Ok(false)` when it is gone or is not a
/// regular file.
fn append_file<W: Write>(tar: &mut tar::Builder<W>, path: &Path, name: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    let mut header = tar::Header::new_gnu();
    header.set_metadata(&metadata);
    header.set_mode(0o600);
    header.set_size(metadata.len());
    header.set_cksum();
    tar.append_data(&mut header, name, file.take(metadata.len()))?;
    Ok(true)
}

/// The names in `dir`, sorted. A missing folder has none.
fn sorted_children(dir: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names: Vec<_> = entries.flatten().map(|entry| entry.file_name()).collect();
    names.sort();
    Ok(names)
}

/// Add each conversation's `outputs/` under `scratch/`, and none of its
/// working files.
fn append_outputs<W: Write>(tar: &mut tar::Builder<W>, scratch: &Path) -> io::Result<u64> {
    if !std::fs::symlink_metadata(scratch).is_ok_and(|metadata| metadata.is_dir()) {
        return Ok(0);
    }
    let mut added = 0;
    for chat in sorted_children(scratch)? {
        added += append_tree(
            tar,
            &scratch.join(&chat).join(OUTPUTS_DIRECTORY),
            &Path::new(SCRATCH_DIRECTORY)
                .join(&chat)
                .join(OUTPUTS_DIRECTORY),
        )?;
    }
    Ok(added)
}

/// Add the tree at `root` beneath `name`, leaving out its child `skip`.
fn append_tree_except<W: Write>(
    tar: &mut tar::Builder<W>,
    root: &Path,
    name: &Path,
    skip: &str,
) -> io::Result<u64> {
    if !std::fs::symlink_metadata(root).is_ok_and(|metadata| metadata.is_dir()) {
        return append_tree(tar, root, name);
    }
    let mut added = 0;
    for child in sorted_children(root)? {
        if child == skip {
            continue;
        }
        added += append_tree(tar, &root.join(&child), &name.join(&child))?;
    }
    Ok(added)
}

/// Add every regular file under `root` beneath `name`, in a stable order.
/// Links are skipped, never followed. Returns how many files it added.
fn append_tree<W: Write>(tar: &mut tar::Builder<W>, root: &Path, name: &Path) -> io::Result<u64> {
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if metadata.is_file() {
        return Ok(u64::from(append_file(tar, root, name)?));
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut children: Vec<PathBuf> = std::fs::read_dir(root)?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    children.sort();
    let mut added = 0;
    for child in children {
        let Some(file_name) = child.file_name() else {
            continue;
        };
        added += append_tree(tar, &child, &name.join(file_name))?;
    }
    Ok(added)
}

/// The two shapes a conversation export takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ConversationExportFormat {
    /// A `.zip` with one Markdown file per conversation.
    Markdown,
    /// One JSON document holding every conversation.
    Json,
}

/// Body of `POST /data/export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ConversationExportRequest {
    pub format: ConversationExportFormat,
    /// The conversations to export. Absent exports every one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chat_ids: Option<Vec<SessionId>>,
}

/// A finished conversation export.
#[derive(Debug)]
pub struct ConversationExport {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
    pub file_name: String,
    pub conversations: usize,
}

#[derive(Serialize)]
struct ConversationExportDocument {
    tidebreak_export: u32,
    exported_at: DateTime<Utc>,
    conversations: Vec<ExportedConversation>,
}

#[derive(Serialize)]
struct ExportedConversation {
    id: SessionId,
    title: Option<String>,
    created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    messages: Vec<ExportedMessage>,
}

#[derive(Serialize)]
struct ExportedMessage {
    id: MessageId,
    role: ExportedRole,
    created_at: DateTime<Utc>,
    text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<ExportedAttachment>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExportedRole {
    User,
    Assistant,
    /// A note Tidebreak wrote into the conversation between turns.
    Note,
}

#[derive(Serialize)]
struct ExportedAttachment {
    name: String,
    media_type: String,
}

/// Export the caller's conversations as the request asks.
///
/// Every id in `chat_ids` must name one of the caller's conversations; an
/// unknown one refuses the export rather than leaving it out without saying.
pub async fn export_conversations(
    store: &ScopedStore,
    request: &ConversationExportRequest,
    now: DateTime<Utc>,
) -> Result<ConversationExport, ServerError> {
    let mut chats = store.list_chats().await?;
    if let Some(ids) = &request.chat_ids {
        if ids.is_empty() {
            return Err(ServerError::bad_request(
                "choose at least one conversation to export, or leave chat_ids out to export all of them",
            ));
        }
        for id in ids {
            if !chats.iter().any(|chat| chat.id == *id) {
                return Err(ServerError::not_found(format!(
                    "conversation {id} not found"
                )));
            }
        }
        chats.retain(|chat| ids.contains(&chat.id));
    }
    chats.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    let mut conversations = Vec::with_capacity(chats.len());
    for chat in chats {
        let transcript = store.get_chat_transcript(chat.id).await?;
        conversations.push(exported_conversation(chat, transcript));
    }
    let count = conversations.len();
    let stamp = now.format("%Y-%m-%d");
    match request.format {
        ConversationExportFormat::Json => {
            let document = ConversationExportDocument {
                tidebreak_export: CONVERSATION_EXPORT_FORMAT,
                exported_at: now,
                conversations,
            };
            let bytes = serde_json::to_vec_pretty(&document).map_err(|error| {
                ServerError::internal(format!("could not encode the export: {error}"))
            })?;
            Ok(ConversationExport {
                bytes,
                content_type: "application/json",
                file_name: format!("Tidebreak conversations {stamp}.json"),
                conversations: count,
            })
        }
        ConversationExportFormat::Markdown => {
            let bytes = markdown_archive(&conversations, now).map_err(|error| {
                ServerError::internal(format!("could not write the export: {error}"))
            })?;
            Ok(ConversationExport {
                bytes,
                content_type: "application/zip",
                file_name: format!("Tidebreak conversations {stamp}.zip"),
                conversations: count,
            })
        }
    }
}

fn exported_conversation(
    chat: Chat,
    transcript: Option<ChatTranscriptSnapshot>,
) -> ExportedConversation {
    let mut messages = Vec::new();
    if let Some(transcript) = transcript {
        for message in transcript.messages {
            let role = match message.role {
                Role::User => ExportedRole::User,
                Role::Assistant => ExportedRole::Assistant,
                Role::System => ExportedRole::Note,
                Role::Tool => continue,
            };
            let mut attachments: Vec<(i32, ExportedAttachment)> = transcript
                .message_document_attachments
                .iter()
                .filter(|attachment| attachment.message_id == message.id)
                .map(|attachment| {
                    (
                        attachment.ordinal,
                        ExportedAttachment {
                            name: attachment
                                .title
                                .clone()
                                .filter(|title| !title.trim().is_empty())
                                .unwrap_or_else(|| "Untitled file".to_owned()),
                            media_type: attachment.media_type.clone(),
                        },
                    )
                })
                .collect();
            attachments.extend(
                transcript
                    .message_attachments
                    .iter()
                    .filter(|attachment| attachment.message_id == message.id)
                    .map(|attachment| {
                        (
                            attachment.ordinal,
                            ExportedAttachment {
                                name: format!("Image {}", attachment.ordinal + 1),
                                media_type: attachment.image.media_type.as_str().to_owned(),
                            },
                        )
                    }),
            );
            attachments.sort_by_key(|(ordinal, _)| *ordinal);
            let text = message.content.trim().to_owned();
            if text.is_empty() && attachments.is_empty() {
                continue;
            }
            messages.push(ExportedMessage {
                id: message.id,
                role,
                created_at: message.created_at,
                text,
                attachments: attachments
                    .into_iter()
                    .map(|(_, attachment)| attachment)
                    .collect(),
            });
        }
    }
    ExportedConversation {
        id: chat.id,
        title: chat.title.filter(|title| !title.trim().is_empty()),
        created_at: chat.created_at,
        model: chat.model,
        messages,
    }
}

fn markdown_archive(
    conversations: &[ExportedConversation],
    now: DateTime<Utc>,
) -> io::Result<Vec<u8>> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    for conversation in conversations {
        archive
            .start_file(markdown_file_name(conversation), options)
            .map_err(io::Error::other)?;
        archive.write_all(render_markdown(conversation, now).as_bytes())?;
    }
    archive
        .finish()
        .map(Cursor::into_inner)
        .map_err(io::Error::other)
}

/// `2026-09-23 Planning the launch (1a2b3c4d).md`. The short id keeps two
/// conversations with the same title apart.
fn markdown_file_name(conversation: &ExportedConversation) -> String {
    let title = conversation
        .title
        .as_deref()
        .unwrap_or("Untitled conversation");
    let mut safe: String = title
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect();
    safe = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    let safe = safe.trim_matches(['.', ' ']);
    let safe: String = safe.chars().take(80).collect();
    let safe = if safe.is_empty() {
        "Untitled conversation".to_owned()
    } else {
        safe
    };
    let id = conversation.id.to_string();
    format!(
        "{} {safe} ({}).md",
        conversation.created_at.format("%Y-%m-%d"),
        &id[..8.min(id.len())]
    )
}

fn render_markdown(conversation: &ExportedConversation, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(
        conversation
            .title
            .as_deref()
            .unwrap_or("Untitled conversation"),
    );
    out.push_str("\n\n");
    out.push_str(&format!(
        "Started {}. Exported from Tidebreak {}.\n",
        conversation.created_at.format("%Y-%m-%d %H:%M UTC"),
        now.format("%Y-%m-%d %H:%M UTC")
    ));
    if conversation.messages.is_empty() {
        out.push_str("\nThis conversation has no messages.\n");
    }
    for message in &conversation.messages {
        let time = message.created_at.format("%Y-%m-%d %H:%M UTC");
        match message.role {
            ExportedRole::User => out.push_str(&format!("\n## You · {time}\n\n")),
            ExportedRole::Assistant => out.push_str(&format!("\n## Tidebreak · {time}\n\n")),
            ExportedRole::Note => out.push_str(&format!("\n## Note · {time}\n\n")),
        }
        if !message.text.is_empty() {
            out.push_str(&message.text);
            out.push('\n');
        }
        if !message.attachments.is_empty() {
            let names = message
                .attachments
                .iter()
                .map(|attachment| attachment.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if !message.text.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("Attached: {names}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::Store as _;

    /// The schema marker the desktop lifecycle keeps beside the database.
    const SCHEMA_MARKER_FILE: &str = "tidebreak-schema.json";

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn disk_use_is_split_into_the_categories_the_page_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("tidebreak.db"), 100);
        write(&root.join("tidebreak.db-wal"), 10);
        write(&root.join("blobs/ab/cd"), 200);
        write(&root.join("scratch/chat-a/outputs/out-1/rev-1"), 300);
        write(&root.join("scratch/chat-a/turn-1/work.txt"), 7);
        write(&root.join("logs/tidebreak.log"), 40);
        write(&root.join("tools/node/bin/node"), 500);
        write(
            &root.join("backups/pre-migration-0.1.0-20260101T000000Z.db"),
            60,
        );
        write(&root.join("listen.json"), 3);
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("tools"), root.join("blobs/link")).unwrap();

        let usage = disk_usage(root);
        let bytes = |category| {
            usage
                .iter()
                .find(|entry| entry.category == category)
                .unwrap()
                .bytes
        };

        assert_eq!(
            usage.iter().map(|entry| entry.category).collect::<Vec<_>>(),
            DataCategory::ALL
        );
        assert_eq!(bytes(DataCategory::Database), 110);
        assert_eq!(
            bytes(DataCategory::Attachments),
            200,
            "links are not followed"
        );
        assert_eq!(bytes(DataCategory::Outputs), 300);
        assert_eq!(bytes(DataCategory::Logs), 40);
        assert_eq!(bytes(DataCategory::EngineTools), 500);
        assert_eq!(bytes(DataCategory::Backups), 60);
        assert_eq!(bytes(DataCategory::Other), 10);
    }

    #[test]
    fn a_missing_data_directory_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let usage = disk_usage(&dir.path().join("missing"));
        assert!(usage.iter().all(|entry| entry.bytes == 0));
        assert_eq!(usage.len(), DataCategory::ALL.len());
    }

    fn archive_entries(archive: BackupArchive) -> Vec<(String, Vec<u8>)> {
        let mut reader = tar::Archive::new(flate2::read::GzDecoder::new(archive.file));
        let mut entries = Vec::new();
        for entry in reader.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().display().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            entries.push((name, bytes));
        }
        entries
    }

    /// The archive restores into a profile that opens: the database copy
    /// holds the rows, and the bytes it points at come along. Logs, engine
    /// tools, earlier backups, and links out of the profile stay behind.
    #[tokio::test]
    async fn a_backup_holds_what_a_person_made_and_leaves_out_run_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let database = root.join(DATABASE_FILE);
        let store =
            tidebreak_core::DbStore::connect(&format!("sqlite://{}?mode=rwc", database.display()))
                .await
                .unwrap();
        store
            .set_setting("backup_probe", &serde_json::json!("kept"))
            .await
            .unwrap();
        std::fs::write(root.join(SCHEMA_MARKER_FILE), b"{}").unwrap();
        write(&root.join("blobs/ab/blob-1"), 11);
        write(&root.join("scratch/chat-a/outputs/out-1/rev-1"), 12);
        write(&root.join("scratch/chat-a/turn-1/work.txt"), 13);
        write(&root.join("logs/tidebreak.log"), 14);
        write(&root.join("tools/node/bin/node"), 15);
        write(
            &root.join("backups/pre-migration-0.1.0-20260101T000000Z.db"),
            16,
        );
        // What a person made or chose, which a restore must bring back.
        write(&root.join("skills/briefing/SKILL.md"), 17);
        write(&root.join("plugins/notes/plugin.json"), 18);
        write(&root.join("prompts/weekly.md"), 19);
        write(&root.join("host-broker-state.json"), 20);
        write(&root.join("code/private/sessions/s-1/memory/MEMORY.md"), 21);
        // A pre-0.59 worktree, a lock, and the run marker stay out.
        write(&root.join("code/worktrees/repo/ws-1/README.md"), 22);
        write(&root.join("tidebreak.lock"), 23);
        write(&root.join("running.json"), 24);
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("logs"), root.join("blobs/escape")).unwrap();

        let archive = build_backup(&database, root).await.unwrap();
        assert!(archive.bytes > 0);
        let files = archive.files;
        let entries = archive_entries(archive);
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();

        assert_eq!(
            names,
            [
                BACKUP_MANIFEST,
                BACKUP_README,
                DATABASE_FILE,
                "blobs/ab/blob-1",
                "code/private/sessions/s-1/memory/MEMORY.md",
                "host-broker-state.json",
                "plugins/notes/plugin.json",
                "prompts/weekly.md",
                "scratch/chat-a/outputs/out-1/rev-1",
                "skills/briefing/SKILL.md",
                SCHEMA_MARKER_FILE,
            ]
        );
        assert_eq!(files, 11);
        let manifest: serde_json::Value = serde_json::from_slice(&entries[0].1).unwrap();
        assert_eq!(manifest["tidebreak_backup"], BACKUP_FORMAT);

        let restored = tempfile::tempdir().unwrap();
        let copy = restored.path().join(DATABASE_FILE);
        std::fs::write(&copy, &entries[2].1).unwrap();
        let reopened =
            tidebreak_core::DbStore::connect(&format!("sqlite://{}?mode=rwc", copy.display()))
                .await
                .unwrap();
        assert_eq!(
            reopened.get_setting("backup_probe").await.unwrap(),
            Some(serde_json::json!("kept"))
        );
        // The working copy is gone once the archive holds it.
        let leftovers: Vec<_> = std::fs::read_dir(root.join(BACKUPS_DIRECTORY))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(
            leftovers,
            [std::ffi::OsString::from(
                "pre-migration-0.1.0-20260101T000000Z.db"
            )]
        );
    }

    fn conversation(title: Option<&str>) -> ExportedConversation {
        let created_at = DateTime::parse_from_rfc3339("2026-09-23T10:15:00Z")
            .unwrap()
            .with_timezone(&Utc);
        ExportedConversation {
            id: SessionId::new(),
            title: title.map(str::to_owned),
            created_at,
            model: None,
            messages: vec![
                ExportedMessage {
                    id: MessageId::new(),
                    role: ExportedRole::User,
                    created_at,
                    text: "Summarize the launch plan.".to_owned(),
                    attachments: vec![ExportedAttachment {
                        name: "plan.pdf".to_owned(),
                        media_type: "application/pdf".to_owned(),
                    }],
                },
                ExportedMessage {
                    id: MessageId::new(),
                    role: ExportedRole::Assistant,
                    created_at,
                    text: "The launch moves to Friday.".to_owned(),
                    attachments: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn markdown_names_who_said_what_and_what_was_attached() {
        let rendered = render_markdown(&conversation(Some("Launch plan")), Utc::now());
        assert!(rendered.starts_with("# Launch plan\n"));
        assert!(rendered.contains(
            "## You · 2026-09-23 10:15 UTC\n\nSummarize the launch plan.\n\nAttached: plan.pdf\n"
        ));
        assert!(rendered
            .contains("## Tidebreak · 2026-09-23 10:15 UTC\n\nThe launch moves to Friday.\n"));
    }

    #[test]
    fn a_markdown_file_name_is_safe_on_every_file_system() {
        let mut named = conversation(Some("Q3: plans / risks? <draft>"));
        let name = markdown_file_name(&named);
        assert!(name.starts_with("2026-09-23 Q3- plans - risks- -draft- ("));
        assert!(name.ends_with(").md"));
        named.title = None;
        assert!(markdown_file_name(&named).starts_with("2026-09-23 Untitled conversation ("));
    }
}
