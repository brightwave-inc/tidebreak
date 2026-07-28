//! Native image attachment bridge.
//!
//! Image bytes terminate here. The read and the upload happen in the host
//! process; the webview receives an opaque attachment id and a few bounded
//! numbers, and never sees the pixels or the path they came from. That is what
//! keeps a compromised or merely buggy renderer from being able to exfiltrate
//! the contents of a file the user only meant to show the model.
//!
//! Choosing the file is [`crate::attachments`]'s job, because one picker serves
//! both images and documents and only the bytes can say which a file is.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use openwave_core::{ChatId, MAX_IMAGE_BYTES};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::documents::resolve_conversation_scope;
use crate::host_access::HostAccess;
use crate::{wait_server_info, AppState};

/// What the renderer learns about an attached image.
///
/// Identity, geometry, and the leaf file name the picker returned — nothing
/// else. There is no directory, no path, and no way back to the bytes from
/// here. The name is included because it is the one thing the composer cannot
/// invent for itself: without it every picked image reads as an anonymous tile,
/// and a reader with three attached cannot tell which one to remove.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachedImage {
    attachment_id: String,
    file_name: String,
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

impl AttachedImage {
    fn new(published: PublishedImageAttachment, file_name: String) -> Self {
        Self {
            attachment_id: published.attachment_id.to_string(),
            file_name,
            media_type: published.media_type,
            width: published.width,
            height: published.height,
            byte_len: published.byte_len,
        }
    }
}

/// The leaf name of the picked file, bounded and safe to render.
///
/// Only the last component crosses, so the directories the user browsed through
/// stay in the host. The character check is the same one the document import
/// applies: a name is written by whoever wrote the file, and control or
/// formatting characters in one can reorder or hide the rest of a composer
/// chip's text.
fn picked_image_name(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("The image picker returned an invalid file".to_owned());
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty()
                && name.chars().count() <= 255
                && name.chars().all(crate::documents::is_safe_title_char)
        })
        .map(str::to_owned)
        .ok_or_else(|| "The selected image has an invalid name".to_owned())
}

/// Read one image off disk and publish it, returning what the renderer may see.
///
/// Split out from the picker so a mixed selection can route a file here without
/// opening a second dialog for it — see [`crate::attachments`].
pub(crate) async fn publish_image_at(
    app_state: &Arc<AppState>,
    host_access: &HostAccess,
    requested_chat: Uuid,
    path: PathBuf,
) -> Result<AttachedImage, String> {
    let file_name = picked_image_name(&path)?;
    let bytes = tauri::async_runtime::spawn_blocking(move || read_selected_image(&path))
        .await
        .map_err(|_| "Could not read the selected image".to_owned())??;

    let chat_id = resolve_conversation_scope(host_access, requested_chat).await?;
    let info = wait_server_info(app_state).await?;
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
    Ok(AttachedImage::new(published, file_name))
}

/// Whether this file should be attached as an image rather than imported as a
/// document, decided from its leading bytes and its size.
///
/// Both halves matter. An oversized PNG is a real image that cannot be an image
/// attachment, and routing it here would refuse the file outright; treating it
/// as a document instead keeps it usable, which is what the reader wanted from
/// dropping it on the composer.
pub(crate) fn is_attachable_image(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(file_name) = path.file_name() else {
        return false;
    };
    let Ok(directory) = Dir::open_ambient_dir(parent, ambient_authority()) else {
        return false;
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let Ok(file) = directory.open_with(file_name, &options) else {
        return false;
    };
    match file.metadata() {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_IMAGE_BYTES => {}
        _ => return false,
    }
    // Enough for the longest signature checked, and no more: this runs on every
    // file in a selection, most of which are not images.
    let mut header = [0u8; 12];
    let mut read = 0;
    let mut handle = file.take(12);
    loop {
        match handle.read(&mut header[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(_) => return false,
        }
        if read == header.len() {
            break;
        }
    }
    declared_media_type(&header[..read]).is_some()
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
    fn the_renderer_projection_is_identity_geometry_and_a_leaf_name_only() {
        let attached = AttachedImage::new(
            PublishedImageAttachment {
                attachment_id: Uuid::nil(),
                media_type: "image/png".to_owned(),
                width: 800,
                height: 600,
                byte_len: 1_024,
            },
            "diagram.png".to_owned(),
        );
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
            [
                "attachmentId",
                "byteLen",
                "fileName",
                "height",
                "mediaType",
                "width"
            ]
        );
        let serialized = json.to_string();
        for forbidden in ["path", "bytes", "data", "base64", "Users", "token"] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }
    }

    #[test]
    fn the_picked_name_is_a_bounded_leaf_and_never_the_directories_above_it() {
        assert_eq!(
            picked_image_name(Path::new("/Users/private/holiday/beach.png")).unwrap(),
            "beach.png"
        );
        // A relative path cannot have come from the picker, and a name carrying
        // a bidi override could redraw the rest of the chip's text.
        assert!(picked_image_name(Path::new("beach.png")).is_err());
        assert!(picked_image_name(Path::new("/Users/private/bad\u{202e}gnp.png")).is_err());
        let overlong = format!("/Users/private/{}.png", "a".repeat(300));
        assert!(picked_image_name(Path::new(&overlong)).is_err());
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
