//! Import one file from a connected folder into the conversation's sources.
//!
//! The model proposes an opaque root and a root-relative path and gets back a
//! document id. Everything between those two facts is native: whether the chat
//! may still read that root, what the bytes actually are, what the source is
//! called, and which document the import belongs to. None of it is negotiable
//! from the model side, and the imported bytes never travel back to it.

use openwave_core::{
    ChatId, ImportConnectedFileArgs, ImportConnectedFileResult, SourceReadiness, ToolCallRecord,
};
use openwave_host_broker::{
    OperationEnvelope, OperationRequest, OperationResult, PathRequest, RelativePath, RootId,
    PROTOCOL_VERSION,
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
    let bytes = match read_source_bytes(state, context, request).await {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => return unavailable("That file is empty, so there is nothing to import."),
        Err(resolution) => return resolution,
    };

    // The broker reauthorized before it released these bytes, but publishing is
    // a separate, later effect. Confirm the root is still attached to this chat
    // immediately before the source is created, so a detach or revocation that
    // won the race discards the bytes instead of persisting them.
    if !root_is_still_attached(state, context, request.root_id).await {
        return unavailable("That connected folder is no longer available to this conversation.");
    }

    let media_type = crate::media_type::sniff_media_type(&bytes, Some(request.title.as_str()));
    let byte_len = bytes.len() as u64;
    match publish(
        app,
        ChatId::from(context.chat_id),
        request,
        &media_type,
        bytes,
    )
    .await
    {
        Ok(accepted) => imported(ImportConnectedFileResult::Imported {
            document_id: accepted.document_id,
            title: request.title.clone(),
            media_type,
            bytes: byte_len,
            // Ingest is asynchronous by design, so this is honest about the
            // source not being usable yet rather than implying it is.
            readiness: SourceReadiness::of(accepted.processing_status, false),
        }),
        Err(()) => unavailable("That file could not be added to this conversation."),
    }
}

async fn read_source_bytes(
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

async fn publish(
    app: &tauri::AppHandle,
    chat_id: ChatId,
    request: &ImportRequest,
    media_type: &str,
    bytes: Vec<u8>,
) -> Result<crate::documents::IngestResponse, ()> {
    use tauri::Manager;

    let state = app.state::<std::sync::Arc<crate::AppState>>();
    let info = crate::wait_server_info(state.inner())
        .await
        .map_err(|_| ())?;
    let response = native_auth(
        local_client().post(format!("{}{}", info.base_url, raw_documents_path(chat_id))),
        &info,
    )
    .query(&[
        ("title", request.title.as_str()),
        ("uri", request.source_uri.as_str()),
    ])
    .header(reqwest::header::CONTENT_TYPE, media_type)
    .body(bytes)
    .send()
    .await
    .map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    response
        .json::<crate::documents::IngestResponse>()
        .await
        .map_err(|_| ())
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
    use openwave_core::{CallId, ToolCallExecution, ToolCallStatus, TurnId};

    fn import_call(path: &str, root_id: uuid::Uuid) -> ToolCallRecord {
        ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: TurnId::new(),
            provider_id: "tool-1".into(),
            name: openwave_core::IMPORT_CONNECTED_FILE_TOOL.into(),
            arguments: serde_json::json!({ "root_id": root_id, "path": path }),
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
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
            readiness: SourceReadiness::Processing,
        });
        let StoredResolution::Completed { result, .. } = imported else {
            panic!("a successful import completes");
        };
        // The model learns the source exists and that it is not ready yet; it
        // does not learn the contents or where the file lives.
        assert!(result.contains("\"readiness\":\"processing\""));
        assert!(!result.contains("root_id"));
        assert!(!result.contains("path"));
    }
}
