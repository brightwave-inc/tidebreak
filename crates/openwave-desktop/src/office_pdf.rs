//! Presentation-to-PDF conversion for the inline preview.
//!
//! There is no presentation engine in the renderer, so slides are shown the
//! way most tools outside PowerPoint show them: converted to PDF and drawn by
//! the PDF viewer. Conversion prefers the app's own managed LibreOffice
//! (downloaded and digest-verified by [`crate::office_install`] the first
//! time a preview needs it, on macOS), then falls back to one the user
//! installed. Its absence is a first-class state the renderer turns into a
//! download or an install hint, not an error.
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
/// renderer designs for, not a failure: on macOS it triggers the managed
/// download (unless one already failed this run), elsewhere the install hint.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub(crate) enum PresentationPdfResult {
    Converted {
        pdf_base64: String,
    },
    ConverterMissing {
        /// Whether this platform can install its own LibreOffice.
        installable: bool,
        /// Why the last managed install this app run failed (or was
        /// cancelled), if it did. While present the renderer shows the hint
        /// and waits for an explicit retry instead of re-downloading.
        install_failure: Option<String>,
    },
}

fn converter_missing() -> PresentationPdfResult {
    PresentationPdfResult::ConverterMissing {
        installable: crate::office_install::supported(),
        install_failure: crate::office_install::last_failure(),
    }
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

    let data_dir = crate::data_dir(&app)?;
    let cache_dir = data_dir.join(CACHE_DIRECTORY);
    let cache_path = cache_dir.join(format!("{}.pdf", content_key(&bytes)));
    if let Ok(cached) = tokio::fs::read(&cache_path).await {
        if !cached.is_empty() {
            return Ok(PresentationPdfResult::Converted {
                pdf_base64: BASE64.encode(cached),
            });
        }
    }

    // One conversion at a time. Duplicate requests for the same deck arrive
    // routinely (two panels, or a fetch retried around an aborted render) and
    // used to race two cold LibreOffice launches — flaky in exactly the
    // hard-to-reproduce way, and their `.partial` cache staging collided.
    // Serialized, the second request waits and is answered from the cache the
    // first one just wrote.
    let _serialized = conversion_lock().lock().await;
    if let Ok(cached) = tokio::fs::read(&cache_path).await {
        if !cached.is_empty() {
            return Ok(PresentationPdfResult::Converted {
                pdf_base64: BASE64.encode(cached),
            });
        }
    }

    let Some(soffice) = locate_soffice(&data_dir) else {
        return Ok(converter_missing());
    };

    let pdf = match run_conversion(&soffice, &bytes, extension).await {
        Ok(pdf) => pdf,
        // The resolved path failed to spawn — a stale launcher script or a
        // half-removed install. To the user that is the same state as no
        // LibreOffice at all: the install hint is the actionable message.
        Err(ConversionError::Spawn) => return Ok(converter_missing()),
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

/// Find a LibreOffice to convert with: the app's own verified managed
/// install first, then one the user installed system-wide, then `PATH`.
///
/// The managed install leads because it is the copy whose provenance this
/// app verified; a system hit is a candidate, not a guarantee — package
/// managers leave launcher scripts behind after the application is removed —
/// so spawn failure downstream is folded back into the missing-converter
/// state.
fn locate_soffice(data_dir: &Path) -> Option<PathBuf> {
    resolve_soffice(
        crate::office_install::managed_soffice(data_dir),
        system_soffice,
    )
}

/// The resolution order, as a seam: a managed install wins outright and the
/// system is not probed at all behind one.
fn resolve_soffice(
    managed: Option<PathBuf>,
    system: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    managed.or_else(system)
}

fn system_soffice() -> Option<PathBuf> {
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

/// Whether a system LibreOffice not only resolves but actually runs.
///
/// The install warm-up must not be talked out of downloading by a leftover
/// launcher script on `PATH` — a Homebrew shim pointing at a removed
/// `/Applications` bundle resolves as a file, spawns, and exits 126/127. One
/// cheap `--version` probe (called at most once per app run) separates a
/// converter that will work from remains that will not.
pub(crate) async fn workable_system_soffice() -> bool {
    let Some(candidate) = system_soffice() else {
        return false;
    };
    let mut command = Command::new(&candidate);
    command
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let Ok(child) = command.spawn() else {
        return false;
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(20), child.wait_with_output()).await,
        Ok(Ok(output)) if output.status.success()
    )
}

/// Serializes conversions; see the call site for why concurrency here is a
/// hazard rather than a win.
fn conversion_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
    &LOCK
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
    let profile_uri = file_uri(&profile)
        .map_err(|error| ConversionError::Failed(format!("workspace: {error}")))?;
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
        .arg(format!("-env:UserInstallation={profile_uri}"))
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
        // The reason travels to the failure card, so it names what actually
        // happened: LibreOffice's own words when it said any, its exit status
        // when it did not.
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = brief(detail.trim());
        return Err(ConversionError::Failed(if detail.is_empty() {
            format!(
                "LibreOffice produced no PDF ({})",
                if output.status.success() {
                    "it exited cleanly without writing one".to_owned()
                } else {
                    output.status.to_string()
                }
            )
        } else {
            format!("LibreOffice failed: {detail}")
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

/// The leading slice of a tool's stderr that fits a failure card. LibreOffice
/// can dump pages; the first lines carry the diagnosis.
fn brief(detail: &str) -> String {
    const MAX_CHARS: usize = 400;
    if detail.chars().count() <= MAX_CHARS {
        return detail.to_owned();
    }
    let mut cut: String = detail.chars().take(MAX_CHARS).collect();
    cut.push('…');
    cut
}

/// A percent-encoded `file:` URI for LibreOffice's `UserInstallation` value.
///
/// Temp roots can contain spaces, `#`, or other URI-reserved characters.
/// Passing the display path verbatim makes LibreOffice reject the value with
/// "The string contains invalid characters" before it opens the deck.
fn file_uri(path: &Path) -> Result<String, &'static str> {
    url::Url::from_file_path(path)
        .map(|uri| uri.into())
        .map_err(|()| "could not encode the LibreOffice profile path")
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

/// Host converter registered with the embedded server's exec provider.
///
/// The same LibreOffice resolution, containment, and on-disk conversion cache
/// as the preview panel above, exposed behind the exec render seam so the
/// model's office visual-QA loop and the preview panel can never disagree
/// about which converter this machine has.
pub(crate) struct ExecOfficeConverter {
    data_dir: PathBuf,
}

impl ExecOfficeConverter {
    pub(crate) fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

#[async_trait::async_trait]
impl openwave_code_execution::HostOfficeConverter for ExecOfficeConverter {
    async fn convert_to_pdf(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<Vec<u8>, openwave_code_execution::OfficeConvertError> {
        use openwave_code_execution::OfficeConvertError;
        if bytes.is_empty() || bytes.len() > MAX_BINARY_DELIVERABLE_BYTES {
            return Err(OfficeConvertError::Failed(
                "the document is empty or too large to convert".into(),
            ));
        }
        let cache_dir = self.data_dir.join(CACHE_DIRECTORY);
        let cache_path = cache_dir.join(format!("{}.pdf", content_key(bytes)));
        if let Ok(cached) = tokio::fs::read(&cache_path).await {
            if !cached.is_empty() {
                return Ok(cached);
            }
        }
        // Serialize with the preview panel's conversions: two cold
        // LibreOffice launches race, and both callers share one cache.
        let _serialized = conversion_lock().lock().await;
        if let Ok(cached) = tokio::fs::read(&cache_path).await {
            if !cached.is_empty() {
                return Ok(cached);
            }
        }
        let Some(soffice) = locate_soffice(&self.data_dir) else {
            return Err(OfficeConvertError::ConverterMissing);
        };
        match run_conversion(&soffice, bytes, extension).await {
            Ok(pdf) => {
                // Cache best-effort, shared with the preview panel: the same
                // deck previewed and QA-rendered converts once.
                let _ = store_cached_pdf(&cache_dir, &cache_path, &pdf).await;
                Ok(pdf)
            }
            // A resolved path that will not spawn is the same state as no
            // LibreOffice at all.
            Err(ConversionError::Spawn) => Err(OfficeConvertError::ConverterMissing),
            Err(ConversionError::Failed(reason)) => Err(OfficeConvertError::Failed(reason)),
        }
    }
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

    /// A verified managed install answers resolution outright; the system is
    /// only probed when there is none. Probing order matters because the
    /// system scan reads `PATH` and can resolve stale launcher scripts.
    #[test]
    fn managed_install_preempts_the_system_scan() {
        let managed = PathBuf::from("/data/tools/libreoffice/x/soffice");
        assert_eq!(
            resolve_soffice(Some(managed.clone()), || {
                panic!("system scan ran despite a managed install")
            }),
            Some(managed)
        );
        let system = PathBuf::from("/usr/bin/soffice");
        assert_eq!(resolve_soffice(None, || Some(system.clone())), Some(system));
    }

    #[test]
    fn libreoffice_profile_uri_encodes_reserved_path_characters() {
        let path = if cfg!(windows) {
            Path::new(r"C:\OpenWave Preview\profile #1")
        } else {
            Path::new("/tmp/OpenWave Preview/profile #1")
        };

        let uri = file_uri(path).unwrap();

        assert!(uri.starts_with("file:///"), "{uri}");
        assert!(uri.contains("OpenWave%20Preview/profile%20%231"), "{uri}");
    }

    /// End-to-end proof against a real LibreOffice, when one is installed.
    /// Skips silently otherwise — CI runners and most dev machines carry no
    /// LibreOffice, and its absence is exactly the state the feature designs
    /// for, not a test failure.
    #[tokio::test]
    async fn converts_a_real_deck_when_libreoffice_is_installed() {
        let Some(soffice) = system_soffice() else {
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
