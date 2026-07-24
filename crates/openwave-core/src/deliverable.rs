//! Closed contract for user-visible files created in conversation-private scratch.

/// Private-scratch directory reserved for user-visible outputs.
pub const DELIVERABLES_DIRECTORY: &str = "artifacts";
/// Largest text artifact the foreground agent may create.
pub const MAX_DELIVERABLE_BYTES: usize = 512 * 1024;
/// Largest filename accepted by the model-facing and native boundaries.
pub const MAX_DELIVERABLE_NAME_CHARS: usize = 120;

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
    fn media_types_are_derived_only_from_supported_extensions() {
        assert_eq!(deliverable_media_type("brief.MD"), Some("text/markdown"));
        assert_eq!(deliverable_media_type("table.csv"), Some("text/csv"));
        assert_eq!(deliverable_media_type("archive.zip"), None);
    }
}
