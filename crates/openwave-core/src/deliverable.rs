//! Closed contract for user-visible files created in conversation-private scratch.
//!
//! An output is a conversation-owned record with an opaque [`OutputId`] and an
//! ordered history of immutable revisions. The record is authoritative: the
//! filename is display metadata, not identity, so re-creating the same
//! filename adds a revision instead of destroying the previous bytes.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::citation::{AssistantCitationReference, CitationLocation};
use crate::id::{AgentRunId, ChatId, OutputCitationId, OutputId, OutputRevisionId, TurnId};

/// Private-scratch directory holding legacy filename-addressed output files.
///
/// The desktop catalog still reads this compatibility surface.
/// `create_deliverable` writes it first, validates those bytes at the
/// publication boundary, then snapshots them under [`OUTPUTS_DIRECTORY`].
/// Identity here is the filename, and rewriting a file destroys the bytes it
/// replaced.
///
/// The durable record layer below is the intended replacement; see
/// [`OUTPUTS_DIRECTORY`].
pub const DELIVERABLES_DIRECTORY: &str = "artifacts";
/// Private-scratch directory holding immutable revision bytes.
///
/// Paired with the `output` and `output_revision` tables. The store layer is
/// complete and owns every new text-deliverable publication. Projecting these
/// records through the desktop catalog is a separate slice.
pub const OUTPUTS_DIRECTORY: &str = "outputs";
/// Largest text artifact the foreground agent may create.
pub const MAX_DELIVERABLE_BYTES: usize = 512 * 1024;
/// Largest binary workspace artifact the host may accept into an output.
///
/// A binary artifact only enters the record through host acceptance of a file
/// pulled back with `WorkspaceLifecycle::get_workspace_file`, which itself
/// bounds a single transfer at 16 MiB. Matching that transfer ceiling keeps the
/// acceptance invariant simple: every file the workspace is willing to hand
/// back fits, so acceptance never has to reject a well-formed artifact a
/// provider already produced. It is 32x the text cap yet still small enough to
/// buffer one revision in memory while publishing it to private scratch and
/// exporting it.
pub const MAX_BINARY_DELIVERABLE_BYTES: usize = 16 * 1024 * 1024;
/// Largest filename accepted by the model-facing and native boundaries.
pub const MAX_DELIVERABLE_NAME_CHARS: usize = 120;
/// Largest declared media type accepted for a binary workspace artifact.
pub const MAX_DELIVERABLE_MEDIA_TYPE_CHARS: usize = 127;
/// Largest number of revisions one output retains.
///
/// Reaching the bound is a product decision rather than a storage failure: the
/// store refuses a further revision so no caller can silently lose history.
pub const MAX_OUTPUT_REVISIONS: u32 = 100;
/// Most distinct source-evidence rows one output revision may cite.
///
/// An output can only cite evidence shown to the turn that produced it, so the
/// same retrieval-result bound used for assistant messages is sufficient.
pub const MAX_OUTPUT_CITATIONS: usize = crate::citation::MAX_ASSISTANT_CITATIONS;

/// Private-scratch location of one immutable revision's bytes.
///
/// Revision files are written once and never replaced, so the path is derived
/// entirely from durable identity. Callers resolve it below the exact chat's
/// private scratch directory; it is never returned to a model or renderer.
#[must_use]
pub fn output_revision_relative_path(
    output_id: OutputId,
    revision_id: OutputRevisionId,
) -> PathBuf {
    PathBuf::from(OUTPUTS_DIRECTORY)
        .join(output_id.to_string())
        .join(revision_id.to_string())
}

/// One conversation-owned output and its current revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRecord {
    /// Durable opaque identity, stable across every revision.
    pub id: OutputId,
    /// Conversation that exclusively owns this output.
    pub chat_id: ChatId,
    /// Display filename. Not identity, and not unique within a chat.
    pub filename: String,
    /// Fixed media type set when the output was created: derived from `filename`
    /// for a text deliverable, or the explicit type of an accepted binary
    /// artifact.
    pub media_type: String,
    /// Revision currently presented as the output's content.
    pub current_revision: OutputRevisionId,
    /// Number of retained revisions, always at least one.
    pub revision_count: u32,
    /// Creation time of the first revision.
    pub created_at: DateTime<Utc>,
    /// Creation time of the current revision.
    pub updated_at: DateTime<Utc>,
    /// Set when the user deleted the output. Revisions are retained until the
    /// conversation itself is deleted so a deletion stays recoverable.
    pub deleted_at: Option<DateTime<Utc>>,
}

/// One immutable revision of an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRevision {
    /// Durable identity, also the private-scratch filename of its bytes.
    pub id: OutputRevisionId,
    /// Output this revision belongs to.
    pub output_id: OutputId,
    /// One-based position in the output's revision history.
    pub ordinal: u32,
    /// Exact byte length of the revision's content.
    pub byte_len: u64,
    /// SHA-256 of the revision's content, used to recognize an exact retry.
    pub sha256: [u8; 32],
    /// Turn that produced the revision, when it came from a foreground turn.
    ///
    /// Mutually exclusive with [`OutputRevision::producing_run_id`]: a revision
    /// records the foreground turn or the background run that produced it, never
    /// both.
    pub turn_id: Option<TurnId>,
    /// Background run that produced the revision, when it came from a background
    /// run rather than a foreground turn.
    ///
    /// Added alongside `turn_id` rather than replacing it: existing readers of
    /// `turn_id` are unchanged, and a revision that predates background-run
    /// production continues to carry only its turn.
    pub producing_run_id: Option<AgentRunId>,
    /// Host-stamped creation time.
    pub created_at: DateTime<Utc>,
}

/// The foreground turn or background run that produced an output revision.
///
/// Callers that mint a revision name exactly one producer through this enum; the
/// store records it into the mutually exclusive `turn_id` / `producing_run_id`
/// columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionProducer {
    /// A foreground turn produced the revision. Only a turn producer may carry
    /// retrieval citations, which resolve against the turn's evidence.
    Turn(TurnId),
    /// A background run produced the revision.
    Run(AgentRunId),
}

impl RevisionProducer {
    /// The producing turn, when the producer is a foreground turn.
    #[must_use]
    pub fn turn_id(self) -> Option<TurnId> {
        match self {
            Self::Turn(turn_id) => Some(turn_id),
            Self::Run(_) => None,
        }
    }

    /// The producing run, when the producer is a background run.
    #[must_use]
    pub fn producing_run_id(self) -> Option<AgentRunId> {
        match self {
            Self::Run(run_id) => Some(run_id),
            Self::Turn(_) => None,
        }
    }
}

/// Renderer-safe historical citation projected from immutable output evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
pub struct OutputCitationSnapshot {
    pub id: OutputCitationId,
    #[serde(skip)]
    pub output_revision_id: OutputRevisionId,
    pub ordinal: u16,
    pub excerpt: String,
    pub heading: Option<String>,
    /// Where the passage sits in its source, in the terms that source is
    /// addressed by.
    pub location: CitationLocation,
}

/// Content-identifying fields of a revision the caller has already written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutputRevision {
    /// Caller-minted identity. Reusing it with the same content is an exact
    /// retry; reusing it with different content is rejected.
    pub id: OutputRevisionId,
    /// Exact byte length of the content already written to private scratch.
    pub byte_len: u64,
    /// SHA-256 of that content.
    pub sha256: [u8; 32],
    /// Foreground turn that produced the revision, when one did. Mutually
    /// exclusive with `producing_run_id`.
    pub turn_id: Option<TurnId>,
    /// Background run that produced the revision, when one did. Mutually
    /// exclusive with `turn_id`.
    pub producing_run_id: Option<AgentRunId>,
    /// Ordered evidence references selected while producing this revision.
    ///
    /// References only resolve against the revision's chat and producing turn;
    /// a cited revision therefore always has a producing turn.
    pub citations: Vec<AssistantCitationReference>,
    /// Host-stamped creation time.
    pub created_at: DateTime<Utc>,
}

impl NewOutputRevision {
    /// Record the producer that minted this revision into its turn/run fields.
    #[must_use]
    pub fn with_producer(mut self, producer: RevisionProducer) -> Self {
        self.turn_id = producer.turn_id();
        self.producing_run_id = producer.producing_run_id();
        self
    }
}

/// Whether an output holds model-authored text or a host-accepted binary
/// artifact. The kind fixes the output's media type and its size ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliverableKind {
    /// Model-authored UTF-8 text. The media type is derived from the filename
    /// extension and the size ceiling is [`MAX_DELIVERABLE_BYTES`].
    Text,
    /// A host-accepted binary workspace artifact carrying an explicit media
    /// type. The size ceiling is [`MAX_BINARY_DELIVERABLE_BYTES`].
    Binary {
        /// Declared media type, validated by [`validate_binary_deliverable`].
        media_type: String,
    },
}

/// Request to create an output together with its first revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutput {
    /// Caller-minted identity, so an ambiguous store response can be retried.
    pub id: OutputId,
    /// Conversation that will own the output.
    pub chat_id: ChatId,
    /// Display filename, validated by [`validate_deliverable_name`] for text
    /// and [`validate_binary_deliverable`] for binary artifacts.
    pub filename: String,
    /// Whether the output is model-authored text or a host-accepted binary
    /// artifact. Fixes the media type and the revision size ceiling.
    pub kind: DeliverableKind,
    /// The output's first revision.
    pub revision: NewOutputRevision,
}

/// Validate one portable, renderer-safe deliverable filename.
///
/// Deliverables deliberately use a single ASCII filename rather than an
/// arbitrary scratch-relative path. This keeps the catalog and native export
/// boundary closed and makes names portable across desktop platforms.
pub fn validate_deliverable_name(name: &str) -> Result<(), &'static str> {
    validate_portable_filename(name)?;
    if deliverable_media_type(name).is_none() {
        return Err("filename must end in .md, .txt, .csv, .json, or .html");
    }
    Ok(())
}

/// Validate one portable, renderer-safe filename without constraining its
/// extension.
///
/// This is the shared name contract for both text deliverables and binary
/// workspace artifacts. Text names additionally require a supported extension
/// ([`validate_deliverable_name`]); a binary artifact carries its media type
/// explicitly, so its extension is unconstrained beyond these portability rules.
pub fn validate_portable_filename(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.chars().count() > MAX_DELIVERABLE_NAME_CHARS {
        return Err("filename must contain between 1 and 120 characters");
    }
    if name.trim() != name || name.starts_with('.') || name.ends_with('.') {
        return Err("filename may not start with a dot or surrounding whitespace");
    }
    if !name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
    {
        return Err("filename must start with a letter or number");
    }
    if !name.is_ascii()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || " _-.()".contains(character))
    {
        return Err(
            "filename may contain only letters, numbers, spaces, dashes, underscores, and parentheses",
        );
    }
    Ok(())
}

/// Validate a host-accepted binary artifact's filename and declared media type.
///
/// The filename obeys the portable rules; the media type is a well-formed,
/// bounded `type/subtype` token that is not one of the curated text types (those
/// belong to the model-authored text path, which derives the type from the
/// name).
pub fn validate_binary_deliverable(name: &str, media_type: &str) -> Result<(), &'static str> {
    validate_portable_filename(name)?;
    validate_deliverable_media_type(media_type)?;
    if media_type_is_text(media_type) {
        return Err("binary artifacts must not declare a curated text media type");
    }
    Ok(())
}

/// Validate a declared media type is a bounded, well-formed `type/subtype`
/// token with no parameters.
pub fn validate_deliverable_media_type(media_type: &str) -> Result<(), &'static str> {
    if media_type.is_empty() || media_type.chars().count() > MAX_DELIVERABLE_MEDIA_TYPE_CHARS {
        return Err("media type must contain between 1 and 127 characters");
    }
    let Some((kind, subtype)) = media_type.split_once('/') else {
        return Err("media type must be in type/subtype form");
    };
    if !is_media_type_token(kind) || !is_media_type_token(subtype) {
        return Err("media type contains invalid characters");
    }
    Ok(())
}

fn is_media_type_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "!#$&-^_.+".contains(character))
}

/// Whether a media type is one of the curated text types whose outputs use the
/// text size ceiling.
#[must_use]
pub fn media_type_is_text(media_type: &str) -> bool {
    matches!(
        media_type,
        "text/markdown" | "text/plain" | "text/csv" | "application/json" | "text/html"
    )
}

/// The revision size ceiling that applies to an output with this media type:
/// [`MAX_DELIVERABLE_BYTES`] for curated text, [`MAX_BINARY_DELIVERABLE_BYTES`]
/// otherwise.
#[must_use]
pub fn revision_byte_ceiling(media_type: &str) -> usize {
    if media_type_is_text(media_type) {
        MAX_DELIVERABLE_BYTES
    } else {
        MAX_BINARY_DELIVERABLE_BYTES
    }
}

/// Return the fixed media type for one supported deliverable filename.
#[must_use]
pub fn deliverable_media_type(name: &str) -> Option<&'static str> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "md" => Some("text/markdown"),
        "txt" => Some("text/plain"),
        "csv" => Some("text/csv"),
        "json" => Some("application/json"),
        "html" => Some("text/html"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deliverable_names_are_portable_and_closed() {
        for valid in [
            "brief.md",
            "Q3 summary (final).txt",
            "forecast.csv",
            "data.json",
            "report.html",
        ] {
            assert!(validate_deliverable_name(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            ".hidden.md",
            "trailing.md.",
            " report.md",
            "_report.md",
            "report.md ",
            "../report.md",
            "folder/report.md",
            "report.pdf",
            "bad\u{202e}.md",
        ] {
            assert!(validate_deliverable_name(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn revision_paths_are_derived_only_from_durable_identity() {
        let output_id = OutputId::new();
        let revision_id = OutputRevisionId::new();
        let path = output_revision_relative_path(output_id, revision_id);

        assert_eq!(
            path,
            PathBuf::from(OUTPUTS_DIRECTORY)
                .join(output_id.to_string())
                .join(revision_id.to_string())
        );
        assert!(path.is_relative());
        // A revision path carries no filename, so a display name can never
        // steer where bytes are written or read.
        assert_eq!(
            path,
            output_revision_relative_path(output_id, revision_id),
            "the same identity always resolves to the same path"
        );
        assert_ne!(
            path,
            output_revision_relative_path(output_id, OutputRevisionId::new()),
            "each revision owns a distinct, write-once path"
        );
    }

    #[test]
    fn media_types_are_derived_only_from_supported_extensions() {
        assert_eq!(deliverable_media_type("brief.MD"), Some("text/markdown"));
        assert_eq!(deliverable_media_type("table.csv"), Some("text/csv"));
        assert_eq!(deliverable_media_type("archive.zip"), None);
    }

    #[test]
    fn binary_artifacts_accept_portable_names_and_well_formed_media_types() {
        for (name, media_type) in [
            ("chart.png", "image/png"),
            ("report.pdf", "application/pdf"),
            ("model.bin", "application/octet-stream"),
            (
                "sheet.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
        ] {
            assert!(
                validate_binary_deliverable(name, media_type).is_ok(),
                "{name} {media_type}"
            );
        }
        // A binary artifact may not masquerade as a curated text type.
        assert!(validate_binary_deliverable("notes.png", "text/plain").is_err());
        // Malformed media types are rejected.
        for media_type in [
            "",
            "image",
            "image/",
            "/png",
            "image/png; charset",
            "im age/png",
        ] {
            assert!(
                validate_deliverable_media_type(media_type).is_err(),
                "{media_type}"
            );
        }
        // Traversal and non-portable names are rejected before the media type.
        assert!(validate_binary_deliverable("../chart.png", "image/png").is_err());
    }

    #[test]
    fn revision_ceiling_tracks_the_content_kind() {
        assert_eq!(
            revision_byte_ceiling("text/markdown"),
            MAX_DELIVERABLE_BYTES
        );
        assert_eq!(
            revision_byte_ceiling("application/json"),
            MAX_DELIVERABLE_BYTES
        );
        assert_eq!(
            revision_byte_ceiling("image/png"),
            MAX_BINARY_DELIVERABLE_BYTES
        );
        const {
            assert!(MAX_BINARY_DELIVERABLE_BYTES > MAX_DELIVERABLE_BYTES);
        }
    }
}
