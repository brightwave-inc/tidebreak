//! Native image attachment bridge.
//!
//! Image bytes terminate here. The picker, the read, and the upload all happen
//! in the host process; the webview receives an opaque attachment id and a few
//! bounded numbers, and never sees the pixels or the path they came from. That
//! is what keeps a compromised or merely buggy renderer from being able to
//! exfiltrate the contents of a file the user only meant to show the model.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use openwave_core::{ChatId, MAX_IMAGE_BYTES};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::documents::resolve_conversation_scope;
use crate::host_access::HostAccess;
use crate::{wait_server_info, AppState};

/// Extensions offered in the picker.
///
/// This is a convenience filter only, never a trust decision: the server sniffs
/// the actual format from the bytes and refuses a file whose contents disagree
/// with what its name claims.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AttachImageRequest {
    chat_id: Uuid,
}

/// What the renderer learns about an attached image.
///
/// Identity plus geometry, nothing else. There is no path, no directory, and no
/// way back to the bytes from here.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachedImage {
    attachment_id: String,
    media_type: String,
    width: u32,
    height: u32,
    byte_len: u64,
}

#[derive(Debug, Deserialize)]
struct PublishedImageAttachment {
    attachment_id: Uuid,
    media_type: String,
    width: u32,
    height: u32,
    byte_len: u64,
}

/// The `{ kind, message }` shape every server error uses.
#[derive(Debug, Deserialize)]
struct ServerErrorBody {
    kind: String,
}

impl From<PublishedImageAttachment> for AttachedImage {
    fn from(published: PublishedImageAttachment) -> Self {
        Self {
            attachment_id: published.attachment_id.to_string(),
            media_type: published.media_type,
            width: published.width,
            height: published.height,
            byte_len: published.byte_len,
        }
    }
}

/// Pick one image from disk and publish it for a conversation.
///
/// Returns `None` when the user dismisses the picker, which is a normal outcome
/// rather than a failure.
#[tauri::command]
pub(crate) async fn attach_chat_image(
    app: AppHandle,
    app_state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: AttachImageRequest,
) -> Result<Option<AttachedImage>, String> {
    // Validate before presenting native consent, then resolve again after the
    // user returns so a long-lived picker cannot retain a deleted conversation.
    resolve_conversation_scope(&host_access, request.chat_id).await?;
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    let Some(path) = pick_image(&app).await? else {
        return Ok(None);
    };
    let bytes = tauri::async_runtime::spawn_blocking(move || read_selected_image(&path))
        .await
        .map_err(|_| "Could not read the selected image".to_owned())??;

    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let info = wait_server_info(app_state.inner()).await?;
    // The declared type is derived from the bytes, not the file name, so the
    // server's sniff-versus-declared check stays a genuine cross-check of two
    // independent claims rather than a comparison of one value with itself.
    let declared = declared_media_type(&bytes)
        .ok_or_else(|| "Attach a PNG, JPEG, WebP, or GIF image".to_owned())?;
    let response = crate::documents::native_auth(
        crate::documents::local_client().post(format!(
            "{}{}",
            info.base_url,
            image_attachments_path(chat_id)
        )),
        &info,
    )
    .header(reqwest::header::CONTENT_TYPE, declared)
    .body(bytes)
    .send()
    .await
    .map_err(|_| "Could not attach the selected image".to_owned())?;
    if !response.status().is_success() {
        let kind = response
            .json::<ServerErrorBody>()
            .await
            .map(|error| error.kind)
            .unwrap_or_default();
        return Err(refusal_message(&kind));
    }
    let published = response
        .json::<PublishedImageAttachment>()
        .await
        .map_err(|_| "Attaching the image returned an invalid response".to_owned())?;
    Ok(Some(AttachedImage::from(published)))
}

fn image_attachments_path(chat_id: ChatId) -> String {
    format!("/chats/{chat_id}/attachments/images")
}

/// Turn a machine-readable server refusal into something worth reading.
///
/// Each distinct reason gets its own sentence, because "that file is not an
/// image" and "that image is too large" call for different actions from the
/// user and a single generic failure would hide which one applies.
fn refusal_message(kind: &str) -> String {
    match kind {
        "image_attachment_empty" => "That image file is empty",
        "image_attachment_too_large" => "Images must be 16 MB or smaller",
        "payload_too_large" => "Images must be 16 MB or smaller",
        "image_attachment_not_an_image" => "That file is not an image",
        "image_attachment_unsupported_format" => "Attach a PNG, JPEG, WebP, or GIF image",
        "image_attachment_media_type_mismatch" | "image_attachment_media_type_required" => {
            "That file's contents do not match its type"
        }
        "image_attachment_zero_dimension" | "image_attachment_unreadable" => {
            "That image file is damaged"
        }
        "image_attachment_dimensions_too_large" => {
            "Images must be 8000 pixels or smaller on a side"
        }
        _ => "Could not attach the selected image",
    }
    .to_owned()
}

/// The media type these bytes actually are, from their magic bytes.
///
/// Deliberately independent of the file name. A `.png` holding a JPEG is
/// declared as a JPEG here and accepted; the server reaches the same conclusion
/// from the same bytes.
fn declared_media_type(bytes: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.starts_with(PNG) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP" {
        return Some("image/webp");
    }
    None
}

async fn pick_image(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = oneshot::channel();
    let mut picker = app
        .dialog()
        .file()
        .set_title("Attach an image")
        .add_filter("Images", IMAGE_EXTENSIONS);
    if let Some(window) = app.get_webview_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.pick_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await
        .map_err(|_| "The image picker closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "The image picker returned an invalid file".to_owned())
}

/// Read the picked file without following symlinks and without exceeding the
/// attachment ceiling.
///
/// Refusing symlinks is what stops a picker selection from resolving somewhere
/// the user did not choose, and the read is capped one byte past the limit so a
/// file that grows between `stat` and `read` is still refused.
fn read_selected_image(path: &Path) -> Result<Vec<u8>, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "The image picker returned an invalid file".to_owned())?;
    let file_name = path
        .file_name()
        .ok_or_else(|| "The image picker returned an invalid file".to_owned())?;
    let directory = Dir::open_ambient_dir(parent, ambient_authority())
        .map_err(|_| "Could not read the selected image".to_owned())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory
        .open_with(file_name, &options)
        .map_err(|_| "Could not read the selected image".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Could not read the selected image".to_owned())?;
    if !metadata.is_file() {
        return Err("Choose an image file".to_owned());
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err("Images must be 16 MB or smaller".to_owned());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the selected image".to_owned())?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("Images must be 16 MB or smaller".to_owned());
    }
    if bytes.is_empty() {
        return Err("That image file is empty".to_owned());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_type_comes_from_the_bytes_and_never_from_the_name() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        assert_eq!(declared_media_type(&png), Some("image/png"));
        assert_eq!(
            declared_media_type(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(declared_media_type(b"GIF89a\0\0"), Some("image/gif"));
        assert_eq!(
            declared_media_type(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        // A RIFF container that is not WebP is not silently promoted.
        assert_eq!(declared_media_type(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(declared_media_type(b"%PDF-1.7"), None);
        assert_eq!(declared_media_type(b""), None);
    }

    #[test]
    fn the_renderer_projection_is_identity_and_geometry_only() {
        let attached = AttachedImage::from(PublishedImageAttachment {
            attachment_id: Uuid::nil(),
            media_type: "image/png".to_owned(),
            width: 800,
            height: 600,
            byte_len: 1_024,
        });
        let json = serde_json::to_value(attached).unwrap();
        let mut keys = json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            ["attachmentId", "byteLen", "height", "mediaType", "width"]
        );
        let serialized = json.to_string();
        for forbidden in ["path", "bytes", "data", "base64", "Users", "token"] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }
    }

    #[test]
    fn a_renderer_cannot_widen_the_request_beyond_one_conversation() {
        let injected = serde_json::json!({
            "chatId": Uuid::nil(),
            "projectId": Uuid::nil(),
        });
        assert!(serde_json::from_value::<AttachImageRequest>(injected).is_err());
    }

    #[test]
    fn each_server_refusal_reaches_the_user_as_its_own_sentence() {
        let kinds = [
            "image_attachment_empty",
            "image_attachment_too_large",
            "image_attachment_not_an_image",
            "image_attachment_unsupported_format",
            "image_attachment_media_type_mismatch",
            "image_attachment_dimensions_too_large",
        ];
        let messages: Vec<String> = kinds.iter().map(|kind| refusal_message(kind)).collect();
        let generic = refusal_message("something_new");
        for message in &messages {
            assert_ne!(
                message, &generic,
                "a known refusal fell through to the generic message"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_image_reader_refuses_symlinks_and_growth_past_the_limit() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.png");
        std::fs::write(&target, [0x89, b'P', b'N', b'G']).unwrap();
        let link = directory.path().join("link.png");
        symlink(&target, &link).unwrap();
        assert!(read_selected_image(&link).is_err());

        let oversized = directory.path().join("oversized.png");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        assert_eq!(
            read_selected_image(&oversized).unwrap_err(),
            "Images must be 16 MB or smaller"
        );

        let empty = directory.path().join("empty.png");
        std::fs::write(&empty, []).unwrap();
        assert_eq!(
            read_selected_image(&empty).unwrap_err(),
            "That image file is empty"
        );
    }
}
