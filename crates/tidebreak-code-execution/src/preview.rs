use std::cmp::Ordering;
use std::io::{Cursor, Read as _};
use std::path::Path;

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use tidebreak_core::{DocumentBlob, ImageData, ImageMediaType, ImageRef, MAX_IMAGE_BYTES};

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
}

#[derive(Debug)]
enum PreviewDirectoryError {
    Missing,
    NotPrivate,
    Unavailable,
}

/// Read, prioritize, resize, and bound images directly under `preview/`.
///
/// Invalid candidates do not turn a successful command into a failed one. They
/// are named in a compact note so the model can repair the preview and rerun.
pub fn scan_preview_directory(preview_dir: &Path) -> PreviewScan {
    let mut scan = PreviewScan::default();
    // Pin the containing scratch directory, then open preview/ relative to it
    // without following the final component. Every candidate is subsequently
    // opened relative to this descriptor with the same no-follow rule. A
    // sandbox process can rename or replace either pathname after this point,
    // but it cannot redirect the descriptor or the file handle we actually
    // inspect and decode.
    let directory = match open_preview_directory(preview_dir) {
        Ok(directory) => directory,
        Err(PreviewDirectoryError::Missing) => return scan,
        Err(PreviewDirectoryError::NotPrivate) => {
            scan.notes
                .push("preview images unavailable: preview/ is not a private workspace directory. Remove it and rerun.".into());
            return scan;
        }
        Err(PreviewDirectoryError::Unavailable) => {
            scan.notes
                .push("preview images unavailable: preview/ could not be read".into());
            return scan;
        }
    };
    let entries = match directory.entries() {
        Ok(entries) => entries,
        Err(_) => {
            scan.notes
                .push("preview images unavailable: preview/ could not be read".into());
            return scan;
        }
    };
    let mut candidates = Vec::new();
    let mut unsupported = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // Classify relative to the pinned directory and without following a
        // symlink. The later no-follow open remains the enforcing operation;
        // this check only avoids treating known non-files as candidates.
        let Ok(metadata) = directory.symlink_metadata(&name) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() == 0 {
            continue;
        }
        if preview_media_type_from_extension(&name).is_none() {
            unsupported.push(name);
            continue;
        }
        candidates.push(Candidate { name });
    }
    candidates.sort_by(|left, right| {
        preview_priority(&left.name)
            .cmp(&preview_priority(&right.name))
            .then_with(|| alphabetical(&left.name, &right.name))
    });
    unsupported.sort_by(|left, right| alphabetical(left, right));

    if !unsupported.is_empty() {
        scan.notes.push(format!(
            "unsupported preview file(s): {}. preview/ accepts PNG, JPEG, or WebP images. Keep SVG and other source files in output/, and write a raster copy to preview/ for visual review.",
            unsupported.join(", ")
        ));
    }

    let mut rejected = Vec::new();
    let mut omitted = Vec::new();
    for candidate in candidates {
        if scan.images.len() == MAX_EXEC_PREVIEW_IMAGES {
            omitted.push(candidate.name);
            continue;
        }
        match prepare_preview(&directory, &candidate) {
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

fn open_preview_directory(preview_dir: &Path) -> Result<Dir, PreviewDirectoryError> {
    let parent_path = preview_dir
        .parent()
        .ok_or(PreviewDirectoryError::Unavailable)?;
    let name = preview_dir
        .file_name()
        .ok_or(PreviewDirectoryError::Unavailable)?;
    let parent = Dir::open_ambient_dir(parent_path, ambient_authority())
        .map_err(|_| PreviewDirectoryError::Unavailable)?;
    match parent.open_dir_nofollow(name) {
        Ok(directory) => Ok(directory),
        Err(_) => match parent.symlink_metadata(name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(PreviewDirectoryError::Missing)
            }
            Ok(metadata) if metadata.is_symlink() || !metadata.is_dir() => {
                Err(PreviewDirectoryError::NotPrivate)
            }
            Ok(_) | Err(_) => Err(PreviewDirectoryError::Unavailable),
        },
    }
}

fn prepare_preview(directory: &Dir, candidate: &Candidate) -> Result<(ImageRef, ImageData), ()> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory
        .open_with(&candidate.name, &options)
        .map_err(|_| ())?;
    let metadata = file.metadata().map_err(|_| ())?;
    let max_bytes = (MAX_WORKSPACE_FILE_BYTES as u64).min(MAX_IMAGE_BYTES);
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes {
        return Err(());
    }
    // Bound the read itself rather than trusting the earlier length. A writer
    // may extend the already-open file after metadata() returns; reading one
    // byte past the ceiling detects that race without allocating unboundedly.
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| ())?);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.is_empty() || bytes.len() as u64 > max_bytes {
        return Err(());
    }
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

    /// A model-controlled symlink inside a legitimate preview directory must
    /// not make the unsandboxed host read an otherwise inaccessible image.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_preview_file_cannot_disclose_an_outside_image() {
        let elsewhere = tempfile::tempdir().unwrap();
        write_png(elsewhere.path(), "private.png", 19, 17);
        let private_bytes = std::fs::read(elsewhere.path().join("private.png")).unwrap();

        let preview = tempfile::tempdir().unwrap();
        write_png(preview.path(), "safe.png", 8, 6);
        std::os::unix::fs::symlink(
            elsewhere.path().join("private.png"),
            preview.path().join("leak.png"),
        )
        .unwrap();

        let scan = scan_preview_directory(preview.path());

        assert_eq!(scan.images.len(), 1);
        assert_eq!(scan.images[0].0.width, 8);
        assert_eq!(scan.images[0].0.height, 6);
        assert_ne!(scan.images[0].1.bytes(), private_bytes);
    }

    /// The no-follow open, rather than the earlier directory listing, is the
    /// enforcement point for a file replaced after it was selected.
    #[cfg(unix)]
    #[test]
    fn a_preview_replaced_by_a_symlink_before_open_is_refused() {
        let elsewhere = tempfile::tempdir().unwrap();
        write_png(elsewhere.path(), "private.png", 19, 17);

        let preview = tempfile::tempdir().unwrap();
        write_png(preview.path(), "candidate.png", 8, 6);
        let directory = open_preview_directory(preview.path()).unwrap();
        let candidate = Candidate {
            name: "candidate.png".into(),
        };

        std::fs::remove_file(preview.path().join("candidate.png")).unwrap();
        std::os::unix::fs::symlink(
            elsewhere.path().join("private.png"),
            preview.path().join("candidate.png"),
        )
        .unwrap();

        assert!(prepare_preview(&directory, &candidate).is_err());
    }

    #[test]
    fn byte_signature_wins_and_extension_is_only_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        write_png(dir.path(), "actually-png.jpg", 24, 12);
        let scan = scan_preview_directory(dir.path());
        assert_eq!(scan.images[0].0.media_type, ImageMediaType::Png);
    }

    #[test]
    fn unsupported_preview_files_produce_an_actionable_note() {
        let preview = tempfile::tempdir().unwrap();
        std::fs::write(
            preview.path().join("palette.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
        )
        .unwrap();

        let scan = scan_preview_directory(preview.path());

        assert!(scan.images.is_empty());
        assert_eq!(scan.notes.len(), 1);
        assert!(scan.notes[0].contains("palette.svg"));
        assert!(scan.notes[0].contains("PNG, JPEG, or WebP"));
        assert!(scan.notes[0].contains("output/"));
    }
}
