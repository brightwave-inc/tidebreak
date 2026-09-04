//! Image-byte validation shared by chat and code-session boundaries.

use std::io::Cursor;

use axum::http::{header, HeaderMap};
use image::{ImageFormat, ImageReader};
use serde::Serialize;
use uuid::Uuid;

use tidebreak_core::{
    DocumentBlob, ImageMediaType, ImageRef, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION,
};

use crate::error::ServerError;

pub const MAX_IMAGE_ATTACHMENT_BYTES: usize = MAX_IMAGE_BYTES as usize;

#[derive(Debug, Serialize)]
pub struct PublishedImageAttachment {
    pub attachment_id: Uuid,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
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

/// Derive durable identity from candidate image bytes.
pub fn inspect_image_bytes(bytes: &[u8]) -> Result<ImageRef, ServerError> {
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
        blob_id: DocumentBlob::from_bytes(bytes).id,
        media_type,
        width,
        height,
        byte_len,
    };
    image.validate().map_err(|reason| {
        ServerError::bad_request_kind("image_attachment_invalid", reason.to_owned())
    })?;
    Ok(image)
}

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

pub fn require_declared_type_matches(
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
