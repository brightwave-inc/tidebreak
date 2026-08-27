//! The two files a sandboxed run communicates through.
//!
//! A supervised run has no return channel except its event stream, so two
//! well-known files carry the agent's own signals outward. The completion
//! latch at `.model-gateway/task-complete` under the workspace lets a bounded
//! task declare itself done mid-run; `TASK_OUTPUT.md` carries the deliverable
//! for a run whose work cannot leave as pushed branches. Both readers match
//! the supervising environment's first-party ones byte for byte, because the
//! environment's tooling consumes the resulting events.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Latch path relative to the workspace.
pub const COMPLETION_LATCH_PATH: &str = ".model-gateway/task-complete";
/// Ceiling for the note carried inside the latch file.
pub const COMPLETION_NOTE_MAX_BYTES: u64 = 4096;
/// Deliverable filename, probed in the working directory then the workspace.
pub const TASK_OUTPUT_NAME: &str = "TASK_OUTPUT.md";
/// Ceiling for the deliverable body one event may carry.
pub const TASK_OUTPUT_MAX_BODY_BYTES: u64 = 224 * 1024;

/// A written latch: the task declared itself complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionLatch {
    /// The note inside the file, when it held usable text.
    pub note: Option<String>,
    /// Why the note was dropped, when the file held something unusable.
    pub note_skipped: Option<&'static str>,
}

/// Where the latch lives for a given workspace.
#[must_use]
pub fn completion_latch_path(workspace: &Path) -> PathBuf {
    workspace.join(COMPLETION_LATCH_PATH)
}

/// Reads the completion latch, when one was written.
///
/// The file's existence is the signal; its content is only a best-effort
/// note. A note that is too large, not text, or unreadable is dropped with a
/// named reason rather than blocking completion — the task already finished,
/// and refusing to notice would strand a done run.
#[must_use]
pub fn read_completion_latch(workspace: &Path) -> Option<CompletionLatch> {
    let path = completion_latch_path(workspace);
    // A directory at the latch path is not a latch.
    if !path.is_file() {
        return None;
    }
    let skipped = |reason| {
        Some(CompletionLatch {
            note: None,
            note_skipped: Some(reason),
        })
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return skipped("unreadable");
    };
    if metadata.len() > COMPLETION_NOTE_MAX_BYTES {
        return skipped("too_large");
    }
    match std::fs::read_to_string(&path) {
        Ok(note) if note.contains('\0') => skipped("not_text"),
        Ok(note) => {
            let note = note.trim();
            Some(CompletionLatch {
                note: (!note.is_empty()).then(|| note.to_owned()),
                note_skipped: None,
            })
        }
        Err(_) => skipped("unreadable"),
    }
}

/// Builds the `task_complete` event payload for a read latch.
#[must_use]
pub fn task_complete_payload(latch: &CompletionLatch) -> serde_json::Value {
    let mut payload = serde_json::json!({ "path": COMPLETION_LATCH_PATH });
    if let Some(note) = &latch.note {
        payload["bytes"] = serde_json::json!(note.len());
        payload["note"] = serde_json::json!(note);
    }
    if let Some(reason) = latch.note_skipped {
        payload["note_skipped"] = serde_json::json!(reason);
    }
    payload
}

/// A read deliverable, bounded to what one event may carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskOutput {
    /// The body, cut back to the ceiling when the file exceeds it.
    pub body: String,
    /// Whether the file was larger than the body carried.
    pub truncated: bool,
    /// The file's full size on disk.
    pub file_bytes: u64,
}

/// Reads the deliverable from `directory`, when one was written.
///
/// `Ok(None)` means no file; `Err` names why an existing file could not be
/// delivered. An oversized file is truncated at the ceiling and marked, never
/// dropped — and when the cut lands mid-character, the body is trimmed back
/// to the last complete one rather than refused.
pub fn read_task_output(directory: &Path) -> Result<Option<TaskOutput>, &'static str> {
    let path = directory.join(TASK_OUTPUT_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(&path).map_err(|_| "unreadable")?;
    let file_bytes = metadata.len();
    let truncated = file_bytes > TASK_OUTPUT_MAX_BODY_BYTES;
    let file = std::fs::File::open(&path).map_err(|_| "unreadable")?;
    let mut body = Vec::new();
    file.take(TASK_OUTPUT_MAX_BODY_BYTES)
        .read_to_end(&mut body)
        .map_err(|_| "unreadable")?;
    let body = match String::from_utf8(body) {
        Ok(body) => body,
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            // The ceiling cut a multi-byte character; keep what is whole.
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).map_err(|_| "not_text")?
        }
        Err(_) => return Err("not_text"),
    };
    if body.contains('\0') {
        return Err("not_text");
    }
    Ok(Some(TaskOutput {
        body,
        truncated,
        file_bytes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_latch_reads_as_none() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(read_completion_latch(root.path()), None);
    }

    #[test]
    fn a_directory_at_the_latch_path_is_not_a_latch() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(completion_latch_path(root.path())).unwrap();
        assert_eq!(read_completion_latch(root.path()), None);
    }

    #[test]
    fn a_blank_latch_completes_without_a_note() {
        let root = tempfile::tempdir().unwrap();
        let path = completion_latch_path(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "  \n").unwrap();
        let latch = read_completion_latch(root.path()).unwrap();
        assert_eq!(latch.note, None);
        assert_eq!(latch.note_skipped, None);
        assert_eq!(
            task_complete_payload(&latch),
            serde_json::json!({ "path": COMPLETION_LATCH_PATH })
        );
    }

    #[test]
    fn a_note_rides_the_completion_payload() {
        let root = tempfile::tempdir().unwrap();
        let path = completion_latch_path(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "shipped as PR 7\n").unwrap();
        let latch = read_completion_latch(root.path()).unwrap();
        assert_eq!(latch.note.as_deref(), Some("shipped as PR 7"));
        let payload = task_complete_payload(&latch);
        assert_eq!(payload["note"], "shipped as PR 7");
        assert_eq!(payload["bytes"], 15);
    }

    /// An unusable note must not block completion: the latch still fires,
    /// with the reason named.
    #[test]
    fn an_oversized_note_is_dropped_but_still_completes() {
        let root = tempfile::tempdir().unwrap();
        let path = completion_latch_path(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x".repeat(COMPLETION_NOTE_MAX_BYTES as usize + 1)).unwrap();
        let latch = read_completion_latch(root.path()).unwrap();
        assert_eq!(latch.note, None);
        assert_eq!(latch.note_skipped, Some("too_large"));
        assert_eq!(task_complete_payload(&latch)["note_skipped"], "too_large");
    }

    #[test]
    fn a_binary_note_is_dropped_as_not_text() {
        let root = tempfile::tempdir().unwrap();
        let path = completion_latch_path(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"done\0").unwrap();
        let latch = read_completion_latch(root.path()).unwrap();
        assert_eq!(latch.note_skipped, Some("not_text"));
    }

    #[test]
    fn an_absent_deliverable_reads_as_none() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(read_task_output(root.path()), Ok(None));
    }

    #[test]
    fn a_small_deliverable_arrives_whole() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(TASK_OUTPUT_NAME), "# Findings\n").unwrap();
        let output = read_task_output(root.path()).unwrap().unwrap();
        assert_eq!(output.body, "# Findings\n");
        assert!(!output.truncated);
        assert_eq!(output.file_bytes, 11);
    }

    /// The ceiling can land mid-character; the body must be cut back to the
    /// last complete one instead of refusing the whole deliverable.
    #[test]
    fn truncation_cuts_back_to_a_character_boundary() {
        let root = tempfile::tempdir().unwrap();
        let cap = TASK_OUTPUT_MAX_BODY_BYTES as usize;
        // Fill to one byte short of the ceiling, then a 3-byte character
        // that straddles it.
        let mut body = "x".repeat(cap - 1);
        body.push('€');
        std::fs::write(root.path().join(TASK_OUTPUT_NAME), &body).unwrap();
        let output = read_task_output(root.path()).unwrap().unwrap();
        assert_eq!(output.body.len(), cap - 1);
        assert!(output.truncated);
        assert_eq!(output.file_bytes, body.len() as u64);
    }

    #[test]
    fn a_binary_deliverable_is_refused_as_not_text() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(TASK_OUTPUT_NAME), b"report\0body").unwrap();
        assert_eq!(read_task_output(root.path()), Err("not_text"));
    }
}
