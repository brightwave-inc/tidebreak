//! Closed contract for user-visible files created in conversation-private scratch.
//!
//! An output is a conversation-owned record with an opaque [`OutputId`] and an
//! ordered history of immutable revisions. The record is authoritative: the
//! filename is display metadata, not identity, so re-creating the same
//! filename adds a revision instead of destroying the previous bytes.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::id::{ChatId, OutputId, OutputRevisionId, TurnId};

/// Private-scratch directory holding the output files in use today.
///
/// This is what `create_deliverable` writes and what the desktop Outputs view
/// reads, so it is the only catalog the shipped product has. Identity here is
/// the filename, and rewriting a file destroys the bytes it replaced.
///
/// The durable record layer below is the intended replacement and nothing
/// writes it yet; see [`OUTPUTS_DIRECTORY`].
pub const DELIVERABLES_DIRECTORY: &str = "artifacts";
/// Private-scratch directory holding immutable revision bytes.
///
/// Paired with the `output` and `output_revision` tables. The store layer is
/// complete and has no callers outside this crate: the cutover from
/// [`DELIVERABLES_DIRECTORY`] is a separate slice. Anything reasoning about
/// what a user can actually see today should look there, not here.
pub const OUTPUTS_DIRECTORY: &str = "outputs";
/// Largest text artifact the foreground agent may create.
pub const MAX_DELIVERABLE_BYTES: usize = 512 * 1024;
/// Largest filename accepted by the model-facing and native boundaries.
pub const MAX_DELIVERABLE_NAME_CHARS: usize = 120;
/// Largest number of revisions one output retains.
///
/// Reaching the bound is a product decision rather than a storage failure: the
/// store refuses a further revision so no caller can silently lose history.
pub const MAX_OUTPUT_REVISIONS: u32 = 100;

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
    /// Fixed media type derived from `filename` when the output was created.
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
    /// Turn that produced the revision, when it came from an agent turn.
    pub turn_id: Option<TurnId>,
    /// Host-stamped creation time.
    pub created_at: DateTime<Utc>,
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
    /// Turn that produced the revision, when it came from an agent turn.
    pub turn_id: Option<TurnId>,
    /// Host-stamped creation time.
    pub created_at: DateTime<Utc>,
}

/// Request to create an output together with its first revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutput {
    /// Caller-minted identity, so an ambiguous store response can be retried.
    pub id: OutputId,
    /// Conversation that will own the output.
    pub chat_id: ChatId,
    /// Display filename, validated by [`validate_deliverable_name`].
    pub filename: String,
    /// The output's first revision.
    pub revision: NewOutputRevision,
}

/// Validate one portable, renderer-safe deliverable filename.
///
/// Deliverables deliberately use a single ASCII filename rather than an
/// arbitrary scratch-relative path. This keeps the catalog and native export
/// boundary closed and makes names portable across desktop platforms.
pub fn validate_deliverable_name(name: &str) -> Result<(), &'static str> {
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
    if deliverable_media_type(name).is_none() {
        return Err("filename must end in .md, .txt, .csv, .json, or .html");
    }
    Ok(())
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
}
