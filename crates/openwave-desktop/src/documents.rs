//! Native conversation-source bridge.
//!
//! File paths and bytes terminate here. The webview receives a deliberately
//! small catalog/search projection and never sees source locations, indexing
//! identities, generation metadata, or canonical document payloads.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use openwave_core::{ChatId, DocumentProcessingStatus, Store};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use unicode_general_category::{get_general_category, GeneralCategory};
use uuid::Uuid;

use crate::host_access::HostAccess;
use crate::{wait_server_info, AppState};

const MAX_IMPORT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SEARCH_QUERY_CHARS: usize = 500;
const MAX_RENDERER_SNIPPET_CHARS: usize = 4_000;
const DOCUMENT_PAGE_SIZE: usize = 200;
const MAX_LIBRARY_DOCUMENTS: usize = 1_000;
const CLIENT_EXECUTOR_HEADER: &str = "x-openwave-client-executor";

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportedDocument {
    document_id: String,
    display_name: String,
    processing_status: DocumentProcessingStatus,
}

#[derive(Debug, Deserialize)]
struct IngestResponse {
    document_id: Uuid,
    processing_status: DocumentProcessingStatus,
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
    let (media_type, display_name) = import_metadata(&path)?;
    let source_bytes = tauri::async_runtime::spawn_blocking(move || read_selected_document(&path))
        .await
        .map_err(|_| "Could not read the selected document".to_owned())??;
    if source_bytes.is_empty() {
        return Err("The selected document is empty".to_owned());
    }

    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let info = wait_server_info(app_state.inner()).await?;
    let response = native_auth(
        local_client().post(format!("{}{}", info.base_url, raw_documents_path(chat_id))),
        &info,
    )
    .query(&[("title", display_name.as_str())])
    .header(reqwest::header::CONTENT_TYPE, media_type)
    .body(source_bytes)
    .send()
    .await
    .map_err(|_| "Could not import the selected document".to_owned())?;
    if !response.status().is_success() {
        return Err("Could not import the selected document".to_owned());
    }
    let accepted = response
        .json::<IngestResponse>()
        .await
        .map_err(|_| "Document import returned an invalid response".to_owned())?;
    Ok(Some(ImportedDocument {
        document_id: accepted.document_id.to_string(),
        display_name,
        processing_status: accepted.processing_status,
    }))
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

fn raw_documents_path(chat_id: ChatId) -> String {
    format!("{}/raw", documents_path(chat_id))
}

fn search_path(chat_id: ChatId) -> String {
    format!("/chats/{chat_id}/search")
}

fn native_auth(
    request: reqwest::RequestBuilder,
    info: &crate::NativeServerInfo,
) -> reqwest::RequestBuilder {
    request
        .bearer_auth(&info.token)
        .header(CLIENT_EXECUTOR_HEADER, &info.executor_token)
}

fn local_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
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

fn import_metadata(path: &Path) -> Result<(&'static str, String), String> {
    if !path.is_absolute() {
        return Err("The document picker returned an invalid file".to_owned());
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    // Map common extensions to a media type; anything else is imported as opaque
    // bytes (`application/octet-stream`). The server accepts every type — text is
    // indexed, binary is stored — so an unknown extension is never an error here.
    let media_type = match extension.as_deref() {
        Some("md" | "markdown") => "text/markdown",
        Some("txt" | "text" | "log") => "text/plain",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("yaml" | "yml") => "application/yaml",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    };
    let display_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty() && name.chars().count() <= 255 && name.chars().all(is_safe_title_char)
        })
        .ok_or_else(|| "The selected document has an invalid name".to_owned())?;
    Ok((media_type, display_name.to_owned()))
}

fn read_selected_document(path: &Path) -> Result<Vec<u8>, String> {
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
    let file = directory
        .open_with(file_name, &options)
        .map_err(|_| "Could not read the selected document".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Could not read the selected document".to_owned())?;
    if !metadata.is_file() {
        return Err("Choose a file to import".to_owned());
    }
    if metadata.len() > MAX_IMPORT_BYTES {
        return Err("Documents must be 16 MB or smaller".to_owned());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_IMPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the selected document".to_owned())?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err("Documents must be 16 MB or smaller".to_owned());
    }
    Ok(bytes)
}

fn is_safe_title_char(character: char) -> bool {
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
    fn import_metadata_exposes_only_a_bounded_filename() {
        let (media_type, name) =
            import_metadata(Path::new("/Users/private/notes/plan.md")).unwrap();
        assert_eq!(media_type, "text/markdown");
        assert_eq!(name, "plan.md");
        assert!(!name.contains("private"));
        // Known extensions map to their media type…
        assert_eq!(
            import_metadata(Path::new("/Users/private/plan.pdf"))
                .unwrap()
                .0,
            "application/pdf"
        );
        assert_eq!(
            import_metadata(Path::new("/Users/private/sheet.csv"))
                .unwrap()
                .0,
            "text/csv"
        );
        // …and any other file is imported as opaque bytes rather than rejected.
        assert_eq!(
            import_metadata(Path::new("/Users/private/archive.bin"))
                .unwrap()
                .0,
            "application/octet-stream"
        );
        assert_eq!(
            import_metadata(Path::new("/Users/private/no_extension"))
                .unwrap()
                .0,
            "application/octet-stream"
        );
        // Path and filename safety still apply regardless of type.
        assert!(import_metadata(Path::new("relative.md")).is_err());
        assert!(import_metadata(Path::new("/Users/private/bad\u{202e}txt.md")).is_err());
        for unsafe_character in ['\u{200d}', '\u{206a}', '\u{206f}', '\u{2028}', '\u{2029}'] {
            let path = format!("/Users/private/bad{unsafe_character}.md");
            assert!(import_metadata(Path::new(&path)).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn selected_document_reader_rejects_symlinks_and_growth_past_the_limit() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.md");
        std::fs::write(&target, "private").unwrap();
        let link = directory.path().join("link.md");
        symlink(&target, &link).unwrap();
        assert!(read_selected_document(&link).is_err());

        let oversized = directory.path().join("oversized.md");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MAX_IMPORT_BYTES + 1).unwrap();
        assert_eq!(
            read_selected_document(&oversized).unwrap_err(),
            "Documents must be 16 MB or smaller"
        );
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
