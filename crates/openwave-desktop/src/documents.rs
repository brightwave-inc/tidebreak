//! Native conversation-source bridge.
//!
//! File paths and bytes terminate here. The webview receives a deliberately
//! small catalog/search projection and never sees source locations, indexing
//! identities, generation metadata, or canonical document payloads.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use openwave_core::{ChatId, DocumentProcessingStatus, DocumentSourceBlob, Store};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use tokio_util::io::ReaderStream;
use unicode_general_category::{get_general_category, GeneralCategory};
use uuid::Uuid;

use crate::host_access::HostAccess;
use crate::{wait_server_info, AppState};

const MAX_SEARCH_QUERY_CHARS: usize = 500;
const MAX_RENDERER_SNIPPET_CHARS: usize = 4_000;
const DOCUMENT_PAGE_SIZE: usize = 200;
const MAX_LIBRARY_DOCUMENTS: usize = 1_000;
const CLIENT_EXECUTOR_HEADER: &str = "x-openwave-client-executor";
const IMPORT_PROGRESS_EVENT: &str = "library-import-progress";
const IMPORT_HASH_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct CatalogPage {
    documents: Vec<CatalogDocument>,
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogDocument {
    document_id: Uuid,
    title: Option<String>,
    media_type: String,
    processing_status: DocumentProcessingStatus,
    searchable: bool,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryDocument {
    document_id: String,
    title: Option<String>,
    media_type: String,
    processing_status: DocumentProcessingStatus,
    searchable: bool,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryCatalog {
    documents: Vec<LibraryDocument>,
    truncated: bool,
}

impl From<CatalogDocument> for LibraryDocument {
    fn from(document: CatalogDocument) -> Self {
        Self {
            document_id: document.document_id.to_string(),
            title: document
                .title
                .and_then(|title| is_safe_renderer_text(&title, 255, false).then_some(title)),
            media_type: document.media_type,
            processing_status: document.processing_status,
            searchable: document.searchable,
            updated_at: document.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    citations: Vec<SearchCitation>,
}

#[derive(Debug, Deserialize)]
struct SearchCitation {
    document_id: Uuid,
    snippet: String,
    heading_path: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibrarySearchResult {
    document_id: String,
    snippet: String,
    heading: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportedDocument {
    document_id: String,
    display_name: String,
    processing_status: DocumentProcessingStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IngestResponse {
    pub(crate) document_id: Uuid,
    pub(crate) processing_status: DocumentProcessingStatus,
}

#[derive(Debug, Deserialize)]
struct StreamedIngestResponse {
    document_id: Uuid,
    processing_status: DocumentProcessingStatus,
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
enum LibraryImportProgressStatus {
    Queued,
    Streaming,
    Imported,
    AlreadyPresent,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryImportProgress {
    display_name: String,
    status: LibraryImportProgressStatus,
    document_id: Option<String>,
    processing_status: Option<DocumentProcessingStatus>,
    message: Option<String>,
}

struct PreparedDocumentImport {
    display_name: String,
    media_type: String,
    source_blob: DocumentSourceBlob,
    file: std::fs::File,
}

struct CompletedDocumentImport {
    document: ImportedDocument,
    already_present: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SearchLibraryRequest {
    chat_id: Uuid,
    query: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LibraryRequest {
    chat_id: Uuid,
}

#[tauri::command]
pub(crate) async fn list_library_documents(
    state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: LibraryRequest,
) -> Result<LibraryCatalog, String> {
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let info = wait_server_info(state.inner()).await?;
    let http = local_client();
    let path = documents_path(chat_id);
    let mut cursor: Option<String> = None;
    let mut documents = Vec::new();
    let truncated;
    loop {
        let mut request = http
            .get(format!("{}{}", info.base_url, path))
            .query(&[("limit", DOCUMENT_PAGE_SIZE.to_string())]);
        if let Some(cursor) = cursor.as_deref() {
            request = request.query(&[("cursor", cursor)]);
        }
        let response = native_auth(request, &info)
            .send()
            .await
            .map_err(|_| "Could not load this conversation's sources".to_owned())?;
        if !response.status().is_success() {
            return Err("Could not load this conversation's sources".to_owned());
        }
        let page = response
            .json::<CatalogPage>()
            .await
            .map_err(|_| "The source catalog returned an invalid response".to_owned())?;
        documents.extend(page.documents.into_iter().map(LibraryDocument::from));
        cursor = page.next_cursor;
        if documents.len() >= MAX_LIBRARY_DOCUMENTS || cursor.is_none() {
            let overflowed = documents.len() > MAX_LIBRARY_DOCUMENTS;
            documents.truncate(MAX_LIBRARY_DOCUMENTS);
            truncated = overflowed || cursor.is_some();
            break;
        }
    }
    Ok(LibraryCatalog {
        documents,
        truncated,
    })
}

#[tauri::command]
pub(crate) async fn search_library_documents(
    state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: SearchLibraryRequest,
) -> Result<Vec<LibrarySearchResult>, String> {
    let query = request.query.trim();
    if query.is_empty() || query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("Enter a search between 1 and 500 characters".to_owned());
    }
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let info = wait_server_info(state.inner()).await?;
    let response = native_auth(
        local_client().post(format!("{}{}", info.base_url, search_path(chat_id))),
        &info,
    )
    .json(&serde_json::json!({ "query": query, "k": 8 }))
    .send()
    .await
    .map_err(|_| "Could not search this conversation's sources".to_owned())?;
    if !response.status().is_success() {
        return Err("Could not search this conversation's sources".to_owned());
    }
    let response = response
        .json::<SearchResponse>()
        .await
        .map_err(|_| "Document search returned an invalid response".to_owned())?;
    Ok(response
        .citations
        .into_iter()
        .map(safe_search_result)
        .collect())
}

#[tauri::command]
pub(crate) async fn import_library_document(
    app: AppHandle,
    app_state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: LibraryRequest,
) -> Result<Option<ImportedDocument>, String> {
    // Validate before presenting native consent, then resolve again after the
    // user returns so a long-lived picker cannot retain a deleted conversation.
    resolve_conversation_scope(&host_access, request.chat_id).await?;
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    let Some(path) = pick_document(&app).await? else {
        return Ok(None);
    };
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let completed = import_selected_document(&app, app_state.inner(), chat_id, path).await?;
    Ok(Some(completed.document))
}

/// Select and import multiple files in one native call. Each selected file is
/// independent: a bad or vanished file is reported in its own result and never
/// prevents the rest of the batch from reaching the durable source queue.
#[tauri::command]
pub(crate) async fn import_library_documents(
    app: AppHandle,
    app_state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: LibraryRequest,
) -> Result<Option<LibraryImportBatch>, String> {
    resolve_conversation_scope(&host_access, request.chat_id).await?;
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    let Some(paths) = pick_documents(&app).await? else {
        return Ok(None);
    };

    let pending = paths
        .into_iter()
        .map(|path| {
            let display_name =
                import_display_name(&path).unwrap_or_else(|_| "Selected document".to_owned());
            (path, display_name)
        })
        .collect::<Vec<_>>();
    for (_, display_name) in &pending {
        emit_import_progress(
            &app,
            LibraryImportProgress {
                display_name: display_name.clone(),
                status: LibraryImportProgressStatus::Queued,
                document_id: None,
                processing_status: None,
                message: None,
            },
        );
    }

    let mut results = Vec::with_capacity(pending.len());
    for (path, display_name) in pending {
        let result = match resolve_conversation_scope(&host_access, request.chat_id).await {
            Ok(chat_id) => {
                match import_selected_document(&app, app_state.inner(), chat_id, path).await {
                    Ok(completed) if completed.already_present => {
                        emit_import_progress(
                            &app,
                            completed_progress(
                                &completed.document,
                                LibraryImportProgressStatus::AlreadyPresent,
                            ),
                        );
                        LibraryImportResult::AlreadyPresent {
                            document: completed.document,
                        }
                    }
                    Ok(completed) => {
                        emit_import_progress(
                            &app,
                            completed_progress(
                                &completed.document,
                                LibraryImportProgressStatus::Imported,
                            ),
                        );
                        LibraryImportResult::Imported {
                            document: completed.document,
                        }
                    }
                    Err(message) => failed_import_result(&app, display_name, message),
                }
            }
            Err(message) => failed_import_result(&app, display_name, message),
        };
        results.push(result);
    }
    Ok(Some(LibraryImportBatch { results }))
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
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
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

fn documents_path(chat_id: ChatId) -> String {
    format!("/chats/{chat_id}/documents")
}

pub(crate) fn raw_documents_path(chat_id: ChatId) -> String {
    format!("{}/raw", documents_path(chat_id))
}

fn streamed_raw_documents_path(chat_id: ChatId) -> String {
    format!("{}/raw-stream", documents_path(chat_id))
}

fn search_path(chat_id: ChatId) -> String {
    format!("/chats/{chat_id}/search")
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

async fn pick_document(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = oneshot::channel();
    // Any file may be imported: text-like formats are indexed and searchable,
    // and the rest are stored so they can still be worked with. No filter is set
    // so the native picker never greys out a file for its type.
    let mut picker = app.dialog().file().set_title("Import a document");
    if let Some(window) = app.get_webview_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.pick_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await
        .map_err(|_| "The document picker closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "The document picker returned an invalid file".to_owned())
}

async fn pick_documents(app: &AppHandle) -> Result<Option<Vec<PathBuf>>, String> {
    let (tx, rx) = oneshot::channel();
    let mut picker = app.dialog().file().set_title("Import documents");
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

fn prepare_selected_document(path: &Path) -> Result<PreparedDocumentImport, String> {
    let display_name = import_display_name(path)?;
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
    let mut file = directory
        .open_with(file_name, &options)
        .map_err(|_| "Could not read the selected document".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Could not read the selected document".to_owned())?;
    if !metadata.is_file() {
        return Err("Choose a file to import".to_owned());
    }
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
        digest.update(&buffer[..read]);
        let missing = crate::media_type::SNIFF_WINDOW_BYTES.saturating_sub(sniff_bytes.len());
        sniff_bytes.extend_from_slice(&buffer[..read.min(missing)]);
    }
    if byte_len == 0 {
        return Err("The selected document is empty".to_owned());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "Could not read the selected document".to_owned())?;
    let sha256: [u8; 32] = digest.finalize().into();
    Ok(PreparedDocumentImport {
        display_name,
        media_type: crate::media_type::sniff_media_type_for_path(&sniff_bytes, path),
        source_blob: DocumentSourceBlob::from_digest(sha256, byte_len),
        file: file.into_std(),
    })
}

async fn import_selected_document(
    app: &AppHandle,
    app_state: &Arc<AppState>,
    chat_id: ChatId,
    path: PathBuf,
) -> Result<CompletedDocumentImport, String> {
    let prepared = tauri::async_runtime::spawn_blocking(move || prepare_selected_document(&path))
        .await
        .map_err(|_| "Could not read the selected document".to_owned())??;
    emit_import_progress(
        app,
        LibraryImportProgress {
            display_name: prepared.display_name.clone(),
            status: LibraryImportProgressStatus::Streaming,
            document_id: None,
            processing_status: None,
            message: None,
        },
    );
    let info = wait_server_info(app_state).await?;
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
    .map_err(|_| "Could not import the selected document".to_owned())?;
    if !response.status().is_success() {
        return Err("Could not import the selected document".to_owned());
    }
    let accepted = response
        .json::<StreamedIngestResponse>()
        .await
        .map_err(|_| "Document import returned an invalid response".to_owned())?;
    Ok(CompletedDocumentImport {
        document: ImportedDocument {
            document_id: accepted.document_id.to_string(),
            display_name: prepared.display_name,
            processing_status: accepted.processing_status,
        },
        already_present: accepted.already_present,
    })
}

fn encode_sha256(sha256: [u8; 32]) -> String {
    sha256.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn completed_progress(
    document: &ImportedDocument,
    status: LibraryImportProgressStatus,
) -> LibraryImportProgress {
    LibraryImportProgress {
        display_name: document.display_name.clone(),
        status,
        document_id: Some(document.document_id.clone()),
        processing_status: Some(document.processing_status),
        message: None,
    }
}

fn failed_import_result(
    app: &AppHandle,
    display_name: String,
    message: String,
) -> LibraryImportResult {
    emit_import_progress(
        app,
        LibraryImportProgress {
            display_name: display_name.clone(),
            status: LibraryImportProgressStatus::Failed,
            document_id: None,
            processing_status: None,
            message: Some(message.clone()),
        },
    );
    LibraryImportResult::Failed {
        display_name,
        message,
    }
}

fn emit_import_progress(app: &AppHandle, progress: LibraryImportProgress) {
    if let Err(error) = app.emit(IMPORT_PROGRESS_EVENT, progress) {
        eprintln!("openwave-desktop: could not emit import progress: {error}");
    }
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

fn safe_search_result(citation: SearchCitation) -> LibrarySearchResult {
    let snippet = citation
        .snippet
        .chars()
        .filter(|character| {
            is_safe_title_char(*character) || matches!(character, '\n' | '\r' | '\t')
        })
        .take(MAX_RENDERER_SNIPPET_CHARS)
        .collect();
    LibrarySearchResult {
        document_id: citation.document_id.to_string(),
        snippet,
        heading: citation
            .heading_path
            .last()
            .filter(|heading| is_safe_renderer_text(heading.trim(), 200, false))
            .map(|heading| heading.trim().to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chat_record_is_the_only_authority_for_source_scope() {
        use openwave_core::{Chat, DbStore, Project, ProjectId};

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
        assert_eq!(
            search_path(standalone.id),
            format!("/chats/{}/search", standalone.id)
        );

        let injected = serde_json::json!({
            "chatId": standalone.id,
            "projectId": project.id,
        });
        assert!(serde_json::from_value::<LibraryRequest>(injected).is_err());
        let injected_search = serde_json::json!({
            "chatId": standalone.id,
            "projectId": project.id,
            "query": "notes",
        });
        assert!(serde_json::from_value::<SearchLibraryRequest>(injected_search).is_err());
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
                        processing_status: DocumentProcessingStatus::Queued,
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

    #[cfg(unix)]
    #[test]
    fn selected_document_preparation_rejects_symlinks_and_keeps_large_files_streamable() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.md");
        std::fs::write(&target, "private").unwrap();
        let link = directory.path().join("link.md");
        symlink(&target, &link).unwrap();
        assert!(prepare_selected_document(&link).is_err());

        let large = directory.path().join("large.md");
        let file = std::fs::File::create(&large).unwrap();
        let large_len = 16 * 1024 * 1024 + 1;
        file.set_len(large_len).unwrap();
        let prepared = prepare_selected_document(&large).unwrap();
        assert_eq!(
            prepared.source_blob.byte_len, large_len,
            "preparation hashes the file but leaves its bytes on disk for the upload stream"
        );
        assert_eq!(prepared.file.metadata().unwrap().len(), large_len);
    }

    #[test]
    fn renderer_search_projection_drops_canonical_metadata_and_bounds_text() {
        let result = safe_search_result(SearchCitation {
            document_id: Uuid::nil(),
            snippet: format!("{}\u{202e}", "x".repeat(MAX_RENDERER_SNIPPET_CHARS + 20)),
            heading_path: vec!["Private heading".to_owned()],
        });
        let json = serde_json::to_value(result).unwrap();
        assert_eq!(
            json["snippet"].as_str().unwrap().chars().count(),
            MAX_RENDERER_SNIPPET_CHARS
        );
        assert_eq!(json["heading"], "Private heading");
        assert!(!json["snippet"].as_str().unwrap().contains('\u{202e}'));
        let serialized = json.to_string();
        for forbidden in [
            "chunk", "span", "score", "region", "revision", "token", "path",
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    fn renderer_catalog_projection_has_a_closed_safe_shape() {
        let document = LibraryDocument::from(CatalogDocument {
            document_id: Uuid::nil(),
            title: Some("notes.md".to_owned()),
            media_type: "text/markdown".to_owned(),
            processing_status: DocumentProcessingStatus::Ready,
            searchable: true,
            updated_at: "2026-07-18T00:00:00Z".to_owned(),
        });
        let json = serde_json::to_value(document).unwrap();
        let keys = json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "documentId",
                "mediaType",
                "processingStatus",
                "searchable",
                "title",
                "updatedAt"
            ]
        );
        let serialized = json.to_string();
        for forbidden in ["uri", "revision", "fingerprint", "content", "token", "path"] {
            assert!(!serialized.contains(forbidden));
        }
    }
}
