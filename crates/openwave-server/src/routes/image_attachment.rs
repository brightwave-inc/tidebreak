//! Chat-scoped image attachment publication.
//!
//! One trusted boundary turns raw bytes into durable identity. What comes back
//! is an opaque content-addressed id plus bounded metadata — never a filesystem
//! path, never the pixels — so a caller can reference the attachment on a turn
//! without ever holding it.
//!
//! Two rules make this the only place an image's format is decided.
//!
//! The media type is **sniffed from the bytes**. A client-declared
//! `Content-Type` and a file extension are both attacker- or accident-supplied,
//! and a file that says `image/png` while holding something else is a mistake
//! worth surfacing, not one worth silently correcting: a declared type that
//! disagrees with the sniffed bytes is refused.
//!
//! Dimensions are read from the **image header only**. Fully decoding an
//! untrusted image to learn how big it is buys nothing and opens the classic
//! decompression bomb — a few kilobytes of deflate that expand to gigabytes of
//! pixels. The header carries the two numbers this endpoint needs.

use std::io::Cursor;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use image::{ImageFormat, ImageReader};
use serde::Serialize;
use uuid::Uuid;

use openwave_core::{
    ChatId, DocumentSourceBlob, ImageMediaType, ImageRef, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION,
};

use crate::error::ServerError;
use crate::extract::{Path, RawBytes};
use crate::routes::SERVED_BYTES_CONTENT_POLICY;
use crate::state::AppState;

/// Body limit for the publish endpoint, matching the durable per-image ceiling
/// so a request that the store would refuse never reaches a handler at all.
pub const MAX_IMAGE_ATTACHMENT_BYTES: usize = MAX_IMAGE_BYTES as usize;

/// Renderer-safe result of publishing one image.
///
/// `attachment_id` is the content-addressed blob id: opaque, derived from the
/// bytes, and revealing nothing about where they live. Everything else is a
/// small bounded number a UI can render.
#[derive(Debug, Serialize)]
pub struct PublishedImageAttachment {
    /// Opaque identity to reference on a later turn.
    pub attachment_id: Uuid,
    /// Format sniffed from the bytes.
    pub media_type: String,
    /// Pixel width read from the image header.
    pub width: u32,
    /// Pixel height read from the image header.
    pub height: u32,
    /// Size of the stored bytes.
    pub byte_len: u64,
}

impl From<ImageRef> for PublishedImageAttachment {
    fn from(image: ImageRef) -> Self {
        Self {
            attachment_id: image.blob_id,
            media_type: image.media_type.as_str().to_owned(),
            width: image.width,
            height: image.height,
            byte_len: image.byte_len,
        }
    }
}

/// `POST /chats/{chat_id}/attachments/images` — validate and durably retain one
/// image for a conversation, returning identity a turn can reference.
///
/// The bytes become a content-addressed blob immediately but gain no durable
/// reference until a turn carries them, so an attachment that is published and
/// never sent is reclaimed by the orphan sweep rather than leaking. Publishing
/// identical bytes twice yields the same id and one stored copy.
pub async fn publish_chat_image_attachment(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
    headers: HeaderMap,
    RawBytes(bytes): RawBytes,
) -> Result<impl IntoResponse, ServerError> {
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    let bytes = bytes.to_vec();
    let image = inspect_image_bytes(&bytes)?;
    require_declared_type_matches(&headers, image.media_type)?;

    // Mirrors the raw-document publish path: serialize writers for this exact
    // content address, then publish before anything can reference it, so an
    // accepted identity never points at missing bytes.
    let _blob_write = state.blob_writes.acquire(image.blob_id).await?;
    state.blobs.put(image.blob_id, bytes).await?;
    Ok((
        StatusCode::CREATED,
        crate::extract::Json(PublishedImageAttachment::from(image)),
    ))
}

/// `GET /chats/{chat_id}/attachments/images/{attachment_id}` — return pixels
/// only for an image durably attached to this conversation.
///
/// This is deliberately separate from the renderer transcript. The transcript
/// contains identity and geometry only; a renderer must present its bearer
/// token and the image must still be referenced by a message in the requested
/// chat before bytes can cross this endpoint.
pub async fn get_chat_image_attachment(
    State(state): State<AppState>,
    Path((chat_id, attachment_id)): Path<(ChatId, Uuid)>,
) -> Result<Response, ServerError> {
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    let message_image = state
        .store
        .list_message_attachments(chat_id)
        .await?
        .into_iter()
        .find(|attachment| attachment.image.blob_id == attachment_id)
        .map(|attachment| attachment.image);
    let tool_image = if message_image.is_none() {
        state
            .store
            .list_tool_calls(chat_id)
            .await?
            .into_iter()
            .filter_map(|call| call.result_preview)
            .find_map(|preview| match preview {
                openwave_core::ToolResultPreview::Exec { images, .. } => images
                    .into_iter()
                    .find(|image| image.blob_id == attachment_id),
                _ => None,
            })
    } else {
        None
    };
    let image = message_image.or(tool_image).ok_or_else(|| {
        ServerError::not_found(format!(
            "image attachment {attachment_id} not found in chat {chat_id}"
        ))
    })?;
    let bytes = state.blobs.get(image.blob_id).await?.ok_or_else(|| {
        ServerError::internal(format!(
            "image attachment {attachment_id} is missing from blob storage"
        ))
    })?;
    let actual_len = u64::try_from(bytes.len())
        .map_err(|_| ServerError::internal("image attachment byte length exceeds u64"))?;
    if actual_len != image.byte_len {
        return Err(ServerError::internal(format!(
            "image attachment {attachment_id} does not match its descriptor"
        )));
    }
    Response::builder()
        .status(StatusCode::OK)
        // Named exactly as stored, with no resolution table in between. The
        // document route needs one because its stored type is whatever the
        // ingesting caller declared; here the type is an `ImageMediaType`, a
        // closed enum of four inert raster formats that only a magic-byte
        // sniff can produce. The allowlist is the type, and it is enforced at
        // ingest rather than re-derived on every read. Naming it verbatim is
        // also required by the renderer, which compares the fetched blob's
        // type against the transcript's record and refuses to draw a
        // disagreement.
        .header(header::CONTENT_TYPE, image.media_type.as_str())
        .header(header::CONTENT_LENGTH, actual_len.to_string())
        // The bearer is never put in a URL. Do not let the resulting pixels
        // persist in an HTTP cache either; the renderer owns the object URL's
        // short lifetime instead.
        .header(header::CACHE_CONTROL, "no-store")
        // Defense in depth rather than a live hole: the bytes were sniffed at
        // ingest, so they really are the format named above and a sniffing
        // browser would reach the same answer. These say so anyway, because
        // the guarantee lives in a validation step several layers away and a
        // future format admitted there should not silently widen what a
        // browser will do with the response.
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, SERVED_BYTES_CONTENT_POLICY)
        .header(header::REFERRER_POLICY, "no-referrer")
        // `inline` is the honest answer for every format this route can serve,
        // and it is what keeps the renderer working: the fetch that feeds the
        // `<img>` ignores the disposition, but a reader who opens the URL
        // directly should see the image rather than a download.
        .header(header::CONTENT_DISPOSITION, "inline")
        .body(Body::from(bytes))
        .map_err(|error| {
            ServerError::internal(format!(
                "failed to build image attachment response: {error}"
            ))
        })
}

/// Derive durable identity from candidate image bytes.
///
/// Every refusal carries its own machine-readable `kind` so a client can tell
/// "that is not an image" from "that image is too big" and say something useful
/// about the file the user actually picked.
pub(crate) fn inspect_image_bytes(bytes: &[u8]) -> Result<ImageRef, ServerError> {
    if bytes.is_empty() {
        return Err(ServerError::bad_request_kind(
            "image_attachment_empty",
            "image attachment must not be empty",
        ));
    }
    let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if byte_len > MAX_IMAGE_BYTES {
        return Err(ServerError::bad_request_kind(
            "image_attachment_too_large",
            format!("image attachment must be at most {MAX_IMAGE_BYTES} bytes"),
        ));
    }
    let media_type = sniff_media_type(bytes)?;
    let (width, height) = read_header_dimensions(bytes, media_type)?;
    if width == 0 || height == 0 {
        return Err(ServerError::bad_request_kind(
            "image_attachment_zero_dimension",
            "image attachment has a zero width or height",
        ));
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(ServerError::bad_request_kind(
            "image_attachment_dimensions_too_large",
            format!("image attachment must be at most {MAX_IMAGE_DIMENSION} pixels on a side"),
        ));
    }
    let image = ImageRef {
        blob_id: DocumentSourceBlob::from_bytes(bytes).id,
        media_type,
        width,
        height,
        byte_len,
    };
    // The durable bounds are checked above; this is the store's own predicate,
    // kept here so the two can never drift apart unnoticed.
    image.validate().map_err(|reason| {
        ServerError::bad_request_kind("image_attachment_invalid", reason.to_owned())
    })?;
    Ok(image)
}

/// Identify the format from magic bytes, refusing anything off the allowlist.
///
/// Two distinct refusals: bytes that are not a recognizable image at all, and
/// bytes that are a real image in a format OpenWave will not forward to a
/// provider. A caller can offer to convert in the second case and cannot in the
/// first.
fn sniff_media_type(bytes: &[u8]) -> Result<ImageMediaType, ServerError> {
    let format = image::guess_format(bytes).map_err(|_| {
        ServerError::bad_request_kind(
            "image_attachment_not_an_image",
            "attachment bytes are not a recognized image",
        )
    })?;
    match format {
        ImageFormat::Png => Ok(ImageMediaType::Png),
        ImageFormat::Jpeg => Ok(ImageMediaType::Jpeg),
        ImageFormat::WebP => Ok(ImageMediaType::Webp),
        ImageFormat::Gif => Ok(ImageMediaType::Gif),
        _ => Err(ServerError::bad_request_kind(
            "image_attachment_unsupported_format",
            "image attachments must be PNG, JPEG, WebP, or GIF",
        )),
    }
}

/// Read `(width, height)` from the image header without decoding pixels.
///
/// The format comes from the sniff above rather than being guessed again, so a
/// container that lies about itself cannot route the bytes to a different
/// decoder than the one whose media type was recorded.
fn read_header_dimensions(
    bytes: &[u8],
    media_type: ImageMediaType,
) -> Result<(u32, u32), ServerError> {
    let format = match media_type {
        ImageMediaType::Png => ImageFormat::Png,
        ImageMediaType::Jpeg => ImageFormat::Jpeg,
        ImageMediaType::Webp => ImageFormat::WebP,
        ImageMediaType::Gif => ImageFormat::Gif,
    };
    ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| {
            ServerError::bad_request_kind(
                "image_attachment_unreadable",
                "image attachment header could not be read",
            )
        })
}

/// Require the declared `Content-Type` to agree with the sniffed bytes.
///
/// The header is required rather than optional: a caller that never states a
/// type can never be caught contradicting one, and the point of this check is
/// to catch the contradiction.
fn require_declared_type_matches(
    headers: &HeaderMap,
    sniffed: ImageMediaType,
) -> Result<(), ServerError> {
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ServerError::bad_request_kind(
                "image_attachment_media_type_required",
                "Content-Type header is required for an image attachment",
            )
        })?;
    if ImageMediaType::parse(declared) != Some(sniffed) {
        return Err(ServerError::bad_request_kind(
            "image_attachment_media_type_mismatch",
            format!(
                "declared Content-Type `{declared}` disagrees with the attachment's actual format `{sniffed}`"
            ),
        ));
    }
    Ok(())
}

/// A PNG that declares `width × height` and carries no pixel data at all.
///
/// The whole point of reading dimensions from the header is that the pixels are
/// never needed, so the test fixtures deliberately have none: an accepted
/// `8001 × 600` here is proof no decode happened, because 4.8 million pixels
/// cannot be hiding in sixty bytes.
#[cfg(test)]
pub(crate) fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // bit depth, color type, compression, filter, interlace
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);

    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    bytes.extend_from_slice(&png_chunk(b"IDAT", &[]));
    bytes.extend_from_slice(&png_chunk(b"IEND", &[]));
    bytes
}

#[cfg(test)]
fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut payload = kind.to_vec();
    payload.extend_from_slice(data);
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(&payload);
    chunk.extend_from_slice(&crc32(&payload).to_be_bytes());
    chunk
}

/// A one-pixel GIF whose logical screen declares `width × height`.
///
/// PNG's own decoder refuses a degenerate header outright, so a GIF is what it
/// takes to hand the endpoint a well-formed image that genuinely reports a zero
/// dimension.
#[cfg(test)]
pub(crate) fn gif_with_screen_size(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    // Global colour table present, two entries; no background, square pixels.
    bytes.extend_from_slice(&[0x80, 0x00, 0x00]);
    bytes.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    // One 1×1 frame at the origin with no local colour table.
    bytes.push(0x2c);
    bytes.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0, 0x00]);
    // LZW: minimum code size 2, then clear / pixel 0 / end-of-information.
    bytes.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    bytes.push(0x3b);
    bytes
}

#[cfg(test)]
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(error: ServerError) -> String {
        error.kind().to_owned()
    }

    #[test]
    fn identity_is_content_addressed_and_dimensions_come_from_the_header() {
        let bytes = png_header(800, 600);
        let image = inspect_image_bytes(&bytes).expect("a well-formed PNG header is accepted");
        assert_eq!(image.media_type, ImageMediaType::Png);
        assert_eq!((image.width, image.height), (800, 600));
        assert_eq!(image.byte_len, bytes.len() as u64);
        // Content-addressed: identical bytes always name the same blob, and the
        // id reveals nothing about where they came from.
        assert_eq!(
            image.blob_id,
            inspect_image_bytes(&png_header(800, 600)).unwrap().blob_id
        );
        assert_ne!(
            image.blob_id,
            inspect_image_bytes(&png_header(801, 600)).unwrap().blob_id
        );
    }

    #[test]
    fn every_refusal_has_its_own_machine_readable_reason() {
        assert_eq!(
            kind_of(inspect_image_bytes(&[]).unwrap_err()),
            "image_attachment_empty"
        );
        assert_eq!(
            kind_of(inspect_image_bytes(b"%PDF-1.7\n%not an image").unwrap_err()),
            "image_attachment_not_an_image"
        );
        // A real image in a format outside the allowlist is a different problem
        // from bytes that are not an image at all.
        assert_eq!(
            kind_of(inspect_image_bytes(b"II*\0\0\0\0\0").unwrap_err()),
            "image_attachment_unsupported_format"
        );
        assert_eq!(
            kind_of(inspect_image_bytes(b"BM\0\0\0\0\0\0\0\0\0\0\0\0").unwrap_err()),
            "image_attachment_unsupported_format"
        );
        // A well-formed image that genuinely reports a zero dimension.
        assert_eq!(
            kind_of(inspect_image_bytes(&gif_with_screen_size(0, 8)).unwrap_err()),
            "image_attachment_zero_dimension"
        );
        assert_eq!(
            kind_of(inspect_image_bytes(&gif_with_screen_size(8, 0)).unwrap_err()),
            "image_attachment_zero_dimension"
        );
        // PNG's own decoder refuses a degenerate header before dimensions come
        // back, which is a different — but still distinct — refusal.
        assert_eq!(
            kind_of(inspect_image_bytes(&png_header(0, 600)).unwrap_err()),
            "image_attachment_unreadable"
        );
        assert_eq!(
            kind_of(inspect_image_bytes(&png_header(MAX_IMAGE_DIMENSION + 1, 600)).unwrap_err()),
            "image_attachment_dimensions_too_large"
        );
        assert_eq!(
            kind_of(inspect_image_bytes(&png_header(600, MAX_IMAGE_DIMENSION + 1)).unwrap_err()),
            "image_attachment_dimensions_too_large"
        );
        // PNG magic with a corrupt header: recognized as PNG, unreadable as one.
        let mut corrupt = png_header(800, 600);
        corrupt.truncate(12);
        assert_eq!(
            kind_of(inspect_image_bytes(&corrupt).unwrap_err()),
            "image_attachment_unreadable"
        );
    }

    #[test]
    fn a_declared_type_that_disagrees_with_the_bytes_is_refused_not_corrected() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
        assert!(require_declared_type_matches(&headers, ImageMediaType::Png).is_ok());
        // Parameters and casing are noise, not disagreement.
        headers.insert(
            header::CONTENT_TYPE,
            "IMAGE/PNG; charset=binary".parse().unwrap(),
        );
        assert!(require_declared_type_matches(&headers, ImageMediaType::Png).is_ok());

        headers.insert(header::CONTENT_TYPE, "image/jpeg".parse().unwrap());
        assert_eq!(
            kind_of(require_declared_type_matches(&headers, ImageMediaType::Png).unwrap_err()),
            "image_attachment_media_type_mismatch"
        );
        // A type OpenWave does not even recognize still counts as disagreement.
        headers.insert(
            header::CONTENT_TYPE,
            "application/octet-stream".parse().unwrap(),
        );
        assert_eq!(
            kind_of(require_declared_type_matches(&headers, ImageMediaType::Png).unwrap_err()),
            "image_attachment_media_type_mismatch"
        );

        assert_eq!(
            kind_of(
                require_declared_type_matches(&HeaderMap::new(), ImageMediaType::Png).unwrap_err()
            ),
            "image_attachment_media_type_required"
        );
    }

    #[test]
    fn published_metadata_is_bounded_and_carries_no_pixels_or_paths() {
        let image = inspect_image_bytes(&png_header(64, 48)).unwrap();
        let json = serde_json::to_value(PublishedImageAttachment::from(image)).unwrap();
        let mut keys = json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            ["attachment_id", "byte_len", "height", "media_type", "width"]
        );
        assert_eq!(json["media_type"], "image/png");
        assert_eq!(json["attachment_id"], image.blob_id.to_string());
    }
}
