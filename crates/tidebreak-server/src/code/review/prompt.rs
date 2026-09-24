//! What the reviewing engine is asked.
//!
//! The rules, the scope, and the answer format are Tidebreak's and fixed,
//! because the read-only posture and the parser depend on them. Only the
//! focus, what the reviewer looks for, is the person's to change.

/// What the reviewer looks for when the person has not changed it. The
/// desktop's `review_changes` workflow prompt ships the same words.
pub const DEFAULT_REVIEW_FOCUS: &str = "Find real problems in these changes: bugs, wrong logic, unhandled errors and edge cases, security issues, and code that does not do what it claims. Skip style a formatter or linter would catch, and do not praise the code.";

/// Longest focus a person may set, in characters.
pub const MAX_FOCUS_CHARS: usize = 4_000;

/// Which changes a review covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewScope<'a> {
    /// The working tree, committed and not, against its base branch.
    WorkingTree { base: &'a str },
    /// The changes one turn of the conversation made.
    Turn { ordinal: Option<i64> },
}

impl ReviewScope<'_> {
    fn sentence(self) -> String {
        match self {
            Self::WorkingTree { base } => format!(
                "every change in this branch's working tree, committed or not, against the base branch `{base}`."
            ),
            Self::Turn {
                ordinal: Some(ordinal),
            } => format!("the changes turn {ordinal} of the author's conversation made."),
            Self::Turn { ordinal: None } => {
                "the changes one turn of the author's conversation made.".to_owned()
            }
        }
    }
}

/// The whole message the reviewer's one turn receives.
#[must_use]
pub fn review_prompt(
    scope: ReviewScope<'_>,
    diff: &str,
    truncated: bool,
    focus: Option<&str>,
) -> String {
    let focus = focus
        .map(str::trim)
        .filter(|focus| !focus.is_empty())
        .unwrap_or(DEFAULT_REVIEW_FOCUS);
    let cut = if truncated {
        " The diff below is cut short; run `git diff HEAD` to read the rest."
    } else {
        ""
    };
    let scope = scope.sentence();
    format!(
        "Review the changes described below. You are a reviewer, not the author: read and report, and change nothing.\n\
         \n\
         Rules:\n\
         - Do not create, edit, or delete files, run commands that change anything, commit, or push. Tidebreak refuses every such request, and you are working in a copy of the files that is deleted when you finish.\n\
         - You may read any file in the current directory and run read-only commands, such as `git diff HEAD`.\n\
         \n\
         What you are reviewing: {scope}\n\
         The current directory holds the files after the changes. In git, HEAD is the state before them, so `git diff HEAD` shows the same changes as the diff below.{cut}\n\
         \n\
         What to look for: {focus}\n\
         \n\
         Comment only on lines the diff adds or changes, or on the unchanged lines it shows around them. Number lines as the new version of the file numbers them.\n\
         \n\
         When you are done, reply with only a JSON object in a ```json block, in this shape:\n\
         {{\"summary\": \"One or two sentences on the changes overall.\", \"findings\": [{{\"file\": \"src/example.ts\", \"start_line\": 12, \"end_line\": 14, \"severity\": \"high\", \"title\": \"A short title\", \"explanation\": \"What is wrong, and what to do about it.\"}}]}}\n\
         - file: the path from the repository root, without the a/ or b/ a diff header adds.\n\
         - start_line and end_line: whole numbers; the same number for one line.\n\
         - severity: \"high\" for a bug or a security problem that must be fixed, \"medium\" for a problem worth fixing, \"low\" for a minor improvement.\n\
         - Return an empty findings list when nothing needs to change.\n\
         - Write the JSON in your reply. Do not deliver it through a tool, a file, or a plan.\n\
         \n\
         The diff:\n\
         <diff>\n\
         {diff}\n\
         </diff>\n"
    )
}
