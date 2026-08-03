//! Presentation-to-PDF conversion for the inline preview.
//!
//! There is no presentation engine in the renderer, so slides are shown the
//! way most tools outside PowerPoint show them: converted to PDF and drawn by
//! the PDF viewer. Conversion runs a LibreOffice the user already has
//! installed — nothing is bundled or downloaded — and its absence is a
//! first-class state the renderer turns into an install hint, not an error.
//!
//! The converter processes untrusted bytes, so the invocation is contained
//! the way the rest of the app treats external processes: an empty
//! environment, a throwaway working directory that also holds the LibreOffice
//! profile, piped stdio, a hard timeout with `kill_on_drop`, and size caps on
//! both input and output. It is not yet wrapped in the exec sandbox profile —
//! that profile denies the application directories LibreOffice lives in and
//! widening it is real design work, recorded as a known gap.
//!
//! Converted PDFs are cached on disk under `derived/office-pdf/`, keyed by the
//! SHA-256 of the source bytes. The directory sits deliberately outside
//! `blobs/`: the blob orphan auditor deletes unreferenced blobs after a grace
//! period, and a derived artifact has no referencing row. Eviction here is a
//! simple size budget, oldest file first; evicting a live entry only costs a
//! reconversion.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use openwave_core::MAX_BINARY_DELIVERABLE_BYTES;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tokio::process::Command;

/// LibreOffice loads the whole document before exporting; 90 seconds is
/// generous for any deck the 16 MB input cap admits, and bounds a hang on a
/// crafted file.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(90);

/// A PDF export larger than this is not a preview any more. Four times the
/// binary-output ceiling covers decks whose images recompress badly.
const MAX_PDF_BYTES: u64 = 4 * MAX_BINARY_DELIVERABLE_BYTES as u64;

/// Disk budget for cached conversions. Oldest files are pruned past this;
/// a pruned entry just reconverts on next view.
const CACHE_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

const CACHE_DIRECTORY: &str = "derived/office-pdf";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PresentationPdfRequest {
    /// The presentation's bytes, base64 like every other bulk IPC payload.
    content_base64: String,
    /// Stored media type of the source; picks the input filename extension
    /// LibreOffice keys its import filter on.
    media_type: String,
}

/// Outcome of a conversion request. A missing converter is a state the
/// renderer designs for (install hint + download remains), not a failure.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub(crate) enum PresentationPdfResult {
    Converted { pdf_base64: String },
    ConverterMissing,
}

/// Convert one presentation's bytes to PDF with the user's LibreOffice.
///
/// Bytes travel in and out rather than a document identity so the one command
/// serves both transports the viewers already use — HTTP-fetched source
/// documents and IPC-read output revisions — without duplicating either
/// resolution path here.
#[tauri::command]
pub(crate) async fn convert_presentation_to_pdf(
    app: AppHandle,
    request: PresentationPdfRequest,
) -> Result<PresentationPdfResult, String> {
    let extension = input_extension(&request.media_type)
        .ok_or_else(|| "That file type has no presentation preview".to_owned())?;
    let bytes = BASE64
        .decode(request.content_base64.as_bytes())
        .map_err(|_| "Could not read that file".to_owned())?;
    if bytes.is_empty() {
        return Err("That file is empty".to_owned());
    }
    if bytes.len() > MAX_BINARY_DELIVERABLE_BYTES {
        return Err("That file is too large to preview".to_owned());
    }

    let cache_dir = crate::data_dir(&app)?.join(CACHE_DIRECTORY);
    let cache_path = cache_dir.join(format!("{}.pdf", content_key(&bytes)));
    if let Ok(cached) = tokio::fs::read(&cache_path).await {
        if !cached.is_empty() {
            return Ok(PresentationPdfResult::Converted {
                pdf_base64: BASE64.encode(cached),
            });
        }
    }

    let Some(soffice) = locate_soffice() else {
        return Ok(PresentationPdfResult::ConverterMissing);
    };

    let pdf = match run_conversion(&soffice, &bytes, extension).await {
        Ok(pdf) => pdf,
        // The resolved path failed to spawn — a stale launcher script or a
        // half-removed install. To the user that is the same state as no
        // LibreOffice at all: the install hint is the actionable message.
        Err(ConversionError::Spawn) => return Ok(PresentationPdfResult::ConverterMissing),
        Err(ConversionError::Failed(reason)) => return Err(reason),
    };

    // Cache best-effort: a preview that converted but failed to persist is
    // still a preview.
    let _ = store_cached_pdf(&cache_dir, &cache_path, &pdf).await;

    Ok(PresentationPdfResult::Converted {
        pdf_base64: BASE64.encode(pdf),
    })
}

/// The input extension LibreOffice selects its import filter by.
fn input_extension(media_type: &str) -> Option<&'static str> {
    let base = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        "application/vnd.ms-powerpoint" => Some("ppt"),
        "application/vnd.oasis.opendocument.presentation" => Some("odp"),
        _ => None,
    }
}

/// Cache key: the SHA-256 of the source bytes. Content-addressed like blob
/// ids, so the same deck imported twice converts once.
fn content_key(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut key = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// Find a LibreOffice the user installed, checking the platform's standard
/// application location before `PATH`.
///
/// A hit here is a candidate, not a guarantee — package managers leave
/// launcher scripts behind after the application is removed — so spawn
/// failure downstream is folded back into the missing-converter state.
fn locate_soffice() -> Option<PathBuf> {
    for candidate in standard_install_paths() {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in PATH_BINARY_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn standard_install_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(
        "/Applications/LibreOffice.app/Contents/MacOS/soffice",
    )];
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join("Applications/LibreOffice.app/Contents/MacOS/soffice"));
    }
    paths
}

#[cfg(target_os = "windows")]
fn standard_install_paths() -> Vec<PathBuf> {
    ["C:\\Program Files", "C:\\Program Files (x86)"]
        .iter()
        .map(|root| PathBuf::from(root).join("LibreOffice\\program\\soffice.exe"))
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn standard_install_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/bin/soffice"),
        PathBuf::from("/usr/bin/libreoffice"),
    ]
}

#[cfg(windows)]
const PATH_BINARY_NAMES: &[&str] = &["soffice.exe", "soffice.com"];
#[cfg(not(windows))]
const PATH_BINARY_NAMES: &[&str] = &["soffice", "libreoffice"];

enum ConversionError {
    /// The binary would not start; indistinguishable from not installed.
    Spawn,
    /// LibreOffice ran and did not produce a usable PDF.
    Failed(String),
}

/// Run one headless conversion in a throwaway directory.
///
/// The directory is working directory, `HOME`, temp dir, and LibreOffice
/// profile (`UserInstallation`) all at once, so nothing the conversion writes
/// lands outside it and no state is carried between conversions. The
/// environment is cleared to keep the user's session out of an untrusted
/// document's reach.
async fn run_conversion(
    soffice: &Path,
    bytes: &[u8],
    extension: &str,
) -> Result<Vec<u8>, ConversionError> {
    let workdir = tempfile::Builder::new()
        .prefix("openwave-office-pdf-")
        .tempdir()
        .map_err(|error| ConversionError::Failed(format!("workspace: {error}")))?;
    let input = workdir.path().join(format!("slides.{extension}"));
    let out_dir = workdir.path().join("out");
    let profile = workdir.path().join("profile");
    tokio::fs::write(&input, bytes)
        .await
        .map_err(|error| ConversionError::Failed(format!("workspace: {error}")))?;
    tokio::fs::create_dir(&out_dir)
        .await
        .map_err(|error| ConversionError::Failed(format!("workspace: {error}")))?;

    let mut command = Command::new(soffice);
    command
        .arg("--headless")
        .arg("--nologo")
        .arg("--norestore")
        .arg("--nolockcheck")
        .arg("--nofirststartwizard")
        .arg(format!("-env:UserInstallation={}", file_uri(&profile)))
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(&out_dir)
        .arg(&input)
        .current_dir(workdir.path())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.env("HOME", workdir.path());
        command.env("TMPDIR", workdir.path());
    }
    #[cfg(windows)]
    {
        // Windows processes fail in odd ways without SystemRoot; temp goes to
        // the throwaway directory like the Unix branch.
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command.env("TEMP", workdir.path());
        command.env("TMP", workdir.path());
    }

    let child = command.spawn().map_err(|_| ConversionError::Spawn)?;
    // On timeout the future is dropped and `kill_on_drop` terminates the
    // process before the workspace directory is removed.
    let output = tokio::time::timeout(CONVERT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| ConversionError::Failed("Converting the presentation timed out".to_owned()))?
        .map_err(|error| ConversionError::Failed(format!("conversion: {error}")))?;

    // LibreOffice can exit zero without writing anything, so the produced
    // file — present and non-empty — is the success signal, not the code.
    let produced = out_dir.join("slides.pdf");
    let metadata = tokio::fs::metadata(&produced).await;
    let produced_len = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
    if !output.status.success() || produced_len == 0 {
        // Package managers leave launcher scripts behind after the
        // application is removed; those spawn fine and then exit with the
        // shell's not-found / not-executable codes. Same state as no install.
        if matches!(output.status.code(), Some(126) | Some(127)) {
            return Err(ConversionError::Spawn);
        }
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        return Err(ConversionError::Failed(if detail.is_empty() {
            "The presentation could not be converted".to_owned()
        } else {
            format!("The presentation could not be converted: {detail}")
        }));
    }
    if produced_len > MAX_PDF_BYTES {
        return Err(ConversionError::Failed(
            "The converted preview is too large to show".to_owned(),
        ));
    }
    tokio::fs::read(&produced)
        .await
        .map_err(|error| ConversionError::Failed(format!("conversion: {error}")))
}

/// A `file:` URI for LibreOffice's `-env:UserInstallation` argument.
fn file_uri(path: &Path) -> String {
    #[cfg(windows)]
    {
        format!("file:///{}", path.display().to_string().replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        format!("file://{}", path.display())
    }
}

/// Persist one converted PDF and prune the cache to its budget.
async fn store_cached_pdf(cache_dir: &Path, cache_path: &Path, pdf: &[u8]) -> std::io::Result<()> {
    tokio::fs::create_dir_all(cache_dir).await?;
    // Write-then-rename so a crash mid-write never leaves a truncated PDF
    // answering for a content hash.
    let staging = cache_path.with_extension("pdf.partial");
    tokio::fs::write(&staging, pdf).await?;
    tokio::fs::rename(&staging, cache_path).await?;
    prune_cache(cache_dir).await
}

/// Drop oldest-modified cache entries until the directory fits the budget.
async fn prune_cache(cache_dir: &Path) -> std::io::Result<()> {
    let mut entries = Vec::new();
    let mut total: u64 = 0;
    let mut dir = tokio::fs::read_dir(cache_dir).await?;
    while let Some(entry) = dir.next_entry().await? {
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        total += metadata.len();
        entries.push((modified, metadata.len(), entry.path()));
    }
    if total <= CACHE_BUDGET_BYTES {
        return Ok(());
    }
    entries.sort_by_key(|(modified, ..)| *modified);
    for (_, len, path) in entries {
        if total <= CACHE_BUDGET_BYTES {
            break;
        }
        if tokio::fs::remove_file(&path).await.is_ok() {
            total = total.saturating_sub(len);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_media_types_map_to_import_extensions() {
        assert_eq!(
            input_extension(
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            ),
            Some("pptx")
        );
        assert_eq!(
            input_extension("application/vnd.ms-powerpoint; charset=binary"),
            Some("ppt")
        );
        assert_eq!(
            input_extension("application/vnd.oasis.opendocument.presentation"),
            Some("odp")
        );
        assert_eq!(input_extension("application/pdf"), None);
    }

    /// End-to-end proof against a real LibreOffice, when one is installed.
    /// Skips silently otherwise — CI runners and most dev machines carry no
    /// LibreOffice, and its absence is exactly the state the feature designs
    /// for, not a test failure.
    #[tokio::test]
    async fn converts_a_real_deck_when_libreoffice_is_installed() {
        let Some(soffice) = locate_soffice() else {
            eprintln!("skipping: no LibreOffice installed");
            return;
        };
        let deck = include_bytes!("../tests/fixtures/deck.pptx");
        match run_conversion(&soffice, deck, "pptx").await {
            Ok(pdf) => assert!(
                pdf.starts_with(b"%PDF-"),
                "conversion produced something that is not a PDF"
            ),
            // A leftover launcher for a removed install resolves but cannot
            // spawn; that is the missing-converter state, not a defect.
            Err(ConversionError::Spawn) => {
                eprintln!("skipping: resolved LibreOffice cannot spawn");
            }
            Err(ConversionError::Failed(reason)) => {
                panic!("conversion failed with LibreOffice present: {reason}")
            }
        }
    }
}
