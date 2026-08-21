//! Fork: the parent session's transcript, written into the worktree as
//! markdown so a sibling agent of any engine can read it.
//!
//! Pure serialization of what the journal already holds. No model call and no
//! summary — a generated one would be a second thing to be wrong, and the
//! point of the file is that the child sees what the parent saw.
//!
//! The file lands in `.tidebreak/forks/` beside a `.gitignore` holding `*`,
//! which is what keeps it out of `git status` without touching the repo's
//! tracked ignore file or the shared `.git/info/exclude` that every other
//! worktree of the same repository reads.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tidebreak_core::{
    CodeEvent, CodeSession, CodeTurn, CodeTurnId, HarnessKind, SequencedCodeEvent, ToolDetail,
    ToolOutcome,
};

/// Directory holding fork transcripts, relative to the worktree root.
pub(crate) const FORKS_DIR: &str = ".tidebreak/forks";

/// Largest transcript written, in bytes. Bounds the whole file, header
/// included.
///
/// A long session can hold megabytes of assistant text. The child needs
/// recent context far more than it needs the first turn, so the oldest turns
/// are dropped to fit and the header says so. One turn can be over the cap by
/// itself, so that one is cut rather than written past it.
const MAX_TRANSCRIPT_BYTES: usize = 512 * 1024;

/// A written fork transcript, as the route reports it.
pub(crate) struct WrittenTranscript {
    /// Worktree-relative path, in the form the composer shows and the engine
    /// opens.
    pub(crate) path: String,
    /// Bytes on disk.
    pub(crate) byte_len: u64,
    /// Turns the file actually contains.
    pub(crate) turns: u32,
    /// True when anything was left out to fit [`MAX_TRANSCRIPT_BYTES`]:
    /// older turns, or the end of a turn too large on its own.
    pub(crate) truncated: bool,
}

/// Render one session's transcript and write it under the worktree.
///
/// A session can be forked again over a file the last child is already
/// reading, so the write is published by rename: a reader sees one whole
/// version or another and never the middle of one. Each write stages its own
/// sibling file, so two forks racing on one session cannot mix their bytes —
/// the later rename simply wins.
pub(crate) async fn write_transcript(
    worktree: &Path,
    session: &CodeSession,
    turns: &[CodeTurn],
    events: &[SequencedCodeEvent],
) -> std::io::Result<WrittenTranscript> {
    let rendered = render_transcript(session, turns, events);
    let dir = worktree.join(FORKS_DIR);
    tokio::fs::create_dir_all(&dir).await?;
    ignore_scratch_dir(worktree).await?;
    let path = dir.join(format!("{}.md", session.id));
    publish(&path, rendered.markdown.as_bytes()).await?;
    Ok(WrittenTranscript {
        path: format!("{FORKS_DIR}/{}.md", session.id),
        byte_len: rendered.markdown.len() as u64,
        turns: rendered.turns,
        truncated: rendered.truncated,
    })
}

/// Put bytes at `path` in one step, leaving nothing behind if it fails.
///
/// The staged name carries a fresh id rather than a fixed `.part` suffix, so
/// concurrent writers stage separately and neither can be caught writing over
/// the other's half-written file.
async fn publish(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("the transcript path names no file"))?;
    let staged = path.with_file_name(format!(
        "{}.{}.part",
        name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let written = tokio::fs::write(&staged, bytes).await;
    let published = match written {
        Ok(()) => tokio::fs::rename(&staged, path).await,
        Err(err) => Err(err),
    };
    if published.is_err() {
        let _ = tokio::fs::remove_file(&staged).await;
    }
    published
}

/// Make `.tidebreak/` ignore itself.
///
/// A `.gitignore` holding `*` hides the directory and the ignore file with
/// it. Writing here mutates nothing the reader tracks and nothing shared with
/// their other worktrees, which the alternatives — the repository's own
/// `.gitignore`, or `.git/info/exclude` — both do.
async fn ignore_scratch_dir(worktree: &Path) -> std::io::Result<()> {
    let scratch: PathBuf = worktree.join(".tidebreak");
    let marker = scratch.join(".gitignore");
    if tokio::fs::try_exists(&marker).await? {
        return Ok(());
    }
    tokio::fs::write(&marker, "*\n").await
}

/// A rendered transcript and what had to be left out of it.
struct RenderedTranscript {
    markdown: String,
    turns: u32,
    truncated: bool,
}

/// Serialize a session as markdown, newest turns kept when it will not fit.
///
/// The result is never larger than [`MAX_TRANSCRIPT_BYTES`]. Turns are kept
/// from the newest back; the header counts against the same budget, and a
/// turn that is over the budget by itself is cut instead of overrunning it.
fn render_transcript(
    session: &CodeSession,
    turns: &[CodeTurn],
    events: &[SequencedCodeEvent],
) -> RenderedTranscript {
    let engine = harness_label(session.harness_kind);
    let mut sections: Vec<String> = Vec::with_capacity(turns.len());
    for turn in turns {
        sections.push(render_turn(turn, events, engine));
    }

    // The header is part of the file, so it is part of the budget. Its length
    // depends on how many turns are dropped, which is what the budget decides
    // — so reserve the longest it can be, the one that drops every turn.
    let reserved = header(session, turns.len(), turns.len(), engine).len();
    let budget = MAX_TRANSCRIPT_BYTES.saturating_sub(reserved);

    // Budget from the end: the last turn is the one the child most needs.
    // Each section costs the blank line that separates it, too.
    let mut kept = 0usize;
    let mut used = 0usize;
    let mut clipped = false;
    for section in sections.iter_mut().rev() {
        if used + section.len() + 1 > budget {
            if kept > 0 {
                break;
            }
            // One turn is over the cap on its own. The child gets the start
            // of it rather than a file that ignores the bound.
            clip(section, budget.saturating_sub(1));
            clipped = true;
        }
        used += section.len() + 1;
        kept += 1;
    }
    let dropped = sections.len() - kept;

    let mut out = String::with_capacity(reserved + used);
    out.push_str(&header(session, turns.len(), dropped, engine));
    for section in &sections[dropped..] {
        out.push('\n');
        out.push_str(section);
    }

    RenderedTranscript {
        markdown: out,
        turns: kept as u32,
        truncated: dropped > 0 || clipped,
    }
}

/// The file's opening: where the transcript came from, and what is missing.
fn header(session: &CodeSession, total: usize, dropped: usize, engine: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Transcript of a {engine} session");
    out.push('\n');
    let _ = writeln!(
        out,
        "Recorded from the session started {}. {} turn{}{}.",
        session.created_at.to_rfc3339(),
        total,
        if total == 1 { "" } else { "s" },
        if dropped > 0 {
            format!(", of which the {dropped} oldest are not included here")
        } else {
            String::new()
        }
    );
    out.push('\n');
    out.push_str(
        "Messages and tool calls are as the engine reported them. Work a \
         subagent did inside a `Task` call is summarized by that call rather \
         than transcribed.\n",
    );
    out
}

/// Cut a section to `budget` bytes, on a character boundary, and say so.
///
/// The head is what survives: it holds the heading and the ask, without which
/// the section stops being readable markdown at all. Cutting from the front
/// would save the engine's last words and lose the question they answer.
fn clip(section: &mut String, budget: usize) {
    const CUT: &str = "\n_(the rest of this turn was too large to include)_\n";
    if section.len() <= budget {
        return;
    }
    let mut end = budget.saturating_sub(CUT.len()).min(section.len());
    while end > 0 && !section.is_char_boundary(end) {
        end -= 1;
    }
    section.truncate(end);
    if budget > CUT.len() {
        section.push_str(CUT);
    }
}

/// One turn: what the reader asked, then what the engine said and did.
fn render_turn(turn: &CodeTurn, events: &[SequencedCodeEvent], engine: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Turn {} — you\n", turn.ordinal);
    let asked = turn.user_input.trim();
    let _ = writeln!(
        out,
        "{}\n",
        if asked.is_empty() { "_(empty)_" } else { asked }
    );

    let lines = turn_lines(turn.id, events);
    if lines.is_empty() {
        return out;
    }
    let _ = writeln!(out, "## Turn {} — {engine}\n", turn.ordinal);
    for line in lines {
        let _ = writeln!(out, "{line}\n");
    }
    out
}

/// The engine's own messages and tool calls for one turn, in journal order.
///
/// Scoped by walking from this turn's `TurnStarted` to the next one: a turn
/// id appears on the boundary events, not on every event inside it.
fn turn_lines(turn_id: CodeTurnId, events: &[SequencedCodeEvent]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut inside = false;
    // A tool call opens and closes on separate events, and the closing one
    // carries the outcome plus a detail that may say more than the opening
    // one did. Hold the call's name, detail, and line index until then.
    let mut open: Vec<(String, usize, String, ToolDetail)> = Vec::new();

    for entry in events {
        match &entry.event {
            CodeEvent::TurnStarted { turn_id: started } => {
                if inside {
                    break;
                }
                inside = *started == turn_id;
            }
            _ if !inside => {}
            CodeEvent::AssistantMessage {
                text,
                parent_call_id,
            } => {
                if parent_call_id.is_none() && !text.trim().is_empty() {
                    lines.push(text.trim().to_owned());
                }
            }
            CodeEvent::UserSteered { text } => {
                if !text.trim().is_empty() {
                    lines.push(format!("**You, mid-turn:** {}", text.trim()));
                }
            }
            CodeEvent::ToolStarted {
                call_id,
                name,
                detail,
                parent_call_id,
            } => {
                if parent_call_id.is_some() {
                    continue;
                }
                open.push((call_id.clone(), lines.len(), name.clone(), detail.clone()));
                lines.push(tool_line(name, detail, None));
            }
            CodeEvent::ToolCompleted {
                call_id,
                outcome,
                detail,
                parent_call_id,
                ..
            } => {
                if parent_call_id.is_some() {
                    continue;
                }
                let Some(found) = open.iter().position(|(id, ..)| id == call_id) else {
                    continue;
                };
                let (_, at, name, started) = open.remove(found);
                let Some(line) = lines.get_mut(at) else {
                    continue;
                };
                *line = format!(
                    "{}{}",
                    tool_line(&name, &started, detail.as_ref()),
                    match outcome {
                        ToolOutcome::Succeeded => "",
                        ToolOutcome::Failed => " (failed)",
                        ToolOutcome::Denied => " (denied)",
                    }
                );
            }
            CodeEvent::TurnFailed { error } => {
                lines.push(format!("**The turn failed:** {}", error.message.trim()));
            }
            CodeEvent::TurnInterrupted => lines.push("**The turn was interrupted.**".to_owned()),
            _ => {}
        }
    }
    lines
}

/// A tool call as one line: what it was, and what it was pointed at.
///
/// An engine can open a call before its arguments finish streaming, so the
/// detail on the completion replaces the opening one when it scores higher —
/// [`ToolDetail::specificity`]'s rule, so a correction never downgrades a
/// line that already names its subject.
fn tool_line(name: &str, started: &ToolDetail, completed: Option<&ToolDetail>) -> String {
    let chosen = match completed {
        Some(later) if later.specificity() > started.specificity() => later,
        _ => started,
    };
    let subject = chosen.subject().trim();
    if subject.is_empty() {
        format!("- `{name}`")
    } else {
        format!("- `{name}` — {}", one_line(subject))
    }
}

/// Collapse a subject onto one line so a bullet stays a bullet.
fn one_line(value: &str) -> String {
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= 160 {
        return flattened;
    }
    let cut: String = flattened.chars().take(159).collect();
    format!("{cut}…")
}

/// The engine's name as a person writes it.
fn harness_label(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::ClaudeCode => "Claude Code",
        HarnessKind::Codex => "Codex CLI",
        HarnessKind::Opencode => "opencode",
        HarnessKind::Grok => "Grok CLI",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{
        Attention, AttentionSource, BoundedError, CodePermissionMode, CodeSessionId,
        CodeSessionKind, CodeSessionLifecycle, CodeTurnStatus, OwnerId, WorkspaceId,
    };

    fn session() -> CodeSession {
        CodeSession {
            id: CodeSessionId::new(),
            owner: OwnerId::local(),
            workspace_id: WorkspaceId::new(),
            kind: CodeSessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: CodePermissionMode::Plan,
            model: None,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: chrono::Utc::now(),
        }
    }

    fn turn(session_id: CodeSessionId, ordinal: i64, asked: &str) -> CodeTurn {
        CodeTurn {
            id: CodeTurnId::new(),
            session_id,
            ordinal,
            status: CodeTurnStatus::Completed,
            user_input: asked.to_owned(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            started_at: chrono::Utc::now(),
            ended_at: None,
        }
    }

    fn seq(events: Vec<CodeEvent>) -> Vec<SequencedCodeEvent> {
        events
            .into_iter()
            .enumerate()
            .map(|(at, event)| SequencedCodeEvent {
                seq: at as i64 + 1,
                event,
            })
            .collect()
    }

    /// The contract the child reads: its own turns, in order, and nobody
    /// else's. Scoping by `TurnStarted` boundaries is the only thing keeping
    /// turn two's work out of turn one's section.
    #[test]
    fn renders_each_turn_with_only_its_own_events() {
        let session = session();
        let first = turn(session.id, 1, "fix the failing auth test");
        let second = turn(session.id, 2, "now push it");
        let events = seq(vec![
            CodeEvent::TurnStarted { turn_id: first.id },
            CodeEvent::AssistantMessage {
                text: "Looking at the test now.".to_owned(),
                parent_call_id: None,
            },
            CodeEvent::ToolStarted {
                call_id: "call-1".to_owned(),
                name: "Bash".to_owned(),
                detail: ToolDetail::Other {
                    summary: String::new(),
                },
                parent_call_id: None,
            },
            CodeEvent::ToolCompleted {
                call_id: "call-1".to_owned(),
                outcome: ToolOutcome::Failed,
                preview: "1 failed".to_owned(),
                detail: Some(ToolDetail::Command {
                    cmd: "cargo test -p auth".to_owned(),
                    cwd: String::new(),
                }),
                parent_call_id: None,
            },
            CodeEvent::TurnStarted { turn_id: second.id },
            CodeEvent::AssistantMessage {
                text: "Pushed.".to_owned(),
                parent_call_id: None,
            },
        ]);

        let rendered = render_transcript(&session, &[first, second], &events);
        let markdown = rendered.markdown;

        assert_eq!(rendered.turns, 2);
        assert!(!rendered.truncated);
        assert!(markdown.contains("# Transcript of a Claude Code session"));
        assert!(markdown.contains("fix the failing auth test"));
        assert!(markdown.contains("Looking at the test now."));
        // The completion's detail names the command the start could not.
        assert!(markdown.contains("- `Bash` — cargo test -p auth (failed)"));
        // Turn two's message must not have leaked into turn one's section.
        let turn_two = markdown.find("## Turn 2 — you").expect("turn two heading");
        assert!(markdown.find("Pushed.").expect("second message") > turn_two);
    }

    /// Subagent chatter belongs to the subagent. The parent's `Task` call is
    /// what the child needs to see, not the thousand lines inside it.
    #[test]
    fn leaves_subagent_events_to_the_task_call_that_ran_them() {
        let session = session();
        let only = turn(session.id, 1, "audit the crate");
        let events = seq(vec![
            CodeEvent::TurnStarted { turn_id: only.id },
            CodeEvent::ToolStarted {
                call_id: "task-1".to_owned(),
                name: "Task".to_owned(),
                detail: ToolDetail::Other {
                    summary: "audit".to_owned(),
                },
                parent_call_id: None,
            },
            CodeEvent::AssistantMessage {
                text: "subagent thinking out loud".to_owned(),
                parent_call_id: Some("task-1".to_owned()),
            },
            CodeEvent::ToolCompleted {
                call_id: "task-1".to_owned(),
                outcome: ToolOutcome::Succeeded,
                preview: "done".to_owned(),
                detail: None,
                parent_call_id: None,
            },
        ]);

        let markdown = render_transcript(&session, &[only], &events).markdown;
        assert!(markdown.contains("- `Task` — audit"));
        assert!(!markdown.contains("subagent thinking out loud"));
    }

    /// A failed turn is the most useful thing a fork can know, so it survives
    /// into the file rather than reading as silence.
    #[test]
    fn keeps_a_failure_visible() {
        let session = session();
        let only = turn(session.id, 1, "deploy it");
        let events = seq(vec![
            CodeEvent::TurnStarted { turn_id: only.id },
            CodeEvent::TurnFailed {
                error: BoundedError {
                    message: "the engine exited".to_owned(),
                },
            },
        ]);

        let markdown = render_transcript(&session, &[only], &events).markdown;
        assert!(markdown.contains("**The turn failed:** the engine exited"));
    }

    /// Over budget, the oldest turns go and the header says how many. The
    /// child needs the end of the conversation, not the start of it.
    #[test]
    fn drops_the_oldest_turns_to_fit_and_says_so() {
        let session = session();
        let bulk = "x".repeat(200 * 1024);
        let turns: Vec<CodeTurn> = (1..=5)
            .map(|ordinal| turn(session.id, ordinal, &bulk))
            .collect();

        let rendered = render_transcript(&session, &turns, &[]);
        assert!(rendered.truncated);
        assert_eq!(rendered.turns, 2);
        assert!(rendered.markdown.contains("the 3 oldest are not included"));
        assert!(rendered.markdown.contains("## Turn 5 — you"));
        assert!(!rendered.markdown.contains("## Turn 1 — you"));
    }

    /// The cap bounds the file, not the turns inside it, so the header has to
    /// come out of the same budget. Two turns that together sit just under it
    /// leave no room for the paragraph saying where the transcript came from.
    #[test]
    fn counts_the_header_against_the_cap() {
        let session = session();
        let bulk = "x".repeat(MAX_TRANSCRIPT_BYTES / 2 - 40);
        let turns: Vec<CodeTurn> = (1..=2)
            .map(|ordinal| turn(session.id, ordinal, &bulk))
            .collect();

        let rendered = render_transcript(&session, &turns, &[]);
        assert!(
            rendered.markdown.len() <= MAX_TRANSCRIPT_BYTES,
            "{} bytes is over the cap",
            rendered.markdown.len()
        );
        assert_eq!(rendered.turns, 1);
        assert!(rendered.truncated);
    }

    /// One turn can be larger than the whole budget on its own — a single ask
    /// with a pasted log in it. Dropping turns cannot help there, so the turn
    /// itself is cut, and the file says where.
    #[test]
    fn cuts_a_turn_that_is_over_the_cap_by_itself() {
        let session = session();
        let bulk = "x".repeat(MAX_TRANSCRIPT_BYTES * 2);
        let only = turn(session.id, 1, &bulk);

        let rendered = render_transcript(&session, &[only], &[]);
        assert!(
            rendered.markdown.len() <= MAX_TRANSCRIPT_BYTES,
            "{} bytes is over the cap",
            rendered.markdown.len()
        );
        assert_eq!(rendered.turns, 1);
        assert!(rendered.truncated);
        assert!(rendered.markdown.contains("## Turn 1 — you"));
        assert!(rendered
            .markdown
            .contains("the rest of this turn was too large to include"));
    }

    /// Multi-byte text must not be cut through a character. A file the child
    /// engine cannot decode is worse than one that stops early.
    #[test]
    fn cuts_on_a_character_boundary() {
        let session = session();
        let only = turn(session.id, 1, &"日".repeat(MAX_TRANSCRIPT_BYTES));

        let rendered = render_transcript(&session, &[only], &[]);
        assert!(rendered.markdown.len() <= MAX_TRANSCRIPT_BYTES);
        assert!(rendered.truncated);
        // Round-tripping through bytes proves nothing was cut mid-character:
        // `String` would not hold it otherwise.
        assert_eq!(
            String::from_utf8(rendered.markdown.clone().into_bytes()).expect("valid utf-8"),
            rendered.markdown
        );
    }

    /// A session can be forked again while the last child still has the file
    /// open. Every read has to land on a whole document, which is what the
    /// stage-then-rename is for — a plain write truncates in place first.
    #[tokio::test]
    async fn a_reader_never_catches_a_re_fork_mid_write() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let worktree = tempfile::tempdir().expect("tempdir");
        let session = session();
        let short = vec![turn(session.id, 1, "hello")];
        let long: Vec<CodeTurn> = (1..=20)
            .map(|ordinal| turn(session.id, ordinal, &"x".repeat(8 * 1024)))
            .collect();
        write_transcript(worktree.path(), &session, &short, &[])
            .await
            .expect("first write");
        let path = worktree
            .path()
            .join(format!("{FORKS_DIR}/{}.md", session.id));

        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let path = path.clone();
            let done = Arc::clone(&done);
            tokio::task::spawn_blocking(move || {
                let mut reads = 0usize;
                while !done.load(Ordering::Relaxed) {
                    let seen = std::fs::read_to_string(&path).expect("the file is always there");
                    assert!(
                        seen.starts_with("# Transcript of a Claude Code session"),
                        "a reader caught {} bytes mid-write",
                        seen.len()
                    );
                    reads += 1;
                }
                reads
            })
        };

        for _ in 0..20 {
            for turns in [&long, &short] {
                write_transcript(worktree.path(), &session, turns, &[])
                    .await
                    .expect("re-fork");
            }
        }
        done.store(true, Ordering::Relaxed);
        assert!(reader.await.expect("reader") > 0, "the reader never ran");

        // Nothing staged is left behind for the child engine to trip over.
        let left: Vec<String> = std::fs::read_dir(worktree.path().join(FORKS_DIR))
            .expect("forks dir")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
            .collect();
        assert_eq!(left, vec![format!("{}.md", session.id)]);
    }

    /// The file has to be readable by the child engine and invisible to
    /// `git status`, which is the whole reason for the `*` marker beside it.
    #[tokio::test]
    async fn writes_the_file_into_a_self_ignoring_directory() {
        let worktree = tempfile::tempdir().expect("tempdir");
        let session = session();
        let only = turn(session.id, 1, "hello");

        let written = write_transcript(worktree.path(), &session, &[only], &[])
            .await
            .expect("write");

        assert_eq!(written.path, format!("{FORKS_DIR}/{}.md", session.id));
        assert_eq!(written.turns, 1);
        assert!(!written.truncated);
        let on_disk = std::fs::read_to_string(worktree.path().join(&written.path)).expect("read");
        assert_eq!(written.byte_len, on_disk.len() as u64);
        assert!(on_disk.contains("hello"));
        assert_eq!(
            std::fs::read_to_string(worktree.path().join(".tidebreak/.gitignore")).expect("marker"),
            "*\n"
        );
    }
}
