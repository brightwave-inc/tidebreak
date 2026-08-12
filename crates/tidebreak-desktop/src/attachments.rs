//! One attach gesture, routed by what the bytes actually are.
//!
//! The composer offers a single button for putting a file into a conversation.
//! Underneath there are still two destinations, because they are two different
//! capabilities: an image attachment reaches the model as pixels, while an
//! imported document is parsed into text that can be searched and cited. A
//! reader holding a screenshot should not have to know which of those the app
//! will do before they can choose a button.
//!
//! The routing has to happen here rather than in the renderer. Picked paths are
//! deliberately never serialized to the webview, so the renderer has nothing to
//! decide on; it receives only the two sets of outcomes.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::documents::{
    import_document_paths, pick_documents, LibraryImportBatch, PendingLibraryDrop,
};
use crate::host_access::HostAccess;
use crate::image_attachments::{is_attachable_image, publish_image_at, AttachedImage};
use crate::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AttachFilesRequest {
    chat_id: Uuid,
}

/// What one mixed selection produced.
///
/// The two halves are reported separately because the composer shows them
/// differently — images as removable chips it can preview, documents as the
/// conversation's sources — and because a failure in one says nothing about
/// the other.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachedFiles {
    images: Vec<AttachedImage>,
    /// Absent when the selection held no documents, which is not the same as a
    /// selection whose documents all failed.
    documents: Option<LibraryImportBatch>,
    /// Images that could not be published, by file name. Documents carry their
    /// own per-file outcome inside the batch; images have no such envelope, so
    /// their failures are reported here rather than sinking the whole call.
    failed_images: Vec<FailedImage>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FailedImage {
    file_name: String,
    message: String,
}

/// Select any files and put each where it belongs.
///
/// One picker, unfiltered — the document picker never filtered either, so
/// nothing the reader could previously choose is greyed out now.
#[tauri::command]
pub(crate) async fn attach_chat_files(
    app: AppHandle,
    app_state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: AttachFilesRequest,
) -> Result<Option<AttachedFiles>, String> {
    // Validated before the dialog and again after it, so a picker left open
    // across a deletion cannot deposit into a conversation that is gone.
    crate::documents::resolve_conversation_scope(&host_access, request.chat_id).await?;
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    let Some(paths) = pick_documents(&app).await? else {
        return Ok(None);
    };

    Ok(Some(
        attach_paths(app_state.inner(), &host_access, request.chat_id, paths).await,
    ))
}

async fn attach_paths(
    app_state: &Arc<AppState>,
    host_access: &HostAccess,
    chat_id: Uuid,
    paths: Vec<std::path::PathBuf>,
) -> AttachedFiles {
    let (image_paths, document_paths): (Vec<_>, Vec<_>) = paths
        .into_iter()
        .partition(|path| is_attachable_image(path));
    let mut images = Vec::new();
    let mut failed_images = Vec::new();
    for path in image_paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image")
            .to_owned();
        // One bad image must not cost the reader the rest of the selection, so
        // each is reported on its own terms — the same rule the document batch
        // already follows.
        match publish_image_at(app_state, host_access, chat_id, path).await {
            Ok(attached) => images.push(attached),
            Err(message) => failed_images.push(FailedImage {
                file_name: name,
                message,
            }),
        }
    }

    let documents = if document_paths.is_empty() {
        None
    } else {
        Some(import_document_paths(app_state, host_access, chat_id, document_paths).await)
    };

    AttachedFiles {
        images,
        documents,
        failed_images,
    }
}

/// Claim the most recent operating-system drop for this window and attach it
/// through the same image/document routing as the picker.
#[tauri::command]
pub(crate) async fn attach_dropped_chat_files(
    app: AppHandle,
    window: tauri::WebviewWindow,
    app_state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: AttachFilesRequest,
) -> Result<Option<AttachedFiles>, String> {
    crate::documents::resolve_conversation_scope(&host_access, request.chat_id).await?;
    let paths = app
        .state::<PendingLibraryDrop>()
        .take(window.label())
        .ok_or_else(|| "Drop the files into Tidebreak again".to_owned())?;
    Ok(Some(
        attach_paths(app_state.inner(), &host_access, request.chat_id, paths).await,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moved here with the request it guards: `attach_chat_files` is now the
    /// one command that carries a conversation id in from the renderer.
    #[test]
    fn a_renderer_cannot_widen_the_request_beyond_one_conversation() {
        let injected = serde_json::json!({
            "chatId": Uuid::nil(),
            "projectId": Uuid::nil(),
        });
        assert!(serde_json::from_value::<AttachFilesRequest>(injected).is_err());
    }
}
