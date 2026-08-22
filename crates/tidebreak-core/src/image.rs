//! Provider-neutral image attachment identity and the ephemeral bytes that
//! back it for exactly one request.
//!
//! The split here is deliberate and load-bearing. [`ImageRef`] is *durable
//! identity*: a blob id, a bounded media type, and bounded dimensions. It is
//! cheap to clone, safe to log, safe to persist, and safe to hand to a
//! renderer. [`ImageAttachments`] is *ephemeral bytes*: the pixels for one
//! outbound request, hydrated from the blob store just before the adapter
//! serializes and dropped immediately after.
//!
//! Keeping the two apart is what makes "image bytes never enter transcript
//! text, logs, tool arguments, or renderer-safe event payloads" a structural
//! property rather than a rule contributors have to remember. A
//! [`ContentBlock::Image`](crate::ContentBlock::Image) carries only an
//! `ImageRef`, so every existing path that serializes, stores, or debug-prints
//! a content block stays byte-free by construction. The bytes ride beside the
//! request in a field that is `#[serde(skip)]` and whose [`Debug`] is written
//! by hand to print sizes instead of contents.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Largest attachment accepted in either dimension.
///
/// Anthropic documents 8000×8000 as the hard ceiling for a single image, and
/// OpenAI's limits sit below that, so this is the tightest bound that both
/// providers will always accept. Refusing here means an oversized image fails
/// at ingest, where the user can still do something about it, rather than as a
/// provider 400 in the middle of a turn. It also bounds the tile arithmetic in
/// [`ImageRef::estimated_tokens`].
///
/// Note that Anthropic tightens this to roughly 2000px once a request carries
/// many images; downscaling to fit that case belongs to the ingest path, not
/// to this bound.
pub const MAX_IMAGE_DIMENSION: u32 = 8_000;

/// Largest attachment accepted, in bytes.
///
/// Matches the existing raw-document import ceiling so one attachment can
/// never be accepted by one path and refused by the other.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// Image formats Tidebreak will send to a provider.
///
/// Deliberately closed. Every variant here is accepted by the baseline
/// Anthropic and OpenAI image APIs. A provider with a narrower documented set
/// must refuse its unsupported variants before egress (xAI, for example,
/// accepts only PNG and JPEG) rather than passing them through to a provider
/// 400. Vector and exotic raster formats are excluded at the trusted ingest
/// boundary instead of being passed through at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ImageMediaType {
    /// `image/png`
    Png,
    /// `image/jpeg`
    Jpeg,
    /// `image/webp`
    Webp,
    /// `image/gif`
    Gif,
}

impl ImageMediaType {
    /// The IANA media type, as both providers expect to receive it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    /// Parse an IANA media type, ignoring case and any parameters.
    ///
    /// Returns `None` for anything outside the closed set above, including
    /// types a provider might tolerate but Tidebreak does not vouch for.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let base = value.split(';').next()?.trim();
        // Media types are case-insensitive; compare without allocating for the
        // already-lowercase common case.
        let matches = |candidate: &str| base.eq_ignore_ascii_case(candidate);
        if matches("image/png") {
            Some(Self::Png)
        } else if matches("image/jpeg") {
            Some(Self::Jpeg)
        } else if matches("image/webp") {
            Some(Self::Webp)
        } else if matches("image/gif") {
            Some(Self::Gif)
        } else {
            None
        }
    }

    /// The file extension to write these bytes under, without the dot.
    ///
    /// Used where an attachment is materialized as a file rather than sent
    /// over a protocol: an engine that reads the image off disk needs the
    /// name to say what it is, both for its own sniffing and for a person
    /// reading the path in the prompt.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Gif => "gif",
        }
    }

    /// Identify a supported raster format from its file signature.
    ///
    /// This deliberately recognizes only the provider-safe formats represented
    /// by this enum. Callers that accept a filename fallback can apply it after
    /// this byte authority returns `None`.
    #[must_use]
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else {
            None
        }
    }
}

impl fmt::Display for ImageMediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Durable identity of one image attachment.
///
/// Everything here is safe to persist, log, and expose to a renderer. The blob
/// id is an opaque content-derived UUID, never a filesystem path, so it reveals
/// nothing about where the bytes live on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ImageRef {
    /// Content-addressed blob holding the pixels.
    pub blob_id: Uuid,
    /// Format the bytes were sniffed as at ingest.
    pub media_type: ImageMediaType,
    /// Pixel width, read from the image header.
    pub width: u32,
    /// Pixel height, read from the image header.
    pub height: u32,
    /// Size of the stored bytes.
    pub byte_len: u64,
}

impl ImageRef {
    /// Validate the bounds every trusted ingest path must enforce.
    ///
    /// # Errors
    ///
    /// Returns a static reason when a dimension is zero or past
    /// [`MAX_IMAGE_DIMENSION`], or the byte length is zero or past
    /// [`MAX_IMAGE_BYTES`].
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.width == 0 || self.height == 0 {
            return Err("image attachment has a zero dimension");
        }
        if self.width > MAX_IMAGE_DIMENSION || self.height > MAX_IMAGE_DIMENSION {
            return Err("image attachment exceeds the maximum dimension");
        }
        if self.byte_len == 0 {
            return Err("image attachment is empty");
        }
        if self.byte_len > MAX_IMAGE_BYTES {
            return Err("image attachment exceeds the maximum size");
        }
        Ok(())
    }

    /// Estimated prompt tokens this image costs.
    ///
    /// Tile-based: a `512×512` tile is charged a flat rate and partial tiles
    /// round up. This is an approximation — Anthropic documents roughly
    /// `width × height / 750` and OpenAI charges a base plus per-tile cost —
    /// and it is chosen to sit at or above both so context budgeting errs
    /// toward sending less. Budgeting only needs a bound it will not undershoot;
    /// an exact per-provider count is not worth diverging the estimate for.
    ///
    /// Dimensions are validated at ingest, but a zero here still falls back to
    /// a single tile rather than reporting a free image.
    #[must_use]
    pub const fn estimated_tokens(&self) -> usize {
        if self.width == 0 || self.height == 0 {
            return TILE_TOKENS;
        }
        let tiles_x = (self.width as usize).div_ceil(TILE_EDGE);
        let tiles_y = (self.height as usize).div_ceil(TILE_EDGE);
        tiles_x * tiles_y * TILE_TOKENS
    }
}

/// Edge length in pixels of one billing tile.
const TILE_EDGE: usize = 512;

/// Estimated prompt tokens charged for one `512×512` tile.
const TILE_TOKENS: usize = 1_600;

/// Raw bytes for one attachment, held only for the duration of a request.
///
/// The hand-written [`Debug`] is the point: a derived one would dump megabytes
/// of pixels into any log line that formats a [`ChatRequest`](crate::ChatRequest).
#[derive(Clone, PartialEq, Eq)]
pub struct ImageData {
    media_type: ImageMediaType,
    bytes: Vec<u8>,
}

impl ImageData {
    /// Wrap hydrated bytes with the media type they were sniffed as.
    #[must_use]
    pub fn new(media_type: ImageMediaType, bytes: Vec<u8>) -> Self {
        Self { media_type, bytes }
    }

    /// Format of these bytes.
    #[must_use]
    pub const fn media_type(&self) -> ImageMediaType {
        self.media_type
    }

    /// The raw pixels.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Number of bytes held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether no bytes are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for ImageData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageData")
            .field("media_type", &self.media_type)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

/// Bytes for the [`ImageRef`]s appearing in one request, keyed by blob id.
///
/// Attached to [`ChatRequest`](crate::ChatRequest) out of band and skipped by
/// serde, so no serialization of a request — a debug log, an error payload, a
/// journal entry — can carry pixels. Adapters resolve each block's blob id
/// here and fail closed when it is absent; silently dropping the block would
/// send the model a question about an image it was never given.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ImageAttachments {
    by_blob: BTreeMap<Uuid, ImageData>,
}

impl ImageAttachments {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace the bytes for `blob_id`.
    pub fn insert(&mut self, blob_id: Uuid, data: ImageData) {
        self.by_blob.insert(blob_id, data);
    }

    /// Bytes for `blob_id`, if hydrated.
    #[must_use]
    pub fn get(&self, blob_id: Uuid) -> Option<&ImageData> {
        self.by_blob.get(&blob_id)
    }

    /// Whether `blob_id` has hydrated bytes.
    #[must_use]
    pub fn contains(&self, blob_id: Uuid) -> bool {
        self.by_blob.contains_key(&blob_id)
    }

    /// How many attachments are hydrated.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_blob.len()
    }

    /// Whether nothing is hydrated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_blob.is_empty()
    }

    /// Total bytes held across every attachment.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.by_blob.values().map(ImageData::len).sum()
    }

    /// Drop every hydrated attachment, keeping the referring blocks intact.
    ///
    /// Used as a degradation step when a provider refuses a request in a way
    /// that dropping pixels can fix.
    pub fn clear(&mut self) {
        self.by_blob.clear();
    }

    /// Keep only the attachments named by `keep`, dropping the rest.
    ///
    /// The caller decides which references still deserve pixels — typically
    /// the most recent few, to bound outbound body growth over a long chat.
    pub fn retain_only(&mut self, keep: &std::collections::HashSet<Uuid>) {
        self.by_blob.retain(|blob_id, _| keep.contains(blob_id));
    }
}

impl fmt::Debug for ImageAttachments {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageAttachments")
            .field("count", &self.by_blob.len())
            .field("total_bytes", &self.total_bytes())
            .finish()
    }
}

impl FromIterator<(Uuid, ImageData)> for ImageAttachments {
    fn from_iter<T: IntoIterator<Item = (Uuid, ImageData)>>(iter: T) -> Self {
        Self {
            by_blob: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_ref(width: u32, height: u32) -> ImageRef {
        ImageRef {
            blob_id: Uuid::from_u128(1),
            media_type: ImageMediaType::Png,
            width,
            height,
            byte_len: 1_024,
        }
    }

    #[test]
    fn media_types_round_trip_and_reject_unsupported_formats() {
        for media_type in [
            ImageMediaType::Png,
            ImageMediaType::Jpeg,
            ImageMediaType::Webp,
            ImageMediaType::Gif,
        ] {
            assert_eq!(ImageMediaType::parse(media_type.as_str()), Some(media_type));
        }
        assert_eq!(
            ImageMediaType::parse("IMAGE/PNG"),
            Some(ImageMediaType::Png)
        );
        assert_eq!(
            ImageMediaType::parse("image/jpeg; charset=binary"),
            Some(ImageMediaType::Jpeg)
        );
        // Vector and unknown raster formats stay out of the closed set so they
        // fail at ingest rather than at send time.
        assert_eq!(ImageMediaType::parse("image/svg+xml"), None);
        assert_eq!(ImageMediaType::parse("image/tiff"), None);
        assert_eq!(ImageMediaType::parse("application/pdf"), None);
        assert_eq!(ImageMediaType::parse(""), None);
    }

    #[test]
    fn token_estimate_rounds_partial_tiles_up_and_never_reports_a_free_image() {
        // Exactly one tile.
        assert_eq!(image_ref(512, 512).estimated_tokens(), TILE_TOKENS);
        // A single pixel still occupies a whole tile.
        assert_eq!(image_ref(1, 1).estimated_tokens(), TILE_TOKENS);
        // Partial tiles round up in both axes: 3 × 2 tiles.
        assert_eq!(image_ref(1_025, 600).estimated_tokens(), 6 * TILE_TOKENS);
        // A degenerate dimension must not price the image at zero.
        assert_eq!(image_ref(0, 0).estimated_tokens(), TILE_TOKENS);
    }

    #[test]
    fn validation_rejects_degenerate_and_oversized_attachments() {
        assert!(image_ref(800, 600).validate().is_ok());
        assert!(image_ref(0, 600).validate().is_err());
        assert!(image_ref(800, 0).validate().is_err());
        assert!(image_ref(MAX_IMAGE_DIMENSION + 1, 600).validate().is_err());
        assert!(image_ref(800, MAX_IMAGE_DIMENSION + 1).validate().is_err());

        let mut empty = image_ref(800, 600);
        empty.byte_len = 0;
        assert!(empty.validate().is_err());

        let mut oversized = image_ref(800, 600);
        oversized.byte_len = MAX_IMAGE_BYTES + 1;
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn debug_output_reports_sizes_and_never_leaks_bytes() {
        let secret = vec![0xAB; 32];
        let data = ImageData::new(ImageMediaType::Png, secret);
        let rendered = format!("{data:?}");
        assert!(rendered.contains("32 bytes"), "{rendered}");
        assert!(!rendered.contains("171"), "{rendered}");
        assert!(!rendered.contains("ab"), "{rendered}");

        let attachments: ImageAttachments = [(
            Uuid::from_u128(1),
            ImageData::new(ImageMediaType::Png, vec![0xCD; 16]),
        )]
        .into_iter()
        .collect();
        let rendered = format!("{attachments:?}");
        assert!(rendered.contains("total_bytes"), "{rendered}");
        assert!(rendered.contains("16"), "{rendered}");
        assert!(!rendered.contains("205"), "{rendered}");
    }

    #[test]
    fn image_ref_serialization_carries_identity_and_no_pixels() {
        let value = serde_json::to_value(image_ref(800, 600)).unwrap();
        assert_eq!(value["media_type"], "png");
        assert_eq!(value["width"], 800);
        assert_eq!(value["byte_len"], 1_024);
        assert!(value.get("bytes").is_none());
        let restored: ImageRef = serde_json::from_value(value).unwrap();
        assert_eq!(restored, image_ref(800, 600));
    }

    #[test]
    fn retain_only_and_clear_drop_pixels_without_touching_identity() {
        let kept = Uuid::from_u128(1);
        let dropped = Uuid::from_u128(2);
        let mut attachments: ImageAttachments = [
            (kept, ImageData::new(ImageMediaType::Png, vec![1; 8])),
            (dropped, ImageData::new(ImageMediaType::Jpeg, vec![2; 8])),
        ]
        .into_iter()
        .collect();
        assert_eq!(attachments.total_bytes(), 16);

        attachments.retain_only(&std::iter::once(kept).collect());
        assert!(attachments.contains(kept));
        assert!(!attachments.contains(dropped));
        assert_eq!(attachments.len(), 1);

        attachments.clear();
        assert!(attachments.is_empty());
    }
}
