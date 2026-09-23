//! Office-document-to-PDF conversion for inline previews.
//!
//! Presentations and the high-fidelity spreadsheet view are converted to PDF
//! and drawn by the PDF viewer. Conversion prefers the app's own managed LibreOffice
//! (downloaded and digest-verified by [`crate::office_install`] the first
//! time a preview needs it, on macOS), then falls back to one the user
//! installed, provided it is at least the pinned version. Its absence is a
//! first-class state the renderer turns into a download or an install hint,
//! not an error.
//!
//! The converter processes untrusted bytes, so it only runs inside a
//! confinement boundary: on macOS, `sandbox-exec` with the profile in
//! [`crate::office_sandbox`] — no IP network, no writes outside the throwaway
//! directory, the user's files unreadable — plus an empty environment, a
//! throwaway working directory that also holds the LibreOffice profile, piped
//! stdio, a hard timeout with `kill_on_drop`, and size caps on both input and
//! output. Hosts without a confinement implementation (everything but macOS
//! today) report the converter unavailable instead of converting; those
//! hygiene measures are not a security boundary on their own.
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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tidebreak_core::MAX_BINARY_DELIVERABLE_BYTES;
use tokio::process::Command;

/// LibreOffice loads the whole document before exporting; 90 seconds is
/// generous for any document the 16 MB input cap admits, and bounds a hang on a
/// crafted file.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(90);

/// A PDF export larger than this is not a preview any more. Four times the
/// binary-output ceiling covers documents whose images recompress badly.
const MAX_PDF_BYTES: u64 = 4 * MAX_BINARY_DELIVERABLE_BYTES as u64;

/// Disk budget for cached conversions. Oldest files are pruned past this;
/// a pruned entry just reconverts on next view.
const CACHE_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

const CACHE_DIRECTORY: &str = "derived/office-pdf";

/// Whether this host confines the converter. LibreOffice parses untrusted
/// document bytes, and the throwaway profile plus cleared environment are
/// hygiene, not a boundary — so where no confinement implementation exists
/// (everywhere but macOS's Seatbelt profile today) the converter is treated
/// as unavailable rather than launched directly. A future confined Windows
/// converter widens this alongside its own boundary.
const CONVERTER_CONFINED: bool = cfg!(target_os = "macos");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OfficePdfRequest {
    /// The office document's bytes, base64 like every other bulk IPC payload.
    content_base64: String,
    /// Stored media type of the source; picks the input filename extension
    /// LibreOffice keys its import filter on.
    media_type: String,
}

/// Outcome of a conversion request. A missing converter is a state the
/// renderer designs for, not a failure: on macOS it triggers the managed
/// download (unless one already failed this run), elsewhere the install hint.
///
/// `rename_all` alone renames the variants and leaves their fields in
/// snake_case, so `rename_all_fields` is what actually makes the payload the
/// renderer reads (`pdfBase64`, `installFailure`); the test below pins both.
#[derive(Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "status"
)]
pub(crate) enum OfficePdfResult {
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
    Failed {
        /// Short, user-facing summary of the failure.
        message: String,
        /// Complete converter diagnostics, safe to carry over Tauri IPC.
        details: String,
    },
}

fn converter_missing() -> OfficePdfResult {
    OfficePdfResult::ConverterMissing {
        installable: crate::office_install::supported(),
        install_failure: crate::office_install::last_failure(),
    }
}

/// Convert one supported office document's bytes to PDF with LibreOffice.
///
/// Bytes travel in and out rather than a document identity so the one command
/// serves both transports the viewers already use — HTTP-fetched source
/// documents and IPC-read output revisions — without duplicating either
/// resolution path here.
#[tauri::command]
pub(crate) async fn convert_office_to_pdf(
    app: AppHandle,
    request: OfficePdfRequest,
) -> Result<OfficePdfResult, String> {
    let extension = input_extension(&request.media_type)
        .ok_or_else(|| "That file type has no office preview".to_owned())?;
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
    let cache_path = cache_dir.join(format!("{}.pdf", content_key(&bytes, extension)));
    if let Ok(cached) = tokio::fs::read(&cache_path).await {
        if !cached.is_empty() {
            return Ok(OfficePdfResult::Converted {
                pdf_base64: BASE64.encode(cached),
            });
        }
    }

    // One conversion at a time. Duplicate requests for the same document arrive
    // routinely (two panels, or a fetch retried around an aborted render) and
    // used to race two cold LibreOffice launches — flaky in exactly the
    // hard-to-reproduce way, and their `.partial` cache staging collided.
    // Serialized, the second request waits and is answered from the cache the
    // first one just wrote.
    let _serialized = conversion_lock().lock().await;
    if let Ok(cached) = tokio::fs::read(&cache_path).await {
        if !cached.is_empty() {
            return Ok(OfficePdfResult::Converted {
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
        Err(ConversionError::Sandbox(reason)) => {
            return Ok(OfficePdfResult::Failed {
                message: "Could not sandbox LibreOffice".to_owned(),
                details: sanitize_diagnostic(&reason),
            })
        }
        Err(ConversionError::Failed(failure)) => {
            return Ok(OfficePdfResult::Failed {
                message: failure.message,
                details: sanitize_diagnostic(&failure.details),
            })
        }
    };

    // Cache best-effort: a preview that converted but failed to persist is
    // still a preview.
    let _ = store_cached_pdf(&cache_dir, &cache_path, &pdf).await;

    Ok(OfficePdfResult::Converted {
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
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/vnd.ms-excel" => Some("xls"),
        "application/vnd.oasis.opendocument.spreadsheet" => Some("ods"),
        _ => None,
    }
}

/// Cache key: the conversion profile plus the SHA-256 of the source bytes.
/// The profile is part of the identity because Calc's one-page-per-sheet
/// export is intentionally different from LibreOffice's default print export.
fn content_key(bytes: &[u8], extension: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"office-pdf-v2\0");
    hasher.update(extension.as_bytes());
    hasher.update(b"\0");
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
/// install first, then one the user installed at least as new as the pinned
/// version, system-wide before `PATH`.
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
/// system is not probed at all behind one. On a host without converter
/// confinement nothing resolves, however many LibreOffices are installed —
/// the missing-converter state is the only one conversion may answer with.
fn resolve_soffice(
    managed: Option<PathBuf>,
    system: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if !CONVERTER_CONFINED {
        return None;
    }
    managed.or_else(system)
}

/// A LibreOffice the user installed: the standard install locations first,
/// then `PATH`, passing over any install [`system_install_refusal`] turns
/// down.
fn system_soffice() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|dir| PATH_BINARY_NAMES.iter().map(move |name| dir.join(name)));
    standard_install_paths()
        .into_iter()
        .chain(on_path)
        .filter(|candidate| candidate.is_file())
        .find(|candidate| match system_install_refusal(candidate) {
            None => true,
            Some(reason) => {
                report_skipped_system_install(candidate, &reason);
                false
            }
        })
}

/// Why a LibreOffice the user installed may not convert, or `None` when it
/// may. This app does not control that install's version, and an older
/// release can carry parser bugs the pinned one fixed, so it must be at least
/// [`crate::office_install::LIBREOFFICE_VERSION`]. The version comes from the
/// bundle's `Info.plist`, so the install must resolve to a binary inside an
/// application bundle; a launcher script outside one is turned down too.
#[cfg(target_os = "macos")]
fn system_install_refusal(candidate: &Path) -> Option<String> {
    let minimum = crate::office_install::LIBREOFFICE_VERSION;
    let binary = match std::fs::canonicalize(candidate) {
        Ok(binary) => binary,
        Err(error) => return Some(format!("it could not be resolved: {error}")),
    };
    let Some(bundle) = crate::office_sandbox::app_bundle(&binary) else {
        return Some(
            "it is not inside an application bundle, so its version cannot be checked".to_owned(),
        );
    };
    match bundle_version(bundle) {
        Some(version) if version_at_least(&version, minimum) => None,
        Some(version) => Some(format!(
            "it is version {version}, older than {minimum}, the version Tidebreak installs"
        )),
        None => Some(format!(
            "its version could not be read from {}",
            bundle.join("Contents/Info.plist").display()
        )),
    }
}

/// Hosts other than macOS resolve no converter at all, so nothing reaches
/// this. It refuses anyway, so a host that later gains confinement has to add
/// its own version check before a system install can convert.
#[cfg(not(target_os = "macos"))]
fn system_install_refusal(_candidate: &Path) -> Option<String> {
    Some("this host cannot check its version".to_owned())
}

/// The version an application bundle declares in its `Info.plist`.
#[cfg(target_os = "macos")]
fn bundle_version(bundle: &Path) -> Option<String> {
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    info.as_dictionary()?
        .get("CFBundleShortVersionString")?
        .as_string()
        .map(str::to_owned)
}

/// Whether a dotted version is at least `minimum`, compared number by number
/// with missing parts read as zero: `25.8.7.3` meets `25.8.7`, and `7.6.4.1`
/// does not, although it sorts after it as text. A version that is not all
/// dotted decimal numbers meets nothing.
#[cfg(any(target_os = "macos", test))]
fn version_at_least(version: &str, minimum: &str) -> bool {
    fn parts(version: &str) -> Option<Vec<u64>> {
        version
            .split('.')
            .map(|part| {
                if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                part.parse().ok()
            })
            .collect()
    }
    let (Some(version), Some(minimum)) = (parts(version), parts(minimum)) else {
        return false;
    };
    let width = version.len().max(minimum.len());
    let padded = |parts: &[u64]| {
        (0..width)
            .map(|index| parts.get(index).copied().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    padded(&version) >= padded(&minimum)
}

/// Logs why a system LibreOffice was passed over, once per reason per app
/// run: resolution runs on every conversion and every tool status check.
fn report_skipped_system_install(candidate: &Path, reason: &str) {
    static REPORTED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::LazyLock::new(Default::default);
    let line = format!(
        "not using the LibreOffice at {}: {reason}",
        candidate.display()
    );
    let first = REPORTED
        .lock()
        .map(|mut reported| reported.insert(line.clone()))
        .unwrap_or(true);
    if first {
        eprintln!("tidebreak-desktop: {line}");
    }
}

/// Whether a system LibreOffice not only resolves but actually runs.
///
/// The install warm-up must not be talked out of downloading by a leftover
/// launcher script on `PATH` — a Homebrew shim pointing at a removed
/// `/Applications` bundle resolves as a file, spawns, and exits 126/127. One
/// cheap `--version` probe (called at most once per app run) separates a
/// converter that will work from remains that will not.
///
/// On a host without converter confinement the answer is false regardless of
/// what is installed: a converter that conversion refuses to run is not
/// workable, and the tool-status seam must agree with the converter about it.
pub(crate) async fn workable_system_soffice() -> bool {
    if !CONVERTER_CONFINED {
        return false;
    }
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
    /// The confinement itself could not be established. Deliberately its own
    /// state: it says nothing about the document and everything about this
    /// host, and it must never be answered by running LibreOffice unconfined.
    Sandbox(String),
    /// LibreOffice ran and did not produce a usable PDF.
    Failed(ConversionFailure),
}

struct ConversionFailure {
    message: String,
    details: String,
}

impl ConversionFailure {
    fn new(message: impl Into<String>, details: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: details.into(),
        }
    }

    fn message(message: impl Into<String>) -> Self {
        let message = message.into();
        Self::new(message.clone(), message)
    }
}

/// The command that runs one conversion: `sandbox-exec` wrapping `soffice`
/// with the profile from [`crate::office_sandbox`]. macOS is the only host
/// with a confinement implementation, and therefore the only one that
/// converts at all.
#[cfg(target_os = "macos")]
fn converter_command(soffice: &Path, workdir: &Path) -> Result<Command, ConversionError> {
    // A converter that vanished between resolution and here is the
    // missing-converter state, not a sandbox fault; the profile builder would
    // otherwise report its failure to resolve the path.
    if !soffice.exists() {
        return Err(ConversionError::Spawn);
    }
    crate::office_sandbox::confined_command(soffice, workdir).map_err(ConversionError::Sandbox)
}

/// Unreachable through resolution — an unconfined host resolves no converter
/// — and kept a refusal rather than a direct launch so no future call path
/// can run LibreOffice against untrusted bytes outside a boundary. A
/// confined Windows converter replaces this with its own equivalent of the
/// Seatbelt branch above.
#[cfg(not(target_os = "macos"))]
fn converter_command(_soffice: &Path, _workdir: &Path) -> Result<Command, ConversionError> {
    Err(ConversionError::Sandbox(
        "this host has no confinement implementation for the converter".to_owned(),
    ))
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
        .prefix("tidebreak-office-pdf-")
        .tempdir()
        .map_err(|error| {
            ConversionError::Failed(ConversionFailure::message(format!("workspace: {error}")))
        })?;
    let input = workdir.path().join(format!("document.{extension}"));
    let out_dir = workdir.path().join("out");
    let profile = workdir.path().join("profile");
    let profile_uri = file_uri(&profile).map_err(|error| {
        ConversionError::Failed(ConversionFailure::message(format!("workspace: {error}")))
    })?;
    tokio::fs::write(&input, bytes).await.map_err(|error| {
        ConversionError::Failed(ConversionFailure::message(format!("workspace: {error}")))
    })?;
    tokio::fs::create_dir(&out_dir).await.map_err(|error| {
        ConversionError::Failed(ConversionFailure::message(format!("workspace: {error}")))
    })?;

    let mut command = converter_command(soffice, workdir.path())?;
    let export_filter = pdf_export_filter(extension);

    command
        .arg("--headless")
        .arg("--nologo")
        .arg("--norestore")
        .arg("--nolockcheck")
        .arg("--nofirststartwizard")
        .arg(format!("-env:UserInstallation={profile_uri}"))
        .arg("--convert-to")
        .arg(export_filter)
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
        .map_err(|_| {
            ConversionError::Failed(ConversionFailure::message(
                "Converting the document timed out",
            ))
        })?
        .map_err(|error| {
            ConversionError::Failed(ConversionFailure::message(format!("conversion: {error}")))
        })?;

    // LibreOffice can exit zero without writing anything, so the produced
    // file — present and non-empty — is the success signal, not the code.
    let produced = out_dir.join("document.pdf");
    let metadata = tokio::fs::metadata(&produced).await;
    let produced_len = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
    if !output.status.success() || produced_len == 0 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = stderr.trim();
        let stdout = stdout.trim();
        // `sandbox-exec` announces its own failures — an unusable profile, a
        // converter it could not launch under one — before LibreOffice writes
        // anything. Keeping that distinct matters: it is a host problem to
        // report, not a document that failed to convert.
        if stderr.starts_with("sandbox-exec:") {
            return Err(ConversionError::Sandbox(stderr.to_owned()));
        }
        // Package managers leave launcher scripts behind after the
        // application is removed; those spawn fine and then exit with the
        // shell's not-found / not-executable codes. Same state as no install.
        if matches!(output.status.code(), Some(126) | Some(127)) {
            return Err(ConversionError::Spawn);
        }
        // The reason travels to the failure card, so it names what actually
        // happened: LibreOffice's own words when it said any, its exit status
        // when it did not.
        let message = if stderr.is_empty() {
            format!(
                "LibreOffice produced no PDF ({})",
                if output.status.success() {
                    "it exited cleanly without writing one".to_owned()
                } else {
                    output.status.to_string()
                }
            )
        } else {
            format!("LibreOffice failed: {}", first_line(stderr))
        };
        let mut details = format!("Exit status: {}", output.status);
        if !stderr.is_empty() {
            details.push_str("\n\nStandard error:\n");
            details.push_str(stderr);
        }
        if !stdout.is_empty() {
            details.push_str("\n\nStandard output:\n");
            details.push_str(stdout);
        }
        return Err(ConversionError::Failed(ConversionFailure::new(
            message, details,
        )));
    }
    if produced_len > MAX_PDF_BYTES {
        return Err(ConversionError::Failed(ConversionFailure::message(
            "The converted preview is too large to show",
        )));
    }
    tokio::fs::read(&produced).await.map_err(|error| {
        ConversionError::Failed(ConversionFailure::message(format!("conversion: {error}")))
    })
}

fn pdf_export_filter(extension: &str) -> &'static str {
    match extension {
        // Preserve a spreadsheet as one complete canvas per sheet instead of
        // chopping dashboards into ordinary printer pages.
        "xlsx" | "xls" | "ods" => concat!(
            "pdf:calc_pdf_Export:",
            r#"{"SinglePageSheets":{"type":"boolean","value":"true"}}"#
        ),
        _ => "pdf",
    }
}

fn first_line(detail: &str) -> &str {
    detail.lines().next().unwrap_or(detail)
}

/// Tauri command results are JSON strings. Converter diagnostics can contain
/// C0 control bytes that serde_json rejects with “The string contains invalid
/// characters”, hiding LibreOffice's real failure. Preserve whitespace that
/// helps troubleshooting while replacing only characters JSON cannot carry.
fn sanitize_diagnostic(detail: &str) -> String {
    detail
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' => character,
            character if character.is_control() => '�',
            character => character,
        })
        .collect()
}

/// A percent-encoded `file:` URI for LibreOffice's `UserInstallation` value.
///
/// Temp roots can contain spaces, `#`, or other URI-reserved characters.
/// Passing the display path verbatim makes LibreOffice reject the value with
/// "The string contains invalid characters" before it opens the document.
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
impl tidebreak_code_execution::HostOfficeConverter for ExecOfficeConverter {
    async fn convert_to_pdf(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<Vec<u8>, tidebreak_code_execution::OfficeConvertError> {
        use tidebreak_code_execution::OfficeConvertError;
        if bytes.is_empty() || bytes.len() > MAX_BINARY_DELIVERABLE_BYTES {
            return Err(OfficeConvertError::Failed(
                "the document is empty or too large to convert".into(),
            ));
        }
        let cache_dir = self.data_dir.join(CACHE_DIRECTORY);
        let cache_path = cache_dir.join(format!("{}.pdf", content_key(bytes, extension)));
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
                // document previewed and QA-rendered converts once.
                let _ = store_cached_pdf(&cache_dir, &cache_path, &pdf).await;
                Ok(pdf)
            }
            // A resolved path that will not spawn is the same state as no
            // LibreOffice at all.
            Err(ConversionError::Spawn) => Err(OfficeConvertError::ConverterMissing),
            // A confinement failure is reported as itself, not folded into a
            // conversion failure: the document is fine and the host is not.
            Err(ConversionError::Sandbox(reason)) => Err(OfficeConvertError::Failed(format!(
                "could not sandbox LibreOffice: {reason}"
            ))),
            Err(ConversionError::Failed(failure)) => Err(OfficeConvertError::Failed(format!(
                "{}\n{}",
                failure.message, failure.details
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_media_types_map_to_import_extensions() {
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
        assert_eq!(
            input_extension("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            Some("xlsx")
        );
        assert_eq!(input_extension("application/vnd.ms-excel"), Some("xls"));
        assert_eq!(input_extension("application/pdf"), None);
    }

    #[test]
    fn spreadsheet_export_keeps_each_sheet_on_one_pdf_page() {
        assert!(pdf_export_filter("xlsx").contains("calc_pdf_Export"));
        assert!(pdf_export_filter("xlsx").contains("SinglePageSheets"));
        assert_eq!(pdf_export_filter("pptx"), "pdf");
    }

    /// A verified managed install answers resolution outright; the system is
    /// only probed when there is none. Probing order matters because the
    /// system scan reads `PATH` and can resolve stale launcher scripts.
    /// macOS-only because it is the only host where resolution resolves at
    /// all; the gate below pins the others.
    #[cfg(target_os = "macos")]
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

    /// The platform gate: a host without a confinement implementation
    /// resolves no converter even with LibreOffice on offer — the preview
    /// reports the converter unavailable rather than launching it against
    /// untrusted documents outside a boundary. On macOS resolution keeps
    /// working; everywhere else this asserts the refusal.
    #[test]
    fn unconfined_hosts_resolve_no_converter() {
        let resolved = resolve_soffice(
            Some(PathBuf::from("/data/tools/libreoffice/x/soffice")),
            || Some(PathBuf::from("/usr/bin/soffice")),
        );
        assert_eq!(resolved.is_some(), cfg!(target_os = "macos"));
    }

    /// A system LibreOffice must reach the pinned version, compared number by
    /// number rather than as text, before it parses untrusted documents.
    #[test]
    fn system_version_must_reach_the_pinned_version() {
        for newer_or_equal in ["25.8.7", "25.8.7.0", "25.8.7.3", "25.8.10.1", "26.2.0.3"] {
            assert!(
                version_at_least(newer_or_equal, "25.8.7"),
                "{newer_or_equal}"
            );
        }
        // `7.6.4.1` sorts after `25.8.7` as text but is years older.
        for older in ["25.8.6.2", "25.2.7.2", "25.8", "7.6.4.1"] {
            assert!(!version_at_least(older, "25.8.7"), "{older}");
        }
        for unreadable in ["", "25.8.7-beta", "25..7", "v25.8.7", "25.8.7 "] {
            assert!(!version_at_least(unreadable, "25.8.7"), "{unreadable:?}");
        }
    }

    /// Resolution reads the version from the install's own bundle and passes
    /// over anything it cannot vouch for: an older release, a bundle with no
    /// readable version, and a launcher script outside any bundle.
    #[cfg(target_os = "macos")]
    #[test]
    fn system_install_must_be_a_bundle_at_least_the_pinned_version() {
        let root = tempfile::tempdir().expect("root");
        let install = |name: &str, version: Option<&str>| {
            let contents = root.path().join(name).join("Contents");
            std::fs::create_dir_all(contents.join("MacOS")).expect("bundle");
            if let Some(version) = version {
                std::fs::write(
                    contents.join("Info.plist"),
                    format!(
                        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                         <plist version=\"1.0\"><dict>\
                         <key>CFBundleShortVersionString</key><string>{version}</string>\
                         </dict></plist>\n"
                    ),
                )
                .expect("Info.plist");
            }
            let soffice = contents.join("MacOS/soffice");
            std::fs::write(&soffice, b"").expect("soffice");
            soffice
        };
        let pinned = crate::office_install::LIBREOFFICE_VERSION;

        assert_eq!(
            system_install_refusal(&install("Pinned.app", Some(pinned))),
            None
        );
        assert_eq!(
            system_install_refusal(&install("Newer.app", Some(&format!("{pinned}.3")))),
            None
        );
        let older = system_install_refusal(&install("Older.app", Some("7.6.4.1")))
            .expect("an older install is refused");
        assert!(
            older.contains("7.6.4.1") && older.contains(pinned),
            "{older}"
        );
        assert!(system_install_refusal(&install("Unversioned.app", None)).is_some());
        let launcher = root.path().join("soffice");
        std::fs::write(&launcher, b"#!/bin/sh\n").expect("launcher");
        assert!(system_install_refusal(&launcher).is_some());
    }

    #[test]
    fn libreoffice_profile_uri_encodes_reserved_path_characters() {
        let path = if cfg!(windows) {
            Path::new(r"C:\Tidebreak Preview\profile #1")
        } else {
            Path::new("/tmp/Tidebreak Preview/profile #1")
        };

        let uri = file_uri(path).unwrap();

        assert!(uri.starts_with("file:///"), "{uri}");
        assert!(uri.contains("Tidebreak%20Preview/profile%20%231"), "{uri}");
    }

    /// The IPC wire contract for the preview. The renderer reads these keys
    /// by name, and an enum-level `rename_all` renames only the variants —
    /// leaving `pdf_base64` on the wire, which the renderer read as
    /// `undefined` and handed to `atob`, failing every office preview.
    #[test]
    fn conversion_result_serializes_camel_case_variants_and_fields() {
        let converted = serde_json::to_value(OfficePdfResult::Converted {
            pdf_base64: "JVBERi0=".to_owned(),
        })
        .unwrap();
        assert_eq!(
            converted,
            serde_json::json!({ "status": "converted", "pdfBase64": "JVBERi0=" })
        );

        let missing = serde_json::to_value(OfficePdfResult::ConverterMissing {
            installable: true,
            install_failure: Some("Download cancelled".to_owned()),
        })
        .unwrap();
        assert_eq!(
            missing,
            serde_json::json!({
                "status": "converterMissing",
                "installable": true,
                "installFailure": "Download cancelled",
            })
        );

        let failed = serde_json::to_value(OfficePdfResult::Failed {
            message: "LibreOffice failed".to_owned(),
            details: "Exit status: exit status: 1".to_owned(),
        })
        .unwrap();
        assert_eq!(
            failed,
            serde_json::json!({
                "status": "failed",
                "message": "LibreOffice failed",
                "details": "Exit status: exit status: 1",
            })
        );
    }

    #[test]
    fn converter_diagnostics_replace_json_invalid_control_characters() {
        assert_eq!(
            sanitize_diagnostic("first\0line\nsecond\tline\u{001b}"),
            "first�line\nsecond\tline�"
        );
    }

    /// End-to-end proof against a real LibreOffice, when one is installed —
    /// and, on macOS, proof that it converts *under the Seatbelt profile*,
    /// which is the only way that profile is ever checked against a real
    /// `soffice`. Skips silently otherwise — CI runners and most dev machines
    /// carry no LibreOffice, and its absence is exactly the state the feature
    /// designs for, not a test failure.
    #[tokio::test]
    async fn converts_a_real_deck_when_libreoffice_is_installed() {
        if !CONVERTER_CONFINED {
            eprintln!("skipping: this host does not convert");
            return;
        }
        let Some(soffice) = system_soffice() else {
            eprintln!("skipping: no usable system LibreOffice installed");
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
            Err(ConversionError::Sandbox(reason)) => {
                panic!("the converter could not be sandboxed: {reason}")
            }
            Err(ConversionError::Failed(failure)) => {
                panic!(
                    "conversion failed with LibreOffice present: {}\n{}",
                    failure.message, failure.details
                )
            }
        }
    }
}
