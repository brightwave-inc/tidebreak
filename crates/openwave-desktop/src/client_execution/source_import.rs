//! Import one file from a connected folder into the conversation's sources.
//!
//! The model proposes an opaque root and a root-relative path and gets back a
//! document id. Everything between those two facts is native: whether the chat
//! may still read that root, what the bytes actually are, what the source is
//! called, and which document the import belongs to. None of it is negotiable
//! from the model side, and the imported bytes never travel back to it.

use std::{future::Future, path::PathBuf};

use openwave_code_execution::host_paths::resolve_scratch_directory;
use openwave_core::{
    ChatId, HostRootId, ImportConnectedFileArgs, ImportConnectedFileResult, ToolCallRecord,
};
use openwave_host_broker::{
    OperationEnvelope, OperationRequest, OperationResult, PathRequest, RelativePath, RootId,
    MAX_READ_FILE_BINARY_BYTES, PROTOCOL_VERSION,
};

use crate::documents::{is_safe_title_char, local_client, native_auth, raw_documents_path};
use crate::host_access::{AuthoritativeContext, HostAccess};

use super::StoredResolution;

/// Scheme for the durable identity of a source imported from a connected root.
///
/// The identity is `{scheme}:{opaque root id}/{root-relative path}` — the same
/// pathless vocabulary the audit trail uses, and never an absolute host path.
/// Because the server derives the document id from this string and the chat,
/// importing the same file twice converges on one source instead of two.
const CONNECTED_FOLDER_URI_SCHEME: &str = "connected-folder";

/// Longest title accepted by the ingest API.
const MAX_TITLE_CHARS: usize = 255;

/// One import the native side has fully resolved from a model proposal.
pub(super) struct ImportRequest {
    root_id: RootId,
    path: RelativePath,
    title: String,
    source_uri: String,
}

/// Exact bytes selected for one import and the overlay they came from, if any.
struct SourceBytes {
    bytes: Vec<u8>,
    staged_root: Option<PathBuf>,
}

enum PublishError {
    RootUnavailable,
    StagingEnded,
    Unavailable,
}

/// Recover the canonical import from a checkpointed call.
///
/// The arguments were validated before they were durably stored; this repeats
/// the broker-side path parse so a payload the broker would reject can never
/// reach a host operation.
pub(super) fn parse(call: &ToolCallRecord) -> Result<ImportRequest, ()> {
    let args: ImportConnectedFileArgs =
        serde_json::from_value(call.arguments.clone()).map_err(|_| ())?;
    let root_id = RootId::from_uuid(args.root_id).map_err(|_| ())?;
    let path = RelativePath::parse(&args.path).map_err(|_| ())?;
    if path.is_root() {
        return Err(());
    }
    let title = title_from_path(path.as_str()).ok_or(())?;
    let source_uri = format!(
        "{CONNECTED_FOLDER_URI_SCHEME}:{}/{}",
        root_id.as_uuid(),
        path.as_str()
    );
    Ok(ImportRequest {
        root_id,
        path,
        title,
        source_uri,
    })
}

/// Read the file, confirm the chat may still see it, and publish it as a source.
pub(super) async fn execute(
    state: &HostAccess,
    app: &tauri::AppHandle,
    context: AuthoritativeContext,
    request: &ImportRequest,
) -> StoredResolution {
    let source = match read_source_bytes(state, context, request).await {
        Ok(source) if !source.bytes.is_empty() => source,
        Ok(_) => return unavailable("That file is empty, so there is nothing to import."),
        Err(resolution) => return resolution,
    };

    let media_type =
        crate::media_type::sniff_media_type(&source.bytes, Some(request.title.as_str()));
    let byte_len = source.bytes.len() as u64;
    match publish(state, app, context, request, &media_type, source).await {
        Ok(accepted) => imported(ImportConnectedFileResult::Imported {
            document_id: accepted.document_id,
            title: request.title.clone(),
            media_type,
            bytes: byte_len,
            readiness: accepted.readiness,
        }),
        Err(PublishError::RootUnavailable) => {
            unavailable("That connected folder is no longer available to this conversation.")
        }
        Err(PublishError::StagingEnded) => {
            unavailable("That file's staged changes are no longer available to import.")
        }
        Err(PublishError::Unavailable) => {
            unavailable("That file could not be added to this conversation.")
        }
    }
}

async fn read_source_bytes(
    state: &HostAccess,
    context: AuthoritativeContext,
    request: &ImportRequest,
) -> Result<SourceBytes, StoredResolution> {
    let staged_root = current_staged_root(state, context, request.root_id);
    if staged_root.is_some() {
        // Looking up staging proves only that this turn has a private copy; the
        // broker remains the live authority for whether the chat may read the
        // connected root. Check before releasing staged bytes; publication
        // checks both this authority and the selected overlay again.
        if !root_is_still_attached(state, context, request.root_id).await {
            return Err(unavailable(
                "That connected folder is no longer available to this conversation.",
            ));
        }
    }

    select_source_bytes(
        staged_root,
        &request.path,
        read_broker_source_bytes(state, context, request),
    )
    .await
}

async fn read_broker_source_bytes(
    state: &HostAccess,
    context: AuthoritativeContext,
    request: &ImportRequest,
) -> Result<Vec<u8>, StoredResolution> {
    let result = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: openwave_host_broker::RequestId::new(),
            context: context.execution,
            request: OperationRequest::ReadFileBinary(PathRequest {
                root_id: request.root_id,
                path: request.path.clone(),
            }),
        })
        .await;
    match result {
        Ok(OperationResult::ReadFileBinary(file)) => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(&file.content_base64)
                .map_err(|_| unavailable("That file could not be read."))
        }
        Ok(_) => Err(unavailable("That file could not be read.")),
        // Broker transport and host errors are deliberately not described to
        // the model; only whether the file is usable crosses back.
        Err(_) => Err(unavailable(
            "That file is not available to this conversation. It may be too large, or the folder may no longer be connected.",
        )),
    }
}

/// Select the turn's coherent file view, never falling back once staged.
///
/// A missing staged path may represent a deletion by exec. Polling the broker
/// read in that case would resurrect the pre-turn file, so the fallback future
/// is deliberately left untouched whenever an overlay was selected.
async fn select_source_bytes<BrokerRead>(
    staged_root: Option<PathBuf>,
    path: &RelativePath,
    broker_read: BrokerRead,
) -> Result<SourceBytes, StoredResolution>
where
    BrokerRead: Future<Output = Result<Vec<u8>, StoredResolution>>,
{
    match staged_root {
        Some(staged_root) => {
            let bytes = read_staged_file_bytes(&staged_root, path)
                .await
                .ok_or_else(|| {
                    unavailable(
                        "That file is not available to this conversation. It may be too large, or the folder may no longer be connected.",
                    )
                })?;
            Ok(SourceBytes {
                bytes,
                staged_root: Some(staged_root),
            })
        }
        None => broker_read.await.map(|bytes| SourceBytes {
            bytes,
            staged_root: None,
        }),
    }
}

/// Read one import from the turn's staged tree without following symlinks.
///
/// The broker's binary-read ceiling stays authoritative even though these
/// bytes do not cross its transport. The file handle is opened relative to a
/// descriptor-pinned directory, and the second length check refuses a file
/// that grows while it is being read rather than silently truncating it.
async fn read_staged_file_bytes(overlay: &std::path::Path, path: &RelativePath) -> Option<Vec<u8>> {
    let (prefix, name) = path
        .as_str()
        .rsplit_once('/')
        .map_or_else(|| ("", path.as_str()), |(prefix, name)| (prefix, name));
    if name.is_empty() {
        return None;
    }
    let directory = resolve_scratch_directory(overlay, prefix, false).await?;
    let file = directory.open_file(name).await.ok()?;
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;

        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > MAX_READ_FILE_BINARY_BYTES as u64 {
            return None;
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take((MAX_READ_FILE_BINARY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= MAX_READ_FILE_BINARY_BYTES).then_some(bytes)
    })
    .await
    .ok()
    .flatten()
}

/// Whether this chat still has live read authority for the root.
///
/// `ListRoots` is the cheapest operation that answers exactly that: the broker
/// filters its result by the same per-root read authorization an operation
/// would need, so a detached or revoked root simply stops appearing.
async fn root_is_still_attached(
    state: &HostAccess,
    context: AuthoritativeContext,
    root_id: RootId,
) -> bool {
    let result = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: openwave_host_broker::RequestId::new(),
            context: context.execution,
            request: OperationRequest::ListRoots,
        })
        .await;
    matches!(
        result,
        Ok(OperationResult::ListRoots { roots }) if roots.iter().any(|root| root.root_id == root_id)
    )
}

fn current_staged_root(
    state: &HostAccess,
    context: AuthoritativeContext,
    root_id: RootId,
) -> Option<PathBuf> {
    let root_id = HostRootId::from_uuid(root_id.as_uuid()).ok()?;
    state
        .staged_folders()?
        .staged_root(ChatId::from(context.chat_id), root_id)
}

fn selected_staging_is_current(
    state: &HostAccess,
    context: AuthoritativeContext,
    root_id: RootId,
    selected: Option<&std::path::Path>,
) -> bool {
    current_staged_root(state, context, root_id).as_deref() == selected
}

async fn publish(
    host_access: &HostAccess,
    app: &tauri::AppHandle,
    context: AuthoritativeContext,
    request: &ImportRequest,
    media_type: &str,
    source: SourceBytes,
) -> Result<crate::documents::IngestResponse, PublishError> {
    use tauri::Manager;

    let state = app.state::<std::sync::Arc<crate::AppState>>();
    let info = crate::wait_server_info(state.inner())
        .await
        .map_err(|_| PublishError::Unavailable)?;
    let SourceBytes { bytes, staged_root } = source;
    let staged_root = staged_root.as_deref();
    if !selected_staging_is_current(host_access, context, request.root_id, staged_root) {
        return Err(PublishError::StagingEnded);
    }
    let publish_request = native_auth(
        local_client().post(format!(
            "{}{}",
            info.base_url,
            raw_documents_path(ChatId::from(context.chat_id))
        )),
        &info,
    )
    .query(&[
        ("title", request.title.as_str()),
        ("uri", request.source_uri.as_str()),
    ])
    .header(reqwest::header::CONTENT_TYPE, media_type)
    .body(bytes);

    // Publication is a distinct durable effect. Reauthorize beside the POST,
    // then make one final synchronous overlay-identity check so a detach,
    // revocation, or turn teardown that won the race cannot persist stale
    // staged bytes.
    if !root_is_still_attached(host_access, context, request.root_id).await {
        return Err(PublishError::RootUnavailable);
    }
    if !selected_staging_is_current(host_access, context, request.root_id, staged_root) {
        return Err(PublishError::StagingEnded);
    }
    let response = publish_request
        .send()
        .await
        .map_err(|_| PublishError::Unavailable)?;
    if !response.status().is_success() {
        return Err(PublishError::Unavailable);
    }
    response
        .json::<crate::documents::IngestResponse>()
        .await
        .map_err(|_| PublishError::Unavailable)
}

/// Last path segment, when it is safe to show as a title.
fn title_from_path(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    (!name.is_empty()
        && name.chars().count() <= MAX_TITLE_CHARS
        && name.chars().all(is_safe_title_char))
    .then(|| name.to_owned())
}

fn imported(result: ImportConnectedFileResult) -> StoredResolution {
    // An import that reports the source it added is the whole point of the
    // card; anything that did not add one has no row to show.
    let rows = match &result {
        ImportConnectedFileResult::Imported { title, .. } => Some(serde_json::json!({
            "entries": [openwave_core::ResultEntry::new(
                openwave_core::ResultEntryKind::Source,
                title.clone(),
            )],
        })),
        _ => None,
    };
    match serde_json::to_string(&result) {
        Ok(result) => StoredResolution::Completed { result, rows },
        Err(_) => unavailable("That file could not be added to this conversation."),
    }
}

fn unavailable(message: &str) -> StoredResolution {
    let result = ImportConnectedFileResult::Unavailable {
        message: message.to_owned(),
    };
    StoredResolution::Failed {
        result: serde_json::to_string(&result)
            .unwrap_or_else(|_| r#"{"status":"unavailable","message":"unavailable"}"#.to_owned()),
        error_code: "import_unavailable".to_owned(),
        error_detail: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::{
        CallId, DocumentSourceBlob, SourceReadiness, ToolCallExecution, ToolCallStatus, TurnId,
    };

    fn import_call(path: &str, root_id: uuid::Uuid) -> ToolCallRecord {
        ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: TurnId::new(),
            provider_id: "tool-1".into(),
            name: openwave_core::IMPORT_CONNECTED_FILE_TOOL.into(),
            arguments: serde_json::json!({ "root_id": root_id, "path": path }),
            raw_arguments: None,
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: None,
        }
    }

    #[test]
    fn source_identity_is_stable_pathless_and_scoped_to_one_root() {
        let root_id = uuid::Uuid::new_v4();
        let first = parse(&import_call("reports/q3.pdf", root_id)).unwrap();
        let again = parse(&import_call("reports/q3.pdf", root_id)).unwrap();
        // The same proposal must derive the same source, or a repeat import
        // would create a second document instead of recovering the first.
        assert_eq!(first.source_uri, again.source_uri);
        assert_eq!(first.title, "q3.pdf");
        assert_eq!(
            first.source_uri,
            format!("connected-folder:{root_id}/reports/q3.pdf")
        );
        // The same relative path under a different root is a different source.
        let elsewhere = parse(&import_call("reports/q3.pdf", uuid::Uuid::new_v4())).unwrap();
        assert_ne!(first.source_uri, elsewhere.source_uri);
        // Nothing in the identity is an absolute host path.
        assert!(!first.source_uri.contains("/Users/"));
        assert!(!first.source_uri.starts_with('/'));
    }

    #[test]
    fn proposals_the_broker_would_reject_never_reach_a_host_operation() {
        let root_id = uuid::Uuid::new_v4();
        for path in ["../secret.pdf", "reports/../secret", "a\\b", "CON"] {
            assert!(parse(&import_call(path, root_id)).is_err(), "{path}");
        }
        assert!(parse(&import_call("reports/q3.pdf", uuid::Uuid::nil())).is_err());
        // A directory is not an importable file.
        assert!(parse(&import_call("", root_id)).is_err());
    }

    #[test]
    fn titles_are_bounded_leaf_names_that_cannot_carry_control_characters() {
        assert_eq!(
            title_from_path("a/b/report.pdf").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(title_from_path("report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(
            title_from_path(&format!("{}.pdf", "x".repeat(MAX_TITLE_CHARS))),
            None
        );
        for name in ["bad\u{202e}txt.pdf", "bad\u{2028}.pdf", "bad\u{200d}.pdf"] {
            assert_eq!(title_from_path(name), None, "{name}");
        }
    }

    #[test]
    fn every_failure_is_a_typed_result_that_describes_no_host_detail() {
        let StoredResolution::Failed {
            result, error_code, ..
        } = unavailable("That file could not be read.")
        else {
            panic!("import failures must be terminal");
        };
        assert_eq!(error_code, "import_unavailable");
        assert!(result.contains("\"status\":\"unavailable\""));
        assert!(!result.contains("/Users/"));

        let imported = imported(ImportConnectedFileResult::Imported {
            document_id: uuid::Uuid::new_v4(),
            title: "q3.pdf".into(),
            media_type: "application/pdf".into(),
            bytes: 1_024,
            readiness: SourceReadiness::StoredNoText,
        });
        let StoredResolution::Completed { result, .. } = imported else {
            panic!("a successful import completes");
        };
        // The model learns the source exists but has no readable text; it does
        // not learn the contents or where the file lives.
        assert!(result.contains("\"readiness\":\"stored_no_text\""));
        assert!(!result.contains("root_id"));
        assert!(!result.contains("path"));
    }

    /// Reproduces #1233: exec edits a document in the turn's private copy,
    /// then `import_connected_file` must snapshot those bytes rather than the
    /// pre-turn file that remains in the connected folder until write-back. A
    /// staged deletion must likewise stay deleted instead of falling through
    /// to those pre-turn broker bytes.
    #[tokio::test]
    async fn an_import_after_exec_reads_the_staged_document_bytes() {
        let granted = tempfile::tempdir().unwrap();
        std::fs::create_dir(granted.path().join("reports")).unwrap();
        let connected = granted.path().join("reports/q3.docx");
        std::fs::write(&connected, b"pre-turn document").unwrap();

        let scratch = tempfile::tempdir().unwrap();
        let overlay = openwave_code_execution::WriteOverlay::prepare(
            scratch.path(),
            "chat",
            &[granted.path().to_path_buf()],
        )
        .await
        .expect("a readable granted folder stages");
        let staged = overlay.slots()[0].overlay().to_path_buf();

        // What exec does before the import call in the same turn.
        std::fs::write(staged.join("reports/q3.docx"), b"staged document").unwrap();

        let path = RelativePath::parse("reports/q3.docx").unwrap();
        let broker_read = std::cell::Cell::new(false);
        let imported = select_source_bytes(Some(staged.clone()), &path, async {
            broker_read.set(true);
            Ok(b"pre-turn document".to_vec())
        })
        .await
        .expect("the staged document is importable");
        assert_eq!(imported.bytes, b"staged document");
        assert_eq!(imported.staged_root.as_deref(), Some(staged.as_path()));
        assert!(
            !broker_read.get(),
            "a staged import must not read the broker"
        );
        assert_eq!(std::fs::read(connected).unwrap(), b"pre-turn document");

        // The ingest endpoint derives its retained blob digest from these
        // exact bytes, so the durable source identifies the staged revision.
        assert_eq!(
            DocumentSourceBlob::from_bytes(&imported.bytes),
            DocumentSourceBlob::from_bytes(b"staged document")
        );
        assert_ne!(
            DocumentSourceBlob::from_bytes(&imported.bytes),
            DocumentSourceBlob::from_bytes(b"pre-turn document")
        );

        std::fs::remove_file(staged.join("reports/q3.docx")).unwrap();
        broker_read.set(false);
        let missing = select_source_bytes(Some(staged), &path, async {
            broker_read.set(true);
            Ok(b"pre-turn document".to_vec())
        })
        .await;
        let Err(StoredResolution::Failed { error_code, .. }) = missing else {
            panic!("a staged deletion must be unavailable");
        };
        assert_eq!(error_code, "import_unavailable");
        assert!(
            !broker_read.get(),
            "a staged deletion must not resurrect the broker's pre-turn file"
        );
    }
}
