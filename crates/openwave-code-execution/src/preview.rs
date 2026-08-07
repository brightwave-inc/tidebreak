use std::cmp::Ordering;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use openwave_core::{DocumentBlob, ImageData, ImageMediaType, ImageRef, MAX_IMAGE_BYTES};

use crate::MAX_WORKSPACE_FILE_BYTES;

/// Most preview images attached to one successful command result.
pub const MAX_EXEC_PREVIEW_IMAGES: usize = 3;
/// Largest preview edge sent to a model or renderer.
pub const MAX_EXEC_PREVIEW_DIMENSION: u32 = 2_000;

/// A successful preview-directory scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewScan {
    pub images: Vec<(ImageRef, ImageData)>,
    pub notes: Vec<String>,
}

#[derive(Debug)]
struct Candidate {
    name: String,
    path: PathBuf,
}

/// Read, prioritize, resize, and bound images directly under `preview/`.
///
/// Invalid candidates do not turn a successful command into a failed one. They
/// are named in a compact note so the model can repair the preview and rerun.
pub fn scan_preview_directory(preview_dir: &Path) -> PreviewScan {
    let mut scan = PreviewScan::default();
    // Skipping symlinked *entries* below is not enough on its own: the
    // directory itself is opened by path, and local exec is confined to the
    // scratch tree but can plant `<scratch>/preview -> ~/Pictures` there. The
    // traversal has already happened by the time entries are filtered, so
    // images from an arbitrary host directory would be attached to the chat.
    match fs::symlink_metadata(preview_dir) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            scan.notes
                .push("preview images unavailable: preview/ is not a private workspace directory. Remove it and rerun.".into());
            return scan;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return scan,
        Err(_) => {
            scan.notes
                .push("preview images unavailable: preview/ could not be read".into());
            return scan;
        }
    }
    let entries = match fs::read_dir(preview_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return scan,
        Err(_) => {
            scan.notes
                .push("preview images unavailable: preview/ could not be read".into());
            return scan;
        }
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || metadata.len() == 0 {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if preview_media_type_from_extension(&name).is_none() {
            continue;
        }
        candidates.push(Candidate {
            name,
            path: entry.path(),
        });
    }
    candidates.sort_by(|left, right| {
        preview_priority(&left.name)
            .cmp(&preview_priority(&right.name))
            .then_with(|| alphabetical(&left.name, &right.name))
    });

    let mut rejected = Vec::new();
    let mut omitted = Vec::new();
    for candidate in candidates {
        if scan.images.len() == MAX_EXEC_PREVIEW_IMAGES {
            omitted.push(candidate.name);
            continue;
        }
        match prepare_preview(&candidate) {
            Ok(image) => scan.images.push(image),
            Err(()) => rejected.push(candidate.name),
        }
    }
    if !omitted.is_empty() {
        scan.notes.push(format!(
            "preview image cap is {MAX_EXEC_PREVIEW_IMAGES}; omitted {}. Delete or rename files in preview/ and rerun to review them.",
            omitted.join(", ")
        ));
    }
    if !rejected.is_empty() {
        scan.notes.push(format!(
            "unreadable preview image(s) ignored: {}",
            rejected.join(", ")
        ));
    }
    scan
}

fn prepare_preview(candidate: &Candidate) -> Result<(ImageRef, ImageData), ()> {
    let metadata = fs::metadata(&candidate.path).map_err(|_| ())?;
    if metadata.len() > MAX_WORKSPACE_FILE_BYTES as u64 || metadata.len() > MAX_IMAGE_BYTES {
        return Err(());
    }
    let bytes = fs::read(&candidate.path).map_err(|_| ())?;
    let fallback = preview_media_type_from_extension(&candidate.name).ok_or(())?;
    let media_type = ImageMediaType::sniff(&bytes).unwrap_or(fallback);
    let format = match media_type {
        ImageMediaType::Png => ImageFormat::Png,
        ImageMediaType::Jpeg => ImageFormat::Jpeg,
        ImageMediaType::Webp => ImageFormat::WebP,
        ImageMediaType::Gif => return Err(()),
    };
    let decoded = image::load_from_memory_with_format(&bytes, format).map_err(|_| ())?;
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return Err(());
    }
    let prepared = if width > MAX_EXEC_PREVIEW_DIMENSION || height > MAX_EXEC_PREVIEW_DIMENSION {
        let resized = decoded.resize(
            MAX_EXEC_PREVIEW_DIMENSION,
            MAX_EXEC_PREVIEW_DIMENSION,
            FilterType::Lanczos3,
        );
        let mut encoded = Cursor::new(Vec::new());
        resized.write_to(&mut encoded, format).map_err(|_| ())?;
        encoded.into_inner()
    } else {
        bytes
    };
    let (width, height) = image::load_from_memory_with_format(&prepared, format)
        .map_err(|_| ())?
        .dimensions();
    let byte_len = u64::try_from(prepared.len()).map_err(|_| ())?;
    let image = ImageRef {
        blob_id: DocumentBlob::from_bytes(&prepared).id,
        media_type,
        width,
        height,
        byte_len,
    };
    image.validate().map_err(|_| ())?;
    Ok((image, ImageData::new(media_type, prepared)))
}

fn preview_media_type_from_extension(name: &str) -> Option<ImageMediaType> {
    let extension = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(ImageMediaType::Png),
        "jpg" | "jpeg" => Some(ImageMediaType::Jpeg),
        "webp" => Some(ImageMediaType::Webp),
        _ => None,
    }
}

fn preview_priority(name: &str) -> u8 {
    let name = name.to_ascii_lowercase();
    if ["grid", "thumb", "thumbnail", "overview"]
        .iter()
        .any(|needle| name.contains(needle))
    {
        0
    } else if ["page", "slide"].iter().any(|needle| name.contains(needle)) {
        1
    } else {
        2
    }
}

fn alphabetical(left: &str, right: &str) -> Ordering {
    left.to_ascii_lowercase()
        .cmp(&right.to_ascii_lowercase())
        .then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgb, RgbImage};

    fn write_png(dir: &Path, name: &str, width: u32, height: u32) {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([4, 5, 6])))
            .save_with_format(dir.join(name), ImageFormat::Png)
            .unwrap();
    }

    #[test]
    fn scan_prioritizes_caps_and_resizes_previews() {
        let dir = tempfile::tempdir().unwrap();
        write_png(dir.path(), "z.png", 32, 32);
        write_png(dir.path(), "page-2.png", 32, 32);
        write_png(dir.path(), "overview.png", 2_400, 1_200);
        write_png(dir.path(), "thumb-b.png", 32, 32);
        write_png(dir.path(), "thumb-a.png", 32, 32);

        let scan = scan_preview_directory(dir.path());

        assert_eq!(scan.images.len(), 3);
        assert_eq!(scan.images[0].0.width, 2_000);
        assert_eq!(scan.images[0].0.height, 1_000);
        assert!(scan.notes[0].contains("page-2.png"));
        assert!(scan.notes[0].contains("z.png"));
    }

    /// A preview directory replaced by a symlink would otherwise hand host
    /// images from an arbitrary directory to the model: skipping symlinked
    /// entries does not help once the traversal itself has happened.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_preview_directory_is_refused_rather_than_followed() {
        let elsewhere = tempfile::tempdir().unwrap();
        write_png(elsewhere.path(), "private.png", 16, 16);
        let scratch = tempfile::tempdir().unwrap();
        let preview = scratch.path().join("preview");
        std::os::unix::fs::symlink(elsewhere.path(), &preview).unwrap();

        let scan = scan_preview_directory(&preview);

        assert!(scan.images.is_empty());
        assert_eq!(scan.notes.len(), 1);
        assert!(scan.notes[0].contains("not a private workspace directory"));
    }

    #[test]
    fn byte_signature_wins_and_extension_is_only_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        write_png(dir.path(), "actually-png.jpg", 24, 12);
        let scan = scan_preview_directory(dir.path());
        assert_eq!(scan.images[0].0.media_type, ImageMediaType::Png);
    }
}
