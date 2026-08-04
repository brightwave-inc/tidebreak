//! Host-side office-to-PDF rendering for the model's visual QA loop.
//!
//! The document skills teach a validation pass that converts a saved deck or
//! document through LibreOffice and inspects page images. No sandbox can be
//! relied on for that conversion: the local provider pins its `PATH` to
//! system directories and will never see a managed `soffice`, and managed
//! cloud sandboxes ship no LibreOffice today. The converter the host already
//! uses for the preview panel is the one capability that exists everywhere
//! the app runs, so rendering becomes a host service instead of a sandbox
//! binary.
//!
//! Running a converter on the host does not mean running it unconfined: the
//! bytes it parses were written by the sandbox, so the desktop's converter
//! puts `soffice` under its own Seatbelt profile (see the desktop crate's
//! `office_sandbox`), built with the shared helpers in [`crate::sbpl`].
//!
//! The seam is the workspace itself: after an exec lands office files under
//! `output/` in host scratch (directly for the local provider, via the
//! result pull for remote ones), the host converts each to a PDF under
//! [`OFFICE_RENDER_DIR`] and says so in the command's sync notes. The model
//! finishes the loop with the bundled `render_pdf.py` helper — pure pip
//! dependencies that install in every sandbox — which turns the staged PDF
//! into `preview/` images the exec result attaches. Remote sandboxes need the
//! PDF listed in the next call's `files`; the note spells that out.
//!
//! Everything here treats scratch as hostile the way the sync layer does: a
//! sandbox confined to scratch can still plant symlinks in it, so sources are
//! read and targets written only through [`ScratchDir`] handles that refuse
//! symlinked components.

use std::path::Path;

use async_trait::async_trait;

use crate::host_paths::{try_resolve_scratch_directory, ScratchDir, ScratchEntryKind};
use crate::MAX_WORKSPACE_FILE_BYTES;

/// Workspace-relative directory host-converted PDFs are written under,
/// mirroring each source's path relative to `output/`. The PDF for
/// `output/reports/deck.pptx` is `.openwave/render/reports/deck.pptx.pdf` —
/// the full source filename stays in the name so two sources with one stem
/// can never answer for each other.
pub const OFFICE_RENDER_DIR: &str = ".openwave/render";

/// File extensions the render pass converts: the same formats the in-sandbox
/// `render_office.py` helper accepts.
const OFFICE_RENDER_EXTENSIONS: &[&str] = &["docx", "pptx"];

/// The most conversions one exec result will trigger. A QA loop inspects one
/// document at a time; a command that emits a pile of decks should not stall
/// its result behind a conversion queue.
const MAX_RENDERS_PER_EXEC: usize = 3;

/// Why a host conversion did not produce a PDF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfficeConvertError {
    /// No usable LibreOffice on this host. A first-class state, not a fault:
    /// the caller reports it and the skill's reopen checks carry the QA.
    ConverterMissing,
    /// A converter exists and the conversion failed.
    Failed(String),
}

impl std::fmt::Display for OfficeConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConverterMissing => write!(f, "no LibreOffice is available on this host"),
            Self::Failed(reason) => write!(f, "{reason}"),
        }
    }
}

/// Converts one office document's bytes to PDF with a host-provided
/// LibreOffice. The desktop registers its managed-install-backed converter;
/// headless embeddings register none and the render pass degrades to an
/// honest note.
#[async_trait]
pub trait HostOfficeConverter: Send + Sync {
    /// Convert `bytes` — an office document whose format is named by
    /// `extension` (`docx` or `pptx`) — into PDF bytes.
    async fn convert_to_pdf(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<Vec<u8>, OfficeConvertError>;
}

/// One office file found under `output/`, as workspace-relative paths.
struct RenderCandidate {
    /// `output/...` source path, for notes.
    source: String,
    /// Path relative to `output/`, mirrored under the render directory.
    relative: String,
    /// Extension the converter keys its import filter on.
    extension: &'static str,
    modified: Option<std::time::SystemTime>,
}

/// Convert the office files under `<host_dir>/output` into PDFs under
/// [`OFFICE_RENDER_DIR`], returning sync notes describing what happened.
///
/// Nothing here fails the exec: the command already ran, and every outcome —
/// a fresh conversion, an already-current PDF, a missing converter, a failed
/// conversion — degrades into one bounded note the model can act on. A
/// conversion is skipped when its PDF already exists and is at least as new
/// as the source, so reruns of an unchanged deck cost nothing.
pub async fn render_office_outputs(
    converter: Option<&dyn HostOfficeConverter>,
    host_dir: &Path,
) -> Vec<String> {
    let mut notes = Vec::new();
    let candidates = match collect_candidates(host_dir).await {
        Ok(candidates) => candidates,
        Err(note) => {
            if let Some(note) = note {
                notes.push(note);
            }
            return notes;
        }
    };
    if candidates.is_empty() {
        return notes;
    }
    let Some(converter) = converter else {
        notes.push(
            "office render unavailable: this host has no office-to-PDF converter; validate \
             office outputs by reopening them with their library"
                .into(),
        );
        return notes;
    };
    let omitted = candidates.len().saturating_sub(MAX_RENDERS_PER_EXEC);
    for candidate in candidates.into_iter().take(MAX_RENDERS_PER_EXEC) {
        match render_candidate(converter, host_dir, &candidate).await {
            Ok(()) => notes.push(format!(
                "office render: {} -> {OFFICE_RENDER_DIR}/{}.pdf (converted on the host); \
                 render page images with .openwave/exec-scripts/render_pdf.py — on a managed \
                 sandbox, list the PDF in that call's 'files'",
                candidate.source, candidate.relative
            )),
            Err(OfficeConvertError::ConverterMissing) => {
                notes.push(
                    "office render unavailable: no LibreOffice on this host; validate office \
                     outputs by reopening them with their library"
                        .into(),
                );
                break;
            }
            Err(OfficeConvertError::Failed(reason)) => notes.push(format!(
                "office render failed for {}: {reason}",
                candidate.source
            )),
        }
    }
    if omitted > 0 {
        notes.push(format!(
            "office render: {omitted} more office file(s) beyond the {MAX_RENDERS_PER_EXEC}-per-command conversion limit"
        ));
    }
    notes
}

/// Walk `output/` for convertible office files, without following symlinks.
///
/// `Err(None)` means there is no output directory — nothing to do and nothing
/// to say. `Err(Some(note))` reports a directory that exists but cannot be
/// walked.
async fn collect_candidates(host_dir: &Path) -> Result<Vec<RenderCandidate>, Option<String>> {
    let output = try_resolve_scratch_directory(host_dir, "output", false)
        .await
        .map_err(|_| None)?;
    let mut candidates = Vec::new();
    let mut stack: Vec<(ScratchDir, String)> = vec![(output, String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries = dir
            .entries()
            .await
            .map_err(|_| Some("office render skipped: output/ could not be listed".to_owned()))?;
        for entry in entries {
            let relative = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{prefix}/{}", entry.name)
            };
            match entry.kind {
                ScratchEntryKind::Directory => {
                    if let Ok(child) = dir.open_dir(&entry.name).await {
                        stack.push((child, relative));
                    }
                }
                ScratchEntryKind::File => {
                    let Some(extension) = office_extension(&entry.name) else {
                        continue;
                    };
                    let Some(stamp) = dir.file_stamp(&entry.name).await else {
                        continue;
                    };
                    if stamp.len > MAX_WORKSPACE_FILE_BYTES as u64 {
                        continue;
                    }
                    candidates.push(RenderCandidate {
                        source: format!("output/{relative}"),
                        relative,
                        extension,
                        modified: stamp.modified,
                    });
                }
                ScratchEntryKind::Other => {}
            }
        }
    }
    candidates.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(candidates)
}

fn office_extension(name: &str) -> Option<&'static str> {
    let (_, extension) = name.rsplit_once('.')?;
    OFFICE_RENDER_EXTENSIONS
        .iter()
        .find(|known| extension.eq_ignore_ascii_case(known))
        .copied()
}

/// Convert one candidate unless its PDF is already current.
async fn render_candidate(
    converter: &dyn HostOfficeConverter,
    host_dir: &Path,
    candidate: &RenderCandidate,
) -> Result<(), OfficeConvertError> {
    let (source_parent_rel, source_name) = split_relative(&candidate.relative);
    let source_parent =
        try_resolve_scratch_directory(host_dir, &join_under("output", source_parent_rel), false)
            .await
            .map_err(|refusal| {
                OfficeConvertError::Failed(format!("source unreadable: {refusal:?}"))
            })?;

    let target_name = format!("{source_name}.pdf");
    let target_rel = join_under(OFFICE_RENDER_DIR, source_parent_rel);
    // Peek at an existing PDF without scaffolding the render directory: a
    // conversion that never happens (converter missing, source unreadable)
    // should leave no trace.
    if let Ok(existing) = try_resolve_scratch_directory(host_dir, &target_rel, false).await {
        if let (Some(target), Some(source)) = (
            existing
                .file_stamp(&target_name)
                .await
                .and_then(|stamp| stamp.modified),
            candidate.modified,
        ) {
            if target >= source && !existing.is_symlink(&target_name).await {
                // Already converted; the note still points the model at it.
                return Ok(());
            }
        }
    }

    if source_parent.is_symlink(source_name).await {
        return Err(OfficeConvertError::Failed("source is a symlink".into()));
    }
    let bytes = source_parent
        .read_file(source_name)
        .await
        .map_err(|error| OfficeConvertError::Failed(format!("source unreadable: {error}")))?;
    let pdf = converter
        .convert_to_pdf(&bytes, candidate.extension)
        .await?;
    let target_parent = try_resolve_scratch_directory(host_dir, &target_rel, true)
        .await
        .map_err(|refusal| {
            OfficeConvertError::Failed(format!("render directory unavailable: {refusal:?}"))
        })?;
    if target_parent.is_symlink(&target_name).await {
        return Err(OfficeConvertError::Failed(
            "render target is a symlink".into(),
        ));
    }
    target_parent
        .write_file(&target_name, &pdf)
        .await
        .map_err(|error| OfficeConvertError::Failed(format!("PDF unwritable: {error}")))?;
    Ok(())
}

fn split_relative(relative: &str) -> (&str, &str) {
    relative
        .rsplit_once('/')
        .map_or(("", relative), |(parent, name)| (parent, name))
}

fn join_under(root: &str, relative: &str) -> String {
    if relative.is_empty() {
        root.to_owned()
    } else {
        format!("{root}/{relative}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Converts by wrapping the input, so the test can prove which bytes
    /// travelled and that the output landed where the note says.
    struct FakeConverter {
        conversions: AtomicUsize,
    }

    impl FakeConverter {
        fn new() -> Self {
            Self {
                conversions: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl HostOfficeConverter for FakeConverter {
        async fn convert_to_pdf(
            &self,
            bytes: &[u8],
            extension: &str,
        ) -> Result<Vec<u8>, OfficeConvertError> {
            self.conversions.fetch_add(1, Ordering::SeqCst);
            let mut pdf = format!("%PDF({extension})").into_bytes();
            pdf.extend_from_slice(bytes);
            Ok(pdf)
        }
    }

    /// The whole contract in one pass: office files under `output/` convert
    /// into mirrored PDFs under the render directory with an actionable note,
    /// non-office files are ignored, and a rerun over unchanged sources
    /// converts nothing.
    #[tokio::test]
    async fn converts_new_office_outputs_once_and_notes_the_staged_path() {
        let host = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(host.path().join("output/reports")).unwrap();
        std::fs::write(host.path().join("output/deck.pptx"), b"deck bytes").unwrap();
        std::fs::write(host.path().join("output/reports/brief.docx"), b"doc bytes").unwrap();
        std::fs::write(host.path().join("output/data.csv"), b"not office").unwrap();
        let converter = FakeConverter::new();

        let notes = render_office_outputs(Some(&converter), host.path()).await;

        assert_eq!(converter.conversions.load(Ordering::SeqCst), 2);
        assert_eq!(
            std::fs::read(host.path().join(".openwave/render/deck.pptx.pdf")).unwrap(),
            b"%PDF(pptx)deck bytes"
        );
        assert_eq!(
            std::fs::read(host.path().join(".openwave/render/reports/brief.docx.pdf")).unwrap(),
            b"%PDF(docx)doc bytes"
        );
        assert!(!host.path().join(".openwave/render/data.csv.pdf").exists());
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("output/deck.pptx")
                && notes[0].contains(".openwave/render/deck.pptx.pdf")
                && notes[0].contains("render_pdf.py")
                && notes[0].contains("'files'"),
            "{notes:?}"
        );

        // Unchanged sources do not reconvert; the notes still point at the
        // existing PDFs so the model knows they are there.
        let again = render_office_outputs(Some(&converter), host.path()).await;
        assert_eq!(converter.conversions.load(Ordering::SeqCst), 2);
        assert_eq!(again.len(), 2, "{again:?}");
    }

    /// Degraded states stay honest and never fail the pass: no converter and
    /// no LibreOffice each produce one note naming the fallback, and a
    /// workspace without office outputs says nothing at all.
    #[tokio::test]
    async fn degraded_states_produce_one_honest_note() {
        let host = tempfile::tempdir().unwrap();
        assert!(render_office_outputs(None, host.path()).await.is_empty());

        std::fs::create_dir_all(host.path().join("output")).unwrap();
        std::fs::write(host.path().join("output/deck.pptx"), b"deck").unwrap();
        let notes = render_office_outputs(None, host.path()).await;
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("office render unavailable"), "{notes:?}");

        struct Missing;
        #[async_trait]
        impl HostOfficeConverter for Missing {
            async fn convert_to_pdf(
                &self,
                _bytes: &[u8],
                _extension: &str,
            ) -> Result<Vec<u8>, OfficeConvertError> {
                Err(OfficeConvertError::ConverterMissing)
            }
        }
        let notes = render_office_outputs(Some(&Missing), host.path()).await;
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("no LibreOffice"), "{notes:?}");
        assert!(!host.path().join(".openwave/render").exists());
    }

    /// Local exec is confined to scratch but can plant symlinks in it; the
    /// pass must neither read a source through one nor write a PDF through
    /// one.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_sources_and_targets_are_refused() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.pptx"), b"host secret").unwrap();
        let host = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(host.path().join("output")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.pptx"),
            host.path().join("output/deck.pptx"),
        )
        .unwrap();
        let converter = FakeConverter::new();
        render_office_outputs(Some(&converter), host.path()).await;
        assert_eq!(converter.conversions.load(Ordering::SeqCst), 0);

        // A symlinked render directory must not receive writes either; the
        // conversion may have run, but nothing lands outside scratch.
        std::fs::remove_file(host.path().join("output/deck.pptx")).unwrap();
        std::fs::write(host.path().join("output/deck.pptx"), b"real deck").unwrap();
        std::fs::create_dir_all(host.path().join(".openwave")).unwrap();
        std::os::unix::fs::symlink(outside.path(), host.path().join(".openwave/render")).unwrap();
        let notes = render_office_outputs(Some(&converter), host.path()).await;
        assert!(
            notes
                .iter()
                .any(|note| note.contains("office render failed")),
            "{notes:?}"
        );
        assert!(!outside.path().join("deck.pptx.pdf").exists());
    }
}
