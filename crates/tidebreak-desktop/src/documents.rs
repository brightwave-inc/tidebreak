//! Native conversation-document bridge.
//!
//! File paths and bytes terminate here. The webview receives a deliberately
//! small attachment or viewer projection and never sees source locations or
//! canonical document payloads.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tidebreak_core::{ChatId, DocumentBlob, DocumentId, DocumentReadiness, Store};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;
use tokio_util::io::ReaderStream;
use unicode_general_category::{get_general_category, GeneralCategory};
use uuid::Uuid;

use crate::host_access::HostAccess;
use crate::{wait_server_info, AppState};

mod expansion;

const CLIENT_EXECUTOR_HEADER: &str = "x-tidebreak-client-executor";
const IMPORT_DROP_EVENT: &str = "library-import-drop-state";
const IMPORT_HASH_CHUNK_BYTES: usize = 64 * 1024;
const MAX_MESSAGE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CONCURRENT_IMPORTS: usize = 2;
const IMPORT_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(1)];
const DROPPED_PATH_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportedDocument {
    document_id: String,
    display_name: String,
    media_type: String,
    byte_len: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IngestResponse {
    pub(crate) document_id: Uuid,
    pub(crate) readiness: DocumentReadiness,
}

#[derive(Debug, Deserialize)]
struct StreamedIngestResponse {
    document_id: Uuid,
    already_present: bool,
}

/// Per-file result for one native batch import. File locations never leave the
/// native host: the renderer receives only a safe display name and status.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum LibraryImportResult {
    Imported {
        #[serde(flatten)]
        document: ImportedDocument,
    },
    AlreadyPresent {
        #[serde(flatten)]
        document: ImportedDocument,
    },
    Failed {
        #[serde(rename = "displayName")]
        display_name: String,
        message: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryImportBatch {
    results: Vec<LibraryImportResult>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum LibraryImportDropPhase {
    Enter,
    Leave,
    Dropped,
}

/// A path set by an operating-system drag operation. The renderer never
/// receives these paths: it can only claim the most recent drop for its own
/// window, and only once.
#[derive(Default)]
pub(crate) struct PendingLibraryDrop {
    paths: std::sync::Mutex<HashMap<String, PendingDrop>>,
}

struct PendingDrop {
    paths: Vec<PathBuf>,
    received_at: Instant,
}

impl PendingLibraryDrop {
    fn record(&self, window_label: &str, paths: Vec<PathBuf>) {
        let mut pending = self.paths.lock().expect("dropped-path lock poisoned");
        pending.insert(
            window_label.to_owned(),
            PendingDrop {
                paths,
                received_at: Instant::now(),
            },
        );
    }

    fn clear(&self, window_label: &str) {
        self.paths
            .lock()
            .expect("dropped-path lock poisoned")
            .remove(window_label);
    }

    pub(crate) fn take(&self, window_label: &str) -> Option<Vec<PathBuf>> {
        let pending = self
            .paths
            .lock()
            .expect("dropped-path lock poisoned")
            .remove(window_label)?;
        (pending.received_at.elapsed() <= DROPPED_PATH_TTL).then_some(pending.paths)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryImportDropState {
    phase: LibraryImportDropPhase,
    accepted: bool,
    file_count: usize,
}

struct PendingDocumentImport {
    display_name: String,
    source: PendingDocumentSource,
}

enum PendingDocumentSource {
    File(DocumentImportSource),
    Failed(String),
}

#[derive(Clone)]
enum DocumentImportSource {
    Path(PathBuf),
    Open(Arc<std::fs::File>),
}

struct PreparedDocumentImport {
    display_name: String,
    media_type: String,
    source_blob: DocumentBlob,
    file: std::fs::File,
}

struct CompletedDocumentImport {
    document: ImportedDocument,
    already_present: bool,
}

struct CancelExpansionOnDrop {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl CancelExpansionOnDrop {
    fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            cancelled,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelExpansionOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

enum ImportFailure {
    Retryable(String),
    Permanent(String),
}

impl ImportFailure {
    fn message(self) -> String {
        match self {
            Self::Retryable(message) | Self::Permanent(message) => message,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LibraryDocumentRequest {
    chat_id: Uuid,
    document_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct DocumentTitle {
    title: Option<String>,
}

/// Save one source's original bytes wherever the reader chooses.
///
/// The renderer names the document by id and nothing else. The suggested
/// filename is derived from the stored title here, the destination is chosen
/// through the native dialog, and neither the path nor the bytes are ever
/// handed back across the boundary. Returns false when the dialog was
/// dismissed without picking a destination.
#[tauri::command]
pub(crate) async fn export_library_document(
    app: AppHandle,
    app_state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: LibraryDocumentRequest,
) -> Result<bool, String> {
    host_access
        .require_local(crate::host_authority::Authority::NativeExport)
        .await?;
    let (chat_id, document_id) =
        resolve_document_scope(&host_access, request.chat_id, request.document_id).await?;
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    let info = wait_server_info(app_state.inner()).await?;
    let response = native_auth(
        local_client().get(format!(
            "{}{}",
            info.base_url,
            document_path(chat_id, document_id)
        )),
        &info,
    )
    .send()
    .await
    .map_err(|_| "Could not open that source".to_owned())?;
    if !response.status().is_success() {
        return Err("Could not open that source".to_owned());
    }
    let detail = response
        .json::<DocumentTitle>()
        .await
        .map_err(|_| "That source returned an invalid response".to_owned())?;

    let Some(destination) = pick_export_path(
        &app,
        "Save source",
        &export_file_name(detail.title.as_deref()),
    )
    .await?
    else {
        return Ok(false);
    };
    // The dialog may stay open long enough for the conversation to be deleted.
    // Revalidate before the one host write the reader authorized.
    let (chat_id, document_id) =
        resolve_document_scope(&host_access, request.chat_id, request.document_id).await?;
    let response = native_auth(
        streaming_local_client().get(format!(
            "{}{}",
            info.base_url,
            document_file_content_path(chat_id, document_id)
        )),
        &info,
    )
    .send()
    .await
    .map_err(|_| "Could not read that source".to_owned())?;
    if !response.status().is_success() {
        return Err("Could not read that source".to_owned());
    }
    write_exported_document(&destination, response).await?;
    Ok(true)
}

/// Stream a response body into `destination`, replacing it only once complete.
///
/// The bytes go straight from the loopback response to disk rather than being
/// buffered: a source is whatever the reader imported, and some of them are
/// far larger than a copy we would want to hold in memory. The write lands in
/// a sibling temporary first, so an interrupted save cannot leave a truncated
/// file where a whole one used to be.
async fn write_exported_document(
    destination: &Path,
    mut response: reqwest::Response,
) -> Result<(), String> {
    if !destination.is_absolute() {
        return Err("The save destination is invalid".to_owned());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "The save destination is invalid".to_owned())?;
    let filename = destination
        .file_name()
        .ok_or_else(|| "The save destination is invalid".to_owned())?;
    let directory = Dir::open_ambient_dir(parent, ambient_authority())
        .map_err(|_| "Could not open the selected folder".to_owned())?;
    let permissions = match directory.symlink_metadata(filename) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Some(metadata.permissions())
        }
        Ok(_) => return Err("The selected destination is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("Could not inspect the selected destination".to_owned()),
    };
    let temporary = format!(".tidebreak-export-{}.tmp", Uuid::new_v4());
    let result =
        stream_into_temporary(&directory, &temporary, filename, permissions, &mut response).await;
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result
}

async fn stream_into_temporary(
    directory: &Dir,
    temporary: &str,
    filename: &std::ffi::OsStr,
    permissions: Option<cap_std::fs::Permissions>,
    response: &mut reqwest::Response,
) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = directory
        .open_with(temporary, &options)
        .map_err(|_| "Could not save that source".to_owned())?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)
            .map_err(|_| "Could not save that source".to_owned())?;
    }
    let mut file = tokio::fs::File::from_std(file.into_std());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read that source".to_owned())?
    {
        file.write_all(&chunk)
            .await
            .map_err(|_| "Could not save that source".to_owned())?;
    }
    file.sync_all()
        .await
        .map_err(|_| "Could not save that source".to_owned())?;
    drop(file);
    directory
        .rename(temporary, directory, filename)
        .map_err(|_| "Could not save that source".to_owned())?;
    #[cfg(unix)]
    directory
        .open(".")
        .and_then(|handle| handle.sync_all())
        .map_err(|_| "Could not save that source".to_owned())?;
    Ok(())
}

/// A filename to suggest in the save dialog, derived from the stored title.
///
/// Only a name, never a path: separators and anything that could climb out of
/// the folder the reader picks are dropped rather than escaped, and a title
/// that survives none of that falls back to a fixed name.
fn export_file_name(title: Option<&str>) -> String {
    let cleaned = title
        .unwrap_or_default()
        .chars()
        .filter(|character| {
            is_safe_title_char(*character) && !matches!(*character, '/' | '\\' | ':' | '\0')
        })
        .take(200)
        .collect::<String>();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        "document".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// Turn native drag events into a small renderer-safe state signal while
/// retaining the actual paths in process until the drop is claimed.
pub(crate) fn handle_window_drag_drop(app: &AppHandle, window_label: &str, event: &WindowEvent) {
    let WindowEvent::DragDrop(event) = event else {
        return;
    };
    let pending = app.state::<PendingLibraryDrop>();
    let state = match event {
        DragDropEvent::Enter { paths, .. } => {
            pending.clear(window_label);
            drop_state(LibraryImportDropPhase::Enter, paths)
        }
        DragDropEvent::Drop { paths, .. } => {
            let state = drop_state(LibraryImportDropPhase::Dropped, paths);
            if state.accepted {
                pending.record(window_label, paths.clone());
            } else {
                pending.clear(window_label);
            }
            state
        }
        DragDropEvent::Leave => {
            pending.clear(window_label);
            LibraryImportDropState {
                phase: LibraryImportDropPhase::Leave,
                accepted: false,
                file_count: 0,
            }
        }
        DragDropEvent::Over { .. } => return,
        _ => return,
    };
    if let Some(window) = app.get_webview_window(window_label) {
        if let Err(error) = window.emit(IMPORT_DROP_EVENT, state) {
            eprintln!("tidebreak-desktop: could not emit import drop state: {error}");
        }
    }
}

fn drop_state(phase: LibraryImportDropPhase, paths: &[PathBuf]) -> LibraryImportDropState {
    // Folder contents are expanded natively after the drop is claimed. A
    // symlink is never accepted as a shortcut to a file or directory.
    let accepted = !paths.is_empty()
        && paths.iter().all(|path| {
            std::fs::symlink_metadata(path)
                .map(|metadata| {
                    let file_type = metadata.file_type();
                    !file_type.is_symlink() && (file_type.is_file() || file_type.is_dir())
                })
                .unwrap_or(false)
        });
    LibraryImportDropState {
        phase,
        accepted,
        file_count: paths.len(),
    }
}

pub(crate) async fn import_document_paths(
    app_state: &Arc<AppState>,
    host_access: &HostAccess,
    chat_id: Uuid,
    paths: Vec<PathBuf>,
) -> LibraryImportBatch {
    let root_names = paths
        .iter()
        .map(|path| import_display_name(path).unwrap_or_else(|_| "Selected source".to_owned()))
        .collect::<Vec<_>>();
    let cancelled = Arc::new(AtomicBool::new(false));
    let task_cancelled = Arc::clone(&cancelled);
    let mut cancel_on_drop = CancelExpansionOnDrop::new(cancelled);
    let expansion = tauri::async_runtime::spawn_blocking(move || {
        expansion::expand_import_paths(paths, &|| task_cancelled.load(Ordering::Relaxed))
    })
    .await;
    cancel_on_drop.disarm();
    let (expanded, _temp_dir) = match expansion {
        Ok(Ok(expanded)) => expanded.into_parts(),
        Ok(Err(_)) => {
            return LibraryImportBatch {
                results: Vec::new(),
            }
        }
        Err(_) => (
            root_names
                .into_iter()
                .map(|display_name| expansion::ExpandedImportItem::Failure {
                    display_name,
                    message: "Could not inspect the selected source".to_owned(),
                })
                .collect(),
            None,
        ),
    };
    let pending = expanded
        .into_iter()
        .map(|item| match item {
            expansion::ExpandedImportItem::File {
                source,
                display_name,
            } => PendingDocumentImport {
                display_name,
                source: PendingDocumentSource::File(match source {
                    expansion::ExpandedFile::Path(path) => DocumentImportSource::Path(path),
                    expansion::ExpandedFile::Open(file) => {
                        DocumentImportSource::Open(Arc::new(file))
                    }
                }),
            },
            expansion::ExpandedImportItem::Failure {
                display_name,
                message,
            } => PendingDocumentImport {
                display_name,
                source: PendingDocumentSource::Failed(message),
            },
        })
        .collect::<Vec<_>>();

    let mut results = stream::iter(pending.into_iter().enumerate().map(
        |(index, import)| async move {
            let PendingDocumentImport {
                display_name,
                source,
            } = import;
            let path = match source {
                PendingDocumentSource::File(path) => path,
                PendingDocumentSource::Failed(message) => {
                    return (
                        index,
                        LibraryImportResult::Failed {
                            display_name,
                            message,
                        },
                    )
                }
            };
            let result = match resolve_conversation_scope(host_access, chat_id).await {
                Ok(chat_id) => {
                    match import_selected_document(app_state, chat_id, path, display_name.clone())
                        .await
                    {
                        Ok(completed) if completed.already_present => {
                            LibraryImportResult::AlreadyPresent {
                                document: completed.document,
                            }
                        }
                        Ok(completed) => LibraryImportResult::Imported {
                            document: completed.document,
                        },
                        Err(message) => LibraryImportResult::Failed {
                            display_name,
                            message,
                        },
                    }
                }
                Err(message) => LibraryImportResult::Failed {
                    display_name,
                    message,
                },
            };
            (index, result)
        },
    ))
    .buffer_unordered(MAX_CONCURRENT_IMPORTS)
    .collect::<Vec<_>>()
    .await;
    results.sort_by_key(|(index, _)| *index);
    LibraryImportBatch {
        results: results.into_iter().map(|(_, result)| result).collect(),
    }
}

pub(crate) async fn resolve_conversation_scope(
    host_access: &HostAccess,
    chat_id: Uuid,
) -> Result<ChatId, String> {
    if chat_id.is_nil() {
        return Err("Invalid conversation".to_owned());
    }
    let store = host_access
        .store()
        .ok_or_else(|| "Tidebreak is still starting".to_owned())?;
    resolve_conversation_scope_from_store(store.as_ref(), chat_id).await
}

async fn resolve_conversation_scope_from_store(
    store: &dyn Store,
    chat_id: Uuid,
) -> Result<ChatId, String> {
    let chat_id = ChatId::from(chat_id);
    store
        .get_chat(chat_id)
        .await
        .map_err(|_| "Could not load the conversation".to_owned())?
        .ok_or_else(|| "Conversation not found".to_owned())?;
    Ok(chat_id)
}

async fn resolve_document_scope(
    host_access: &HostAccess,
    chat_id: Uuid,
    document_id: Uuid,
) -> Result<(ChatId, DocumentId), String> {
    let chat_id = resolve_conversation_scope(host_access, chat_id).await?;
    if document_id.is_nil() {
        return Err("Invalid source".to_owned());
    }
    let document_id = DocumentId::from(document_id);
    let store = host_access
        .store()
        .ok_or_else(|| "Tidebreak is still starting".to_owned())?;
    let owned = store
        .get_document(document_id)
        .await
        .map_err(|_| "Could not load that source".to_owned())?
        .is_some_and(|document| document.chat_id == Some(chat_id) && document.project_id.is_none());
    if !owned {
        return Err("Source not found in this conversation".to_owned());
    }
    Ok((chat_id, document_id))
}

fn documents_path(chat_id: ChatId) -> String {
    format!("/chats/{chat_id}/documents")
}

fn document_path(chat_id: ChatId, document_id: DocumentId) -> String {
    format!("{}/{document_id}", documents_path(chat_id))
}

pub(crate) fn raw_documents_path(chat_id: ChatId) -> String {
    format!("{}/raw", documents_path(chat_id))
}

fn streamed_raw_documents_path(chat_id: ChatId) -> String {
    format!("{}/raw-stream", documents_path(chat_id))
}

fn document_file_content_path(chat_id: ChatId, document_id: DocumentId) -> String {
    format!("{}/file-content", document_path(chat_id, document_id))
}

pub(crate) fn native_auth(
    request: reqwest::RequestBuilder,
    info: &crate::NativeServerInfo,
) -> reqwest::RequestBuilder {
    request
        .bearer_auth(&info.token)
        .header(CLIENT_EXECUTOR_HEADER, &info.executor_token)
}

pub(crate) fn local_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("fixed local HTTP client configuration is valid")
}

fn streaming_local_client() -> reqwest::Client {
    reqwest::Client::builder()
        // Imports run only over loopback, but can reasonably outlive the
        // short request deadline appropriate for catalog/search calls.
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .expect("fixed local HTTP client configuration is valid")
}

pub(crate) async fn pick_documents(app: &AppHandle) -> Result<Option<Vec<PathBuf>>, String> {
    let (tx, rx) = oneshot::channel();
    let mut picker = app.dialog().file().set_title("Attach files");
    if let Some(window) = app.get_webview_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.pick_files(move |paths| {
        let _ = tx.send(paths);
    });
    rx.await
        .map_err(|_| "The document picker closed unexpectedly".to_owned())?
        .map(|paths| {
            paths
                .into_iter()
                .map(tauri_plugin_dialog::FilePath::into_path)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(|_| "The document picker returned an invalid file".to_owned())
}

/// Ask the reader where to write a file the app is handing back to them.
///
/// `filename` is only a suggestion the dialog opens on; the destination the
/// reader confirms is what gets written.
pub(crate) async fn pick_export_path(
    app: &AppHandle,
    dialog_title: &str,
    filename: &str,
) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = oneshot::channel();
    let mut picker = app
        .dialog()
        .file()
        .set_title(dialog_title)
        .set_file_name(filename);
    if let Some(window) = app.get_webview_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.save_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await
        .map_err(|_| "The save dialog closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "The save dialog returned an invalid destination".to_owned())
}

fn import_display_name(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("The document picker returned an invalid file".to_owned());
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty() && name.chars().count() <= 255 && name.chars().all(is_safe_title_char)
        })
        .map(str::to_owned)
        .ok_or_else(|| "The selected document has an invalid name".to_owned())
}

fn prepare_selected_document(
    source: DocumentImportSource,
    display_name: String,
) -> Result<PreparedDocumentImport, String> {
    if !is_safe_renderer_text(&display_name, 255, false) {
        return Err("The selected document has an invalid name".to_owned());
    }
    // Extension policy is decided before any bytes are read or media-sniffed:
    // an executable renamed to look like a PDF internally is still refused.
    if expansion::has_blocked_import_extension(Path::new(&display_name)) {
        return Err("This file type cannot be imported".to_owned());
    }
    let mut file = match source {
        DocumentImportSource::Path(path) => {
            let parent = path
                .parent()
                .ok_or_else(|| "The document picker returned an invalid file".to_owned())?;
            let file_name = path
                .file_name()
                .ok_or_else(|| "The document picker returned an invalid file".to_owned())?;
            let directory = Dir::open_ambient_dir(parent, ambient_authority())
                .map_err(|_| "Could not read the selected document".to_owned())?;
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            directory
                .open_with(file_name, &options)
                .map_err(|_| "Could not read the selected document".to_owned())?
                .into_std()
        }
        DocumentImportSource::Open(file) => file
            .try_clone()
            .map_err(|_| "Could not read the selected document".to_owned())?,
    };
    let metadata = file
        .metadata()
        .map_err(|_| "Could not read the selected document".to_owned())?;
    if !metadata.is_file() {
        return Err("Choose a file to import".to_owned());
    }
    if metadata.len() > MAX_MESSAGE_FILE_BYTES {
        return Err("Files must be 16 MB or smaller".to_owned());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "Could not read the selected document".to_owned())?;
    let mut digest = Sha256::new();
    let mut byte_len = 0_u64;
    let mut sniff_bytes = Vec::with_capacity(8_192);
    let mut buffer = [0_u8; IMPORT_HASH_CHUNK_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "Could not read the selected document".to_owned())?;
        if read == 0 {
            break;
        }
        byte_len = byte_len
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| "The selected document is too large".to_owned())?;
        if byte_len > MAX_MESSAGE_FILE_BYTES {
            return Err("Files must be 16 MB or smaller".to_owned());
        }
        digest.update(&buffer[..read]);
        let missing =
            tidebreak_server::media_type::SNIFF_WINDOW_BYTES.saturating_sub(sniff_bytes.len());
        sniff_bytes.extend_from_slice(&buffer[..read.min(missing)]);
    }
    if byte_len == 0 {
        return Err("The selected document is empty".to_owned());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "Could not read the selected document".to_owned())?;
    let sha256: [u8; 32] = digest.finalize().into();
    Ok(PreparedDocumentImport {
        media_type: tidebreak_server::media_type::sniff_media_type_for_path(
            &sniff_bytes,
            Path::new(&display_name),
        ),
        display_name,
        source_blob: DocumentBlob::from_digest(sha256, byte_len),
        file,
    })
}

async fn import_selected_document(
    app_state: &Arc<AppState>,
    chat_id: ChatId,
    source: DocumentImportSource,
    display_name: String,
) -> Result<CompletedDocumentImport, String> {
    for delay in IMPORT_RETRY_DELAYS
        .iter()
        .copied()
        .map(Some)
        .chain(std::iter::once(None))
    {
        match import_selected_document_once(
            app_state,
            chat_id,
            source.clone(),
            display_name.clone(),
        )
        .await
        {
            Ok(completed) => return Ok(completed),
            Err(ImportFailure::Retryable(_)) if delay.is_some() => {
                // Retrying only a transport or overloaded-server failure is
                // safe: the server deduplicates by the verified source blob.
                tokio::time::sleep(delay.expect("retry delay is present")).await;
            }
            Err(error) => return Err(error.message()),
        }
    }
    unreachable!("the final import attempt always returns")
}

async fn import_selected_document_once(
    app_state: &Arc<AppState>,
    chat_id: ChatId,
    source: DocumentImportSource,
    display_name: String,
) -> Result<CompletedDocumentImport, ImportFailure> {
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        prepare_selected_document(source, display_name)
    })
    .await
    .map_err(|_| ImportFailure::Permanent("Could not read the selected document".to_owned()))?
    .map_err(ImportFailure::Permanent)?;
    let info = wait_server_info(app_state)
        .await
        .map_err(ImportFailure::Permanent)?;
    let sha256 = encode_sha256(prepared.source_blob.sha256);
    let byte_len = prepared.source_blob.byte_len.to_string();
    let body = reqwest::Body::wrap_stream(ReaderStream::with_capacity(
        tokio::fs::File::from_std(prepared.file),
        IMPORT_HASH_CHUNK_BYTES,
    ));
    let response = native_auth(
        streaming_local_client().post(format!(
            "{}{}",
            info.base_url,
            streamed_raw_documents_path(chat_id)
        )),
        &info,
    )
    .query(&[
        ("title", prepared.display_name.as_str()),
        ("sha256", sha256.as_str()),
        ("byte_len", byte_len.as_str()),
    ])
    .header(reqwest::header::CONTENT_TYPE, &prepared.media_type)
    .body(body)
    .send()
    .await
    .map_err(|_| ImportFailure::Retryable("Could not import the selected document".to_owned()))?;
    if !response.status().is_success() {
        let message = "Could not import the selected document".to_owned();
        return if response.status().is_server_error()
            || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            Err(ImportFailure::Retryable(message))
        } else {
            Err(ImportFailure::Permanent(message))
        };
    }
    let accepted = response
        .json::<StreamedIngestResponse>()
        .await
        .map_err(|_| {
            ImportFailure::Permanent("Document import returned an invalid response".to_owned())
        })?;
    Ok(CompletedDocumentImport {
        document: ImportedDocument {
            document_id: accepted.document_id.to_string(),
            display_name: prepared.display_name,
            media_type: prepared.media_type,
            byte_len: prepared.source_blob.byte_len,
        },
        already_present: accepted.already_present,
    })
}

fn encode_sha256(sha256: [u8; 32]) -> String {
    sha256.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn is_safe_title_char(character: char) -> bool {
    !matches!(
        get_general_category(character),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    )
}

fn is_safe_renderer_text(value: &str, max_chars: usize, allow_line_breaks: bool) -> bool {
    !value.is_empty()
        && value.chars().count() <= max_chars
        && value.chars().all(|character| {
            (is_safe_title_char(character)
                || (allow_line_breaks && matches!(character, '\n' | '\r' | '\t')))
                && character != '\0'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chat_record_is_the_only_authority_for_document_scope() {
        use tidebreak_core::{Chat, DbStore, Project, ProjectId};

        let directory = tempfile::tempdir().unwrap();
        let store = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("library-scope.db").display()
        ))
        .await
        .unwrap();
        let project = Project {
            id: ProjectId::new(),
            title: Some("Project".to_owned()),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: chrono::Utc::now(),
        };
        store.create_project(&project).await.unwrap();
        let standalone = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: chrono::Utc::now(),
        };
        let project_chat = Chat {
            id: ChatId::new(),
            project_id: Some(project.id),
            ..standalone.clone()
        };
        store.create_chat(&standalone).await.unwrap();
        store.create_chat(&project_chat).await.unwrap();

        assert_eq!(
            resolve_conversation_scope_from_store(&store, standalone.id.0)
                .await
                .unwrap(),
            standalone.id
        );
        assert_eq!(
            resolve_conversation_scope_from_store(&store, project_chat.id.0)
                .await
                .unwrap(),
            project_chat.id
        );
        assert_eq!(
            documents_path(standalone.id),
            format!("/chats/{}/documents", standalone.id)
        );
        assert_eq!(
            documents_path(project_chat.id),
            format!("/chats/{}/documents", project_chat.id)
        );
        assert_eq!(
            raw_documents_path(standalone.id),
            format!("/chats/{}/documents/raw", standalone.id)
        );
        let document_id = DocumentId::new();
        assert_eq!(
            document_path(standalone.id, document_id),
            format!("/chats/{}/documents/{document_id}", standalone.id)
        );
        // Original bytes are reachable only through the owning conversation.
        let document_id = DocumentId::new();
        assert_eq!(
            document_file_content_path(standalone.id, document_id),
            format!(
                "/chats/{}/documents/{document_id}/file-content",
                standalone.id
            )
        );

        let injected_document = serde_json::json!({
            "chatId": standalone.id,
            "documentId": DocumentId::new(),
            "projectId": project.id,
        });
        assert!(serde_json::from_value::<LibraryDocumentRequest>(injected_document).is_err());
    }

    #[test]
    fn export_file_name_suggests_a_name_and_never_a_path() {
        assert_eq!(
            export_file_name(Some("Quarterly report.pdf")),
            "Quarterly report.pdf"
        );
        // A title is stored text, not a filename. Anything that could steer the
        // save dialog out of the folder the reader picked is dropped outright,
        // as are the characters filtered everywhere else on this boundary.
        assert_eq!(
            export_file_name(Some("../../.ssh/authorized_keys")),
            "sshauthorized_keys"
        );
        assert_eq!(
            export_file_name(Some("C:\\Windows\\system32")),
            "CWindowssystem32"
        );
        assert_eq!(export_file_name(Some("plan\u{202e}.md")), "plan.md");
        assert_eq!(export_file_name(Some("  ..  ")), "document");
        assert_eq!(export_file_name(None), "document");
    }

    #[test]
    fn import_display_name_exposes_only_a_bounded_filename() {
        let name = import_display_name(Path::new("/Users/private/notes/plan.md")).unwrap();
        assert_eq!(name, "plan.md");
        assert!(!name.contains("private"));
        // The title is the only thing the name decides. Media type comes from
        // the bytes, so an unfamiliar extension is never a reason to refuse.
        assert_eq!(
            import_display_name(Path::new("/Users/private/archive.bin")).unwrap(),
            "archive.bin"
        );
        assert_eq!(
            import_display_name(Path::new("/Users/private/no_extension")).unwrap(),
            "no_extension"
        );
        // Path and filename safety still apply regardless of type.
        assert!(import_display_name(Path::new("relative.md")).is_err());
        assert!(import_display_name(Path::new("/Users/private/bad\u{202e}txt.md")).is_err());
        for unsafe_character in ['\u{200d}', '\u{206a}', '\u{206f}', '\u{2028}', '\u{2029}'] {
            let path = format!("/Users/private/bad{unsafe_character}.md");
            assert!(import_display_name(Path::new(&path)).is_err());
        }
    }

    #[test]
    fn batch_import_result_reports_each_file_without_exposing_its_path() {
        let batch = LibraryImportBatch {
            results: vec![
                LibraryImportResult::Imported {
                    document: ImportedDocument {
                        document_id: Uuid::new_v4().to_string(),
                        display_name: "notes.md".to_owned(),
                        media_type: "text/markdown".to_owned(),
                        byte_len: 42,
                    },
                },
                LibraryImportResult::Failed {
                    display_name: "archive.bin".to_owned(),
                    message: "Could not import the selected document".to_owned(),
                },
            ],
        };
        let json = serde_json::to_value(batch).unwrap();
        assert_eq!(json["results"].as_array().unwrap().len(), 2);
        assert_eq!(json["results"][0]["status"], "imported");
        assert_eq!(json["results"][0]["displayName"], "notes.md");
        assert_eq!(json["results"][1]["status"], "failed");
        assert_eq!(json["results"][1]["displayName"], "archive.bin");
        assert!(json.to_string().contains("notes.md"));
        assert!(!json.to_string().contains("/Users/"));
    }

    #[test]
    fn dropped_paths_are_scoped_to_one_window_and_consumed_once() {
        let pending = PendingLibraryDrop::default();
        pending.record("main", vec![PathBuf::from("/Users/private/notes.md")]);
        pending.record("secondary", vec![PathBuf::from("/Users/private/other.md")]);

        assert_eq!(
            pending.take("main"),
            Some(vec![PathBuf::from("/Users/private/notes.md")])
        );
        assert_eq!(pending.take("main"), None);
        assert_eq!(
            pending.take("secondary"),
            Some(vec![PathBuf::from("/Users/private/other.md")])
        );
    }

    #[cfg(unix)]
    #[test]
    fn drop_state_accepts_directories_but_not_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("sources");
        std::fs::create_dir(&folder).unwrap();
        let link = directory.path().join("sources-link");
        symlink(&folder, &link).unwrap();

        assert!(drop_state(LibraryImportDropPhase::Enter, &[folder]).accepted);
        assert!(!drop_state(LibraryImportDropPhase::Enter, &[link]).accepted);
    }

    #[cfg(unix)]
    #[test]
    fn selected_document_preparation_rejects_symlinks_and_oversized_attachments() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.md");
        std::fs::write(&target, "private").unwrap();
        let link = directory.path().join("link.md");
        symlink(&target, &link).unwrap();
        assert!(
            prepare_selected_document(DocumentImportSource::Path(link), "link.md".to_owned())
                .is_err()
        );

        let large = directory.path().join("large.md");
        let file = std::fs::File::create(&large).unwrap();
        let large_len = 16 * 1024 * 1024 + 1;
        file.set_len(large_len).unwrap();
        assert_eq!(
            prepare_selected_document(DocumentImportSource::Path(large), "large.md".to_owned(),)
                .err()
                .unwrap(),
            "Files must be 16 MB or smaller"
        );
    }

    #[test]
    fn executable_extension_is_rejected_before_pdf_bytes_are_sniffed() {
        let directory = tempfile::tempdir().unwrap();
        let disguised = directory.path().join("invoice.exe");
        std::fs::write(&disguised, b"%PDF-1.7\nnot an executable").unwrap();
        assert_eq!(
            prepare_selected_document(
                DocumentImportSource::Path(disguised),
                "invoice.exe".to_owned(),
            )
            .err()
            .unwrap(),
            "This file type cannot be imported"
        );
    }
}
