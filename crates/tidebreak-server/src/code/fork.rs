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

/// Largest transcript written, in bytes.
///
/// A long session can hold megabytes of assistant text. The child needs
/// recent context far more than it needs the first turn, so the oldest turns
/// are dropped to fit and the header says so.
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
    /// True when older turns were dropped to fit [`MAX_TRANSCRIPT_BYTES`].
    pub(crate) truncated: bool,
}

/// Render one session's transcript and write it under the worktree.
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
    tokio::fs::write(&path, rendered.markdown.as_bytes()).await?;
    Ok(WrittenTranscript {
        path: format!("{FORKS_DIR}/{}.md", session.id),
        byte_len: rendered.markdown.len() as u64,
        turns: rendered.turns,
        truncated: rendered.truncated,
    })
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

    // Budget from the end: the last turn is the one the child most needs.
    let mut kept = 0usize;
    let mut used = 0usize;
    for section in sections.iter().rev() {
        if used + section.len() > MAX_TRANSCRIPT_BYTES && kept > 0 {
            break;
        }
        used += section.len();
        kept += 1;
    }
    let dropped = sections.len() - kept;

    let mut out = String::with_capacity(used + 512);
    let _ = writeln!(out, "# Transcript of a {engine} session");
    out.push('\n');
    let _ = writeln!(
        out,
        "Recorded from the session started {}. {} turn{}{}.",
        session.created_at.to_rfc3339(),
        turns.len(),
        if turns.len() == 1 { "" } else { "s" },
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

    for section in &sections[dropped..] {
        out.push('\n');
        out.push_str(section);
    }

    RenderedTranscript {
        markdown: out,
        turns: kept as u32,
        truncated: dropped > 0,
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
