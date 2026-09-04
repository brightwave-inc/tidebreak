//! Fork: the parent session's transcript, written outside the Git worktree as
//! markdown so a sibling agent of any engine can read it.
//!
//! Each fork is one immutable directory holding two layers. `transcript.md`
//! is the condensed conversation — tool calls as one-line entries, subagent
//! work summarized by its `Task` call — and stays deliberately small so the
//! child spends its context on the work rather than on history. Next to it,
//! one record per turn (`turn-0007.md` for turn 7) holds what the summary
//! leaves out: complete tool output previews, subagent activity, and the
//! engine's reasoning when bounded replay retained that whole turn. An
//! omitted turn keeps its request and an explicit marker. The child reads the
//! summary first and opens a record only when the summary is not enough.
//!
//! Pure serialization of what the journal already holds. No model call and no
//! generated summary — that would be a second thing to be wrong; the point of
//! these files is that the child sees what the parent saw.
//!
//! The files land under Tidebreak's profile data directory. Git cannot index
//! the transcript, the records, or the retained attachment generations.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tidebreak_core::code::SequencedEvent;
use tidebreak_core::{
    BlobStore, DocumentBlob, Event, HarnessKind, HarnessNoticeLevel, Session, ToolDetail,
    ToolOutcome, Turn, TurnId, TurnStatus,
};

/// Directory holding fork transcripts below the workspace's private root.
pub const FORKS_DIR: &str = "forks";

/// The condensed transcript's file name inside a fork's directory.
pub const SUMMARY_NAME: &str = "transcript.md";

/// Largest condensed transcript written, in bytes. Bounds the whole file,
/// header included.
///
/// A long session can hold megabytes of assistant text. The child needs
/// recent context far more than it needs the first turn, so the oldest turns
/// fall back to one-line stubs — and past that are dropped — to fit, and the
/// header says so. One turn can be over the cap by itself, so that one is cut
/// rather than written past it.
const MAX_TRANSCRIPT_BYTES: usize = 512 * 1024;

/// Largest single full-record file. A record over this keeps its head — the
/// ask and the first steps — and says where it was cut.
const MAX_RECORD_FILE_BYTES: usize = 256 * 1024;

/// Largest set of full records written for one fork. Records are kept from
/// the newest turn back; older turns past the budget get no record file, and
/// their stubs say so.
const MAX_RECORD_TOTAL_BYTES: usize = 8 * 1024 * 1024;

/// Largest set of retained fork images materialized in one request. Retained
/// from the newest turn back; a turn past the budget keeps its transcript
/// entry but not its image bytes.
const MAX_FORK_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

type ForkLockRegistry = Mutex<
    std::collections::HashMap<(PathBuf, tidebreak_core::SessionId), Weak<tokio::sync::Mutex<()>>>,
>;

fn fork_lock_registry() -> &'static ForkLockRegistry {
    static REGISTRY: OnceLock<ForkLockRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn fork_lock(
    private_root: &Path,
    session_id: tidebreak_core::SessionId,
) -> Arc<tokio::sync::Mutex<()>> {
    let key = (private_root.to_path_buf(), session_id);
    let mut registry = fork_lock_registry().lock().expect("fork write registry");
    registry.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = registry.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    registry.insert(key, Arc::downgrade(&lock));
    lock
}

/// A session's turns sliced at a fork point.
pub struct ForkCut<'a> {
    /// The turns the fork covers, oldest first, ending at the fork point.
    pub turns: &'a [Turn],
    /// Turns of the session that ran after the fork point.
    pub excluded: usize,
}

/// Why a requested fork does not name a settled turn boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkBoundaryError {
    /// The session has no turn to hand off.
    NoTurns,
    /// The requested turn does not belong to this session.
    UnknownTurn,
    /// The selected turn is still running.
    Running { ordinal: i64 },
    /// A selected turn still has an approval that can reach the engine.
    PendingApproval { ordinal: i64 },
    /// The terminal row exists, but its final journal event has not landed.
    Settling { ordinal: i64 },
    /// A session-level fork would silently omit accepted future input.
    QueuedFollowUp,
}

impl ForkBoundaryError {
    /// Stable HTTP error kind for this refusal.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::UnknownTurn => "fork_turn_not_found",
            Self::NoTurns
            | Self::Running { .. }
            | Self::PendingApproval { .. }
            | Self::Settling { .. } => "fork_turn_unsettled",
            Self::QueuedFollowUp => "fork_queue_pending",
        }
    }

    /// Reader-facing recovery instruction.
    pub fn message(self) -> String {
        match self {
            Self::NoTurns => "this session has no completed turn to fork".to_owned(),
            Self::UnknownTurn => "that turn is not part of this session".to_owned(),
            Self::Running { ordinal } => format!(
                "turn {ordinal} is still running; wait for it to finish, or fork from an earlier completed turn"
            ),
            Self::PendingApproval { ordinal } => format!(
                "turn {ordinal} still has a pending approval; settle it before forking from this turn"
            ),
            Self::Settling { ordinal } => format!(
                "turn {ordinal} is still settling; retry after its final event is recorded, or fork from an earlier completed turn"
            ),
            Self::QueuedFollowUp => {
                "this session has a queued follow-up; wait for it to finish, or fork from an earlier completed turn"
                    .to_owned()
            }
        }
    }
}

/// Slice a session's turns at the end of `at_turn`.
///
/// `None` forks at the newest turn. Returns `None` when `at_turn` names a
/// turn that is not part of this session, which the route reports rather
/// than silently forking the whole conversation.
pub fn cut_at(turns: &[Turn], at_turn: Option<TurnId>) -> Option<ForkCut<'_>> {
    let Some(at) = at_turn else {
        return Some(ForkCut { turns, excluded: 0 });
    };
    let position = turns.iter().position(|turn| turn.id == at)?;
    Some(ForkCut {
        turns: &turns[..=position],
        excluded: turns.len() - position - 1,
    })
}

/// Select a fork only at a turn whose durable work has settled.
///
/// A session-level fork means "everything accepted so far," so a queued
/// follow-up blocks it. An explicit turn fork may leave later running or
/// queued work behind because the reader selected that earlier seam.
pub fn cut_at_settled_boundary<'a>(
    turns: &'a [Turn],
    boundary_status: Option<TurnStatus>,
    pending_approval_turns: &HashSet<TurnId>,
    has_queued_follow_up: bool,
    at_turn: Option<TurnId>,
) -> Result<ForkCut<'a>, ForkBoundaryError> {
    let Some(cut) = cut_at(turns, at_turn) else {
        return Err(ForkBoundaryError::UnknownTurn);
    };
    let Some(boundary) = cut.turns.last() else {
        return Err(ForkBoundaryError::NoTurns);
    };
    if boundary.status == TurnStatus::Running {
        return Err(ForkBoundaryError::Running {
            ordinal: boundary.ordinal,
        });
    }
    if let Some(pending) = cut
        .turns
        .iter()
        .find(|turn| pending_approval_turns.contains(&turn.id))
    {
        return Err(ForkBoundaryError::PendingApproval {
            ordinal: pending.ordinal,
        });
    }
    if at_turn.is_none() && has_queued_follow_up {
        return Err(ForkBoundaryError::QueuedFollowUp);
    }
    // The worker writes the terminal row before its final journal event. The
    // fork-specific replay scan reports that exact event even when the turn
    // itself is too large to retain in the transcript window.
    if boundary_status != Some(boundary.status) {
        return Err(ForkBoundaryError::Settling {
            ordinal: boundary.ordinal,
        });
    }
    Ok(cut)
}

/// A written fork, as the route reports it.
pub struct WrittenTranscript {
    /// Absolute path of the condensed transcript, in the form the composer
    /// shows and the engine opens.
    pub path: String,
    /// Absolute path of the fork's directory, holding the per-turn records
    /// and any retained attachments next to the transcript.
    pub dir: String,
    /// Bytes of the condensed transcript on disk.
    pub byte_len: u64,
    /// Complete turn histories the condensed transcript renders in full.
    pub turns: u32,
    /// Turns the fork covers, up to and including the fork point.
    pub total_turns: u32,
    /// The fork point's ordinal, when the conversation continued past it.
    pub at_turn_ordinal: Option<i64>,
    /// True when bounded replay omitted whole turn histories or the condensed
    /// transcript reduced content to fit [`MAX_TRANSCRIPT_BYTES`].
    pub truncated: bool,
}

/// Render one fork and write its directory under private storage.
///
/// Every fork writes a fresh generation directory named by a UUID, so a
/// child keeps reading a whole, immutable handoff no matter how many later
/// forks the same session produces. The generation is kept for the worktree
/// lifecycle; a failure before the transcript publishes removes the whole
/// directory rather than leaving a partial handoff for the child to trip
/// over.
pub(crate) async fn write_transcript(
    private_root: &super::scratch::ScratchRoot,
    blobs: &dyn BlobStore,
    session: &Session,
    cut: ForkCut<'_>,
    events: &[SequencedEvent],
    complete_turns: &HashSet<TurnId>,
) -> std::io::Result<WrittenTranscript> {
    let private_path = private_root.path();
    let write_lock = fork_lock(private_path, session.id);
    let _write = write_lock.lock().await;
    let generation = uuid::Uuid::new_v4();
    let turns = cut.turns;
    let engine = harness_label(session.harness_kind);

    let scope = super::scratch::scratch_scope(private_root, FORKS_DIR, session.id.0, generation)?;

    let retained_images = plan_attachment_retention(turns);
    materialize_attachments(&scope, blobs, turns, &retained_images).await?;

    let records = render_turn_records(
        private_path,
        session,
        turns,
        events,
        engine,
        generation,
        &retained_images,
        complete_turns,
    );
    for (ordinal, markdown) in &records.files {
        scope
            .publish(
                std::ffi::OsStr::new(&record_name(*ordinal)),
                markdown.as_bytes(),
            )
            .await?;
    }

    let rendered = render_transcript_with_generation(
        private_path,
        session,
        turns,
        cut.excluded,
        events,
        generation,
        &records.ordinals,
        &retained_images,
        complete_turns,
    );
    scope
        .publish(
            std::ffi::OsStr::new(SUMMARY_NAME),
            rendered.markdown.as_bytes(),
        )
        .await?;
    scope.keep();

    let dir = private_path
        .join(FORKS_DIR)
        .join(session.id.to_string())
        .join(generation.to_string());
    Ok(WrittenTranscript {
        path: dir.join(SUMMARY_NAME).display().to_string(),
        dir: dir.display().to_string(),
        byte_len: rendered.markdown.len() as u64,
        turns: rendered.turns,
        total_turns: turns.len() as u32,
        at_turn_ordinal: (cut.excluded > 0)
            .then(|| turns.last().map(|turn| turn.ordinal))
            .flatten(),
        truncated: rendered.truncated,
    })
}

/// A turn's full-record file name, `turn-0007.md` for turn 7.
fn record_name(ordinal: i64) -> String {
    format!("turn-{ordinal:04}.md")
}

/// A rendered transcript and what had to be left out of it.
struct RenderedTranscript {
    markdown: String,
    turns: u32,
    truncated: bool,
}

/// Serialize a session as condensed markdown, newest turns kept in full.
///
/// The result is never larger than [`MAX_TRANSCRIPT_BYTES`]. Turns are kept
/// in full from the newest back; older turns fall back to one-line stubs
/// pointing at their record files, then drop entirely, oldest first. The
/// header counts against the same budget, and a turn that is over the budget
/// by itself is cut instead of overrunning it.
#[allow(clippy::too_many_arguments)]
fn render_transcript_with_generation(
    private_root: &Path,
    session: &Session,
    turns: &[Turn],
    excluded: usize,
    events: &[SequencedEvent],
    generation: uuid::Uuid,
    record_ordinals: &HashSet<i64>,
    retained_images: &HashSet<TurnId>,
    complete_turns: &HashSet<TurnId>,
) -> RenderedTranscript {
    let engine = harness_label(session.harness_kind);
    let mut sections: Vec<String> = Vec::with_capacity(turns.len());
    for turn in turns {
        sections.push(render_turn(
            private_root,
            session.id,
            turn,
            events,
            engine,
            generation,
            retained_images.contains(&turn.id),
            complete_turns.contains(&turn.id),
        ));
    }

    // The header is part of the file, so it is part of the budget. Its length
    // depends on how many turns are reduced, which is what the budget decides
    // — so reserve the longest it can be, the one that reduces every turn.
    let reserved = header(
        session,
        turns,
        excluded,
        turns.len(),
        record_ordinals,
        engine,
        complete_turns,
    )
    .len();
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

    // Turns that lost their full section still get a one-line stub — what was
    // asked, and where the full record is — newest first, while they fit.
    let stubs: Vec<String> = turns[..dropped]
        .iter()
        .map(|turn| {
            render_stub(
                turn,
                record_ordinals.contains(&turn.ordinal),
                complete_turns.contains(&turn.id),
            )
        })
        .collect();
    let mut stub_from = dropped;
    for (at, stub) in stubs.iter().enumerate().rev() {
        if used + stub.len() + 1 > budget {
            break;
        }
        used += stub.len() + 1;
        stub_from = at;
    }

    let mut out = String::with_capacity(reserved + used);
    out.push_str(&header(
        session,
        turns,
        excluded,
        dropped,
        record_ordinals,
        engine,
        complete_turns,
    ));
    for stub in &stubs[stub_from..] {
        out.push('\n');
        out.push_str(stub);
    }
    for section in &sections[dropped..] {
        out.push('\n');
        out.push_str(section);
    }

    let complete = turns
        .iter()
        .filter(|turn| complete_turns.contains(&turn.id))
        .count();
    RenderedTranscript {
        markdown: out,
        turns: turns[dropped..]
            .iter()
            .filter(|turn| complete_turns.contains(&turn.id))
            .count() as u32,
        truncated: dropped > 0 || clipped || complete < turns.len(),
    }
}

/// Decide which turns keep their image bytes, newest back, within
/// [`MAX_FORK_ATTACHMENT_BYTES`].
///
/// A turn keeps all of its images or none of them, so a transcript entry
/// never names half of what a message attached. The first turn that would
/// overflow the budget stops retention: everything older is dropped with it,
/// and the transcript says so per turn.
fn plan_attachment_retention(turns: &[Turn]) -> HashSet<TurnId> {
    let mut retained = HashSet::new();
    let mut total: u64 = 0;
    for turn in turns.iter().rev() {
        if turn.attachments.is_empty() {
            continue;
        }
        let Some(cost) = turn
            .attachments
            .iter()
            .try_fold(0u64, |sum, image| sum.checked_add(image.byte_len))
            .and_then(|sum| total.checked_add(sum))
        else {
            break;
        };
        if cost > MAX_FORK_ATTACHMENT_BYTES {
            break;
        }
        total = cost;
        retained.insert(turn.id);
    }
    retained
}

async fn materialize_attachments(
    scope: &super::scratch::ScratchScope,
    blobs: &dyn BlobStore,
    turns: &[Turn],
    retained: &HashSet<TurnId>,
) -> std::io::Result<()> {
    for turn in turns {
        if !retained.contains(&turn.id) {
            continue;
        }
        for (ordinal, image) in turn.attachments.iter().enumerate() {
            let bytes = blobs
                .get(image.blob_id)
                .await
                .map_err(|error| std::io::Error::other(format!("read attachment blob: {error}")))?
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "fork attachment blob {} is missing",
                        image.blob_id
                    ))
                })?;
            let actual_len = u64::try_from(bytes.len())
                .map_err(|_| std::io::Error::other("fork attachment length exceeds u64"))?;
            if actual_len != image.byte_len
                || tidebreak_core::ImageMediaType::sniff(&bytes) != Some(image.media_type)
                || DocumentBlob::from_bytes(&bytes).id != image.blob_id
            {
                return Err(std::io::Error::other(format!(
                    "fork attachment {} does not match its retained descriptor",
                    image.blob_id
                )));
            }
            let name = fork_attachment_name(turn, ordinal, image);
            scope.publish(std::ffi::OsStr::new(&name), &bytes).await?;
        }
    }
    Ok(())
}

fn fork_attachment_name(turn: &Turn, ordinal: usize, image: &tidebreak_core::ImageRef) -> String {
    format!(
        "turn-{}-{}-{}.{}",
        turn.ordinal,
        ordinal + 1,
        image.blob_id,
        image.media_type.extension()
    )
}

fn fork_attachment_path(
    private_root: &Path,
    session_id: tidebreak_core::SessionId,
    generation: uuid::Uuid,
    turn: &Turn,
    ordinal: usize,
    image: &tidebreak_core::ImageRef,
) -> String {
    private_root
        .join(FORKS_DIR)
        .join(session_id.to_string())
        .join(generation.to_string())
        .join(fork_attachment_name(turn, ordinal, image))
        .display()
        .to_string()
}

/// The file's opening: where the transcript came from, what it condenses,
/// and where the full records are.
fn header(
    session: &Session,
    turns: &[Turn],
    excluded: usize,
    reduced: usize,
    record_ordinals: &HashSet<i64>,
    engine: &str,
    complete_turns: &HashSet<TurnId>,
) -> String {
    let total = turns.len();
    let mut out = String::new();
    let _ = writeln!(out, "# Transcript of a {engine} session");
    out.push('\n');
    let _ = write!(
        out,
        "Recorded from the session started {}. {} turn{}",
        session.created_at.to_rfc3339(),
        total,
        if total == 1 { "" } else { "s" },
    );
    if excluded > 0 {
        if let Some(at) = turns.last() {
            let _ = write!(
                out,
                ", forked at the end of turn {}; the conversation continued for {} more turn{}, so the worktree may already hold work this transcript does not describe",
                at.ordinal,
                excluded,
                if excluded == 1 { "" } else { "s" },
            );
        }
    }
    out.push_str(".\n");
    if reduced > 0 {
        let _ = writeln!(
            out,
            "The {reduced} oldest turn{} appear{} only as stubs here, or not at all, to fit this file's size cap.",
            if reduced == 1 { "" } else { "s" },
            if reduced == 1 { "s" } else { "" },
        );
    }
    let omitted = turns
        .iter()
        .filter(|turn| !complete_turns.contains(&turn.id))
        .count();
    if omitted > 0 {
        let _ = writeln!(
            out,
            "The bounded journal replay omitted the engine events for {omitted} turn{} as a whole because it could not retain every framing record required to reconstruct them.",
            if omitted == 1 { "" } else { "s" },
        );
    }
    out.push('\n');
    out.push_str(
        "Messages and tool calls are as the engine reported them, condensed: a \
         tool call is one line, and work a subagent did inside a `Task` call is \
         summarized by that call.\n",
    );
    if !record_ordinals.is_empty() {
        out.push('\n');
        out.push_str(
            "Each retained turn record sits in this file's directory — \
             `turn-0007.md` for turn 7. A complete record holds tool output \
             and subagent activity; a turn omitted from bounded replay keeps \
             its request and an explicit omission marker. Read a record only \
             when this summary is not enough.\n",
        );
        let missing = turns
            .iter()
            .filter(|turn| !record_ordinals.contains(&turn.ordinal))
            .count();
        if missing > 0 {
            let _ = writeln!(
                out,
                "The {missing} oldest turn{} ha{} no record file; they were dropped to fit the record size cap.",
                if missing == 1 { "" } else { "s" },
                if missing == 1 { "s" } else { "ve" },
            );
        }
    }
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

/// One reduced turn: the ask's first line, and where the full record is.
///
/// The heading deliberately differs from a full section's `— you`, so the
/// child can tell a stub from a turn it can read here.
fn render_stub(turn: &Turn, has_record: bool, events_complete: bool) -> String {
    let asked = turn.user_input.trim();
    let asked = if asked.is_empty() {
        "_(empty)_".to_owned()
    } else {
        let flat = one_line(asked);
        format!("“{flat}”")
    };
    let mut out = String::new();
    let _ = writeln!(out, "## Turn {} (condensed)\n", turn.ordinal);
    if !events_complete {
        out.push_str(
            "The engine events for this turn were omitted as a whole from bounded replay. ",
        );
    }
    let _ = writeln!(
        out,
        "Asked: {asked}. {}",
        if has_record {
            format!("Turn record: `{}`.", record_name(turn.ordinal))
        } else {
            "No record file was retained for this turn.".to_owned()
        }
    );
    out
}

/// One turn: what the reader asked, then what the engine said and did.
#[allow(clippy::too_many_arguments)]
fn render_turn(
    private_root: &Path,
    session_id: tidebreak_core::SessionId,
    turn: &Turn,
    events: &[SequencedEvent],
    engine: &str,
    generation: uuid::Uuid,
    images_retained: bool,
    events_complete: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Turn {} — you\n", turn.ordinal);
    render_attachment_list(
        &mut out,
        private_root,
        session_id,
        turn,
        generation,
        images_retained,
    );
    let asked = turn.user_input.trim();
    let _ = writeln!(
        out,
        "{}\n",
        if asked.is_empty() { "_(empty)_" } else { asked }
    );

    if !events_complete {
        let _ = writeln!(out, "## Turn {} — {engine}\n", turn.ordinal);
        out.push_str(
            "_This turn's engine events were omitted as a whole because the bounded journal replay could not retain every framing record required to reconstruct them._\n",
        );
        return out;
    }

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

/// The attached-image list a turn section opens with, in both layers.
fn render_attachment_list(
    out: &mut String,
    private_root: &Path,
    session_id: tidebreak_core::SessionId,
    turn: &Turn,
    generation: uuid::Uuid,
    images_retained: bool,
) {
    if turn.attachments.is_empty() {
        return;
    }
    if !images_retained {
        let _ = writeln!(
            out,
            "Images attached to this message were not retained: they fell \
             outside the fork's attachment size cap.\n",
        );
        return;
    }
    let noun = if turn.attachments.len() == 1 {
        "Image"
    } else {
        "Images"
    };
    let _ = writeln!(out, "{noun} attached to this message:");
    for (ordinal, image) in turn.attachments.iter().enumerate() {
        let _ = writeln!(
            out,
            "- `{}`",
            fork_attachment_path(private_root, session_id, generation, turn, ordinal, image)
        );
    }
    out.push('\n');
}

/// The engine's own messages and tool calls for one turn, in journal order.
///
/// Scoped by walking from this turn's `TurnStarted` to the next one: a turn
/// id appears on the boundary events, not on every event inside it.
fn turn_lines(turn_id: TurnId, events: &[SequencedEvent]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut inside = false;
    // A tool call opens and closes on separate events, and the closing one
    // carries the outcome plus a detail that may say more than the opening
    // one did. Hold the call's name, detail, and line index until then.
    let mut open: Vec<(String, usize, String, ToolDetail)> = Vec::new();

    for entry in events {
        match &entry.event {
            Event::TurnStarted { turn_id: started } => {
                if inside {
                    break;
                }
                inside = *started == turn_id;
            }
            _ if !inside => {}
            Event::AssistantMessage {
                text,
                parent_call_id,
            } => {
                if parent_call_id.is_none() && !text.trim().is_empty() {
                    lines.push(text.trim().to_owned());
                }
            }
            Event::UserSteered { text, .. } => {
                if !text.trim().is_empty() {
                    lines.push(format!("**You, mid-turn:** {}", text.trim()));
                }
            }
            Event::ToolStarted {
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
            Event::ToolCompleted {
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
                    outcome_suffix(outcome)
                );
            }
            Event::TurnFailed { error, .. } => {
                lines.push(format!("**The turn failed:** {}", error.message.trim()));
            }
            Event::TurnInterrupted { .. } => lines.push("**The turn was interrupted.**".to_owned()),
            Event::TurnResumed { .. } => {
                lines.push("**The turn resumed after the worker restarted.**".to_owned())
            }
            Event::TurnRefused { .. } => {
                lines.push("**The model declined to continue.**".to_owned())
            }
            _ => {}
        }
    }
    lines
}

/// The full-record files for a fork, newest turns first when over budget.
struct RenderedRecords {
    /// `(ordinal, markdown)` for each record that fit, oldest first.
    files: Vec<(i64, String)>,
    /// The ordinals in `files`, for the summary's stubs and header.
    ordinals: HashSet<i64>,
}

/// Render every turn's full record, kept from the newest back within
/// [`MAX_RECORD_TOTAL_BYTES`], each clipped to [`MAX_RECORD_FILE_BYTES`].
#[allow(clippy::too_many_arguments)]
fn render_turn_records(
    private_root: &Path,
    session: &Session,
    turns: &[Turn],
    events: &[SequencedEvent],
    engine: &str,
    generation: uuid::Uuid,
    retained_images: &HashSet<TurnId>,
    complete_turns: &HashSet<TurnId>,
) -> RenderedRecords {
    let mut files: Vec<(i64, String)> = Vec::with_capacity(turns.len());
    let mut total = 0usize;
    for turn in turns.iter().rev() {
        let mut record = render_turn_record(
            private_root,
            session,
            turn,
            events,
            engine,
            generation,
            retained_images.contains(&turn.id),
            complete_turns.contains(&turn.id),
        );
        clip(&mut record, MAX_RECORD_FILE_BYTES);
        if total + record.len() > MAX_RECORD_TOTAL_BYTES {
            break;
        }
        total += record.len();
        files.push((turn.ordinal, record));
    }
    files.reverse();
    let ordinals = files.iter().map(|(ordinal, _)| *ordinal).collect();
    RenderedRecords { files, ordinals }
}

/// One turn's full record: the complete ask, then everything the journal
/// holds for the turn — messages, reasoning, tool calls with their output
/// previews, and subagent activity nested under the `Task` call that ran it.
#[allow(clippy::too_many_arguments)]
fn render_turn_record(
    private_root: &Path,
    session: &Session,
    turn: &Turn,
    events: &[SequencedEvent],
    engine: &str,
    generation: uuid::Uuid,
    images_retained: bool,
    events_complete: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Turn {} — {} record\n",
        turn.ordinal,
        if events_complete { "full" } else { "request" }
    );
    let _ = writeln!(out, "## Turn {} — you\n", turn.ordinal);
    render_attachment_list(
        &mut out,
        private_root,
        session.id,
        turn,
        generation,
        images_retained,
    );
    let asked = turn.user_input.trim();
    let _ = writeln!(
        out,
        "{}\n",
        if asked.is_empty() { "_(empty)_" } else { asked }
    );

    if !events_complete {
        let _ = writeln!(
            out,
            "_The bounded journal replay omitted this turn's engine events as a whole because it could not retain every framing record required to reconstruct them; only the request above survives._",
        );
        return out;
    }
    let blocks = record_blocks(turn.id, events);
    if blocks.is_empty() {
        return out;
    }
    let _ = writeln!(out, "## Turn {} — {engine}\n", turn.ordinal);
    for block in blocks {
        let _ = writeln!(out, "{block}\n");
    }
    out
}

/// The engine's work for one turn, at full fidelity, in journal order.
///
/// Unlike [`turn_lines`], subagent events stay — indented under their `Task`
/// call — tool previews ride their call, and reasoning runs are kept as
/// quoted blocks.
fn record_blocks(turn_id: TurnId, events: &[SequencedEvent]) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut inside = false;
    let mut reasoning: Vec<&str> = Vec::new();
    // An open call remembers its block, its opening detail, and its depth, so
    // the completion can rewrite the same block with the outcome and preview.
    let mut open: Vec<(String, usize, String, ToolDetail, bool)> = Vec::new();

    fn flush_reasoning(blocks: &mut Vec<String>, reasoning: &mut Vec<&str>) {
        if reasoning.is_empty() {
            return;
        }
        let text = reasoning.join("");
        *reasoning = Vec::new();
        if text.trim().is_empty() {
            return;
        }
        let mut block = String::from("> _Thinking:_");
        for line in text.trim().lines() {
            block.push_str("\n> ");
            block.push_str(line);
        }
        blocks.push(block);
    }

    for entry in events {
        if let Event::TurnStarted { turn_id: started } = &entry.event {
            if inside {
                break;
            }
            inside = *started == turn_id;
            continue;
        }
        if !inside {
            continue;
        }
        if let Event::ReasoningDelta { text } = &entry.event {
            reasoning.push(text);
            continue;
        }
        flush_reasoning(&mut blocks, &mut reasoning);
        match &entry.event {
            Event::AssistantMessage {
                text,
                parent_call_id,
            } => {
                if text.trim().is_empty() {
                    continue;
                }
                if parent_call_id.is_none() {
                    blocks.push(text.trim().to_owned());
                } else {
                    blocks.push(format!(
                        "  - The subagent said:\n{}",
                        indent_block(text.trim(), "    ")
                    ));
                }
            }
            Event::UserSteered { text, .. } => {
                if !text.trim().is_empty() {
                    blocks.push(format!("**You, mid-turn:** {}", text.trim()));
                }
            }
            Event::ToolStarted {
                call_id,
                name,
                detail,
                parent_call_id,
            } => {
                let nested = parent_call_id.is_some();
                open.push((
                    call_id.clone(),
                    blocks.len(),
                    name.clone(),
                    detail.clone(),
                    nested,
                ));
                let line = tool_line(name, detail, None);
                blocks.push(if nested { format!("  {line}") } else { line });
            }
            Event::ToolCompleted {
                call_id,
                outcome,
                preview,
                detail,
                ..
            } => {
                let Some(found) = open.iter().position(|(id, ..)| id == call_id) else {
                    continue;
                };
                let (_, at, name, started, nested) = open.remove(found);
                let Some(block) = blocks.get_mut(at) else {
                    continue;
                };
                let indent = if nested { "  " } else { "" };
                let mut line = format!(
                    "{indent}{}{}",
                    tool_line(&name, &started, detail.as_ref()),
                    outcome_suffix(outcome)
                );
                if !preview.trim().is_empty() {
                    line.push('\n');
                    line.push_str(&fenced(preview.trim_end(), &format!("{indent}  ")));
                }
                *block = line;
            }
            Event::HarnessNotice { level, message } => {
                blocks.push(format!(
                    "**Engine notice ({}):** {}",
                    notice_level(*level),
                    message.trim()
                ));
            }
            Event::TurnFailed { error, .. } => {
                blocks.push(format!("**The turn failed:** {}", error.message.trim()));
            }
            Event::TurnInterrupted { .. } => {
                blocks.push("**The turn was interrupted.**".to_owned());
            }
            Event::TurnResumed { .. } => {
                blocks.push("**The turn resumed after the worker restarted.**".to_owned());
            }
            Event::TurnRefused { .. } => {
                blocks.push("**The model declined to continue.**".to_owned());
            }
            _ => {}
        }
    }
    flush_reasoning(&mut blocks, &mut reasoning);
    blocks
}

fn outcome_suffix(outcome: &ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Succeeded => "",
        ToolOutcome::Failed => " (failed)",
        ToolOutcome::Denied => " (denied)",
    }
}

fn notice_level(level: HarnessNoticeLevel) -> &'static str {
    match level {
        HarnessNoticeLevel::Info => "info",
        HarnessNoticeLevel::Warning => "warning",
        HarnessNoticeLevel::Error => "error",
    }
}

/// A preview as a fenced block, indented to sit under its call's bullet.
///
/// The fence outgrows any backtick run inside the content, so a preview that
/// itself contains fenced code cannot break out of the block.
fn fenced(content: &str, indent: &str) -> String {
    let longest_run = content.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest_run + 1).max(3));
    let mut out = format!("{indent}{fence}");
    for line in content.lines() {
        out.push('\n');
        out.push_str(indent);
        out.push_str(line);
    }
    out.push('\n');
    out.push_str(indent);
    out.push_str(&fence);
    out
}

/// Prefix every line of a block, so nested content stays under its bullet.
fn indent_block(text: &str, indent: &str) -> String {
    text.lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
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
        HarnessKind::Internal => "Tidebreak",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{
        Attention, AttentionSource, BoundedError, FsBlobStore, ImageMediaType, ImageRef, OwnerId,
        PermissionMode, SessionId, SessionKind, SessionLifecycle, TurnStatus, WorkspaceId,
    };

    fn session() -> Session {
        Session {
            visibility: tidebreak_core::SessionVisibility::Private,
            id: SessionId::new(),
            owner: OwnerId::local(),
            workspace_id: Some(WorkspaceId::new()),
            kind: SessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: SessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: chrono::Utc::now(),
            execution_location: tidebreak_core::ExecutionLocation::Machine,
        }
    }

    fn turn(session_id: SessionId, ordinal: i64, asked: &str) -> Turn {
        Turn {
            actor: None,
            id: TurnId::new(),
            session_id,
            ordinal,
            status: TurnStatus::Completed,
            model: None,
            fast_mode: false,
            user_input: asked.to_owned(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            rewrite: None,
            started_at: chrono::Utc::now(),
            ended_at: None,
            park_ref: None,
            park_wait: None,
        }
    }

    fn seq(events: Vec<Event>) -> Vec<SequencedEvent> {
        events
            .into_iter()
            .enumerate()
            .map(|(at, event)| SequencedEvent {
                seq: at as i64 + 1,
                event,
            })
            .collect()
    }

    fn all_turn_ids(turns: &[Turn]) -> HashSet<TurnId> {
        turns.iter().map(|turn| turn.id).collect()
    }

    fn test_root(directory: &tempfile::TempDir) -> super::super::scratch::ScratchRoot {
        super::super::scratch::ScratchRoot::open_for_test(directory.path()).expect("scratch root")
    }

    /// Render the condensed transcript as if every turn had a record file and
    /// every image was retained, which is what most summary tests care about.
    fn render_transcript(
        session: &Session,
        turns: &[Turn],
        events: &[SequencedEvent],
    ) -> RenderedTranscript {
        let records: HashSet<i64> = turns.iter().map(|turn| turn.ordinal).collect();
        let retained: HashSet<TurnId> = turns.iter().map(|turn| turn.id).collect();
        render_transcript_with_generation(
            Path::new("/private"),
            session,
            turns,
            0,
            events,
            uuid::Uuid::nil(),
            &records,
            &retained,
            &retained,
        )
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
            Event::TurnStarted { turn_id: first.id },
            Event::AssistantMessage {
                text: "Looking at the test now.".to_owned(),
                parent_call_id: None,
            },
            Event::ToolStarted {
                call_id: "call-1".to_owned(),
                name: "Bash".to_owned(),
                detail: ToolDetail::Other {
                    summary: String::new(),
                },
                parent_call_id: None,
            },
            Event::ToolCompleted {
                call_id: "call-1".to_owned(),
                outcome: ToolOutcome::Failed,
                preview: "1 failed".to_owned(),
                output: None,
                action: None,
                result: None,
                detail: Some(ToolDetail::Command {
                    cmd: "cargo test -p auth".to_owned(),
                    cwd: String::new(),
                }),
                parent_call_id: None,
            },
            Event::TurnStarted { turn_id: second.id },
            Event::AssistantMessage {
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
        // The summary stays condensed: the tool's output lives in the record.
        assert!(!markdown.contains("1 failed"));
        // Turn two's message must not have leaked into turn one's section.
        let turn_two = markdown.find("## Turn 2 — you").expect("turn two heading");
        assert!(markdown.find("Pushed.").expect("second message") > turn_two);
    }

    /// Subagent chatter belongs to the subagent. The parent's `Task` call is
    /// what the child needs to see in the summary, not the thousand lines
    /// inside it.
    #[test]
    fn leaves_subagent_events_to_the_task_call_that_ran_them() {
        let session = session();
        let only = turn(session.id, 1, "audit the crate");
        let events = seq(vec![
            Event::TurnStarted { turn_id: only.id },
            Event::ToolStarted {
                call_id: "task-1".to_owned(),
                name: "Task".to_owned(),
                detail: ToolDetail::Other {
                    summary: "audit".to_owned(),
                },
                parent_call_id: None,
            },
            Event::AssistantMessage {
                text: "subagent thinking out loud".to_owned(),
                parent_call_id: Some("task-1".to_owned()),
            },
            Event::ToolCompleted {
                call_id: "task-1".to_owned(),
                outcome: ToolOutcome::Succeeded,
                preview: "done".to_owned(),
                output: None,
                action: None,
                result: None,
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
            Event::TurnStarted { turn_id: only.id },
            Event::TurnFailed {
                error: BoundedError {
                    message: "the engine exited".to_owned(),
                },
                detail: None,
            },
        ]);

        let markdown = render_transcript(&session, &[only], &events).markdown;
        assert!(markdown.contains("**The turn failed:** the engine exited"));
    }

    /// The fork point is a turn boundary: later turns leave the handoff, and
    /// their count survives so the header can warn about the shared worktree.
    #[test]
    fn cuts_at_a_turn_and_counts_the_excluded_tail() {
        let session = session();
        let turns: Vec<Turn> = (1..=5)
            .map(|ordinal| turn(session.id, ordinal, "work"))
            .collect();

        let cut = cut_at(&turns, Some(turns[1].id)).expect("a known turn");
        assert_eq!(cut.turns.len(), 2);
        assert_eq!(cut.excluded, 3);

        let whole = cut_at(&turns, None).expect("no fork point");
        assert_eq!(whole.turns.len(), 5);
        assert_eq!(whole.excluded, 0);

        assert!(cut_at(&turns, Some(TurnId::new())).is_none());
    }

    /// A session-level fork must not turn the partial journal prefix of the
    /// newest running turn into an immutable handoff.
    #[test]
    fn refuses_a_running_newest_turn() {
        let session = session();
        let completed = turn(session.id, 1, "first");
        let mut running = turn(session.id, 2, "keep working");
        running.status = TurnStatus::Running;
        let turns = [completed.clone(), running];

        let result = cut_at_settled_boundary(&turns, None, &HashSet::new(), false, None);

        assert!(matches!(
            result,
            Err(ForkBoundaryError::Running { ordinal: 2 })
        ));

        let earlier = cut_at_settled_boundary(
            &turns,
            Some(TurnStatus::Completed),
            &HashSet::new(),
            false,
            Some(completed.id),
        )
        .expect("earlier completed boundary");
        assert_eq!(earlier.turns.len(), 1);
        assert_eq!(earlier.excluded, 1);
    }

    /// The turn row can become terminal before approval cleanup commits. A
    /// pending approval keeps that boundary open until the settlement lands.
    #[test]
    fn refuses_a_terminal_turn_with_a_parked_approval() {
        let session = session();
        let completed = turn(session.id, 1, "run the command");
        let pending = HashSet::from([completed.id]);
        let turns = [completed];

        let result =
            cut_at_settled_boundary(&turns, Some(TurnStatus::Completed), &pending, false, None);

        assert!(matches!(
            result,
            Err(ForkBoundaryError::PendingApproval { ordinal: 1 })
        ));
    }

    /// A session-level fork cannot claim to include an accepted queued
    /// follow-up. The explicit seam remains available because it names the
    /// exact completed boundary the reader chose.
    #[test]
    fn queued_follow_up_requires_an_explicit_turn_boundary() {
        let session = session();
        let completed = turn(session.id, 1, "first");
        let ordinary = cut_at_settled_boundary(
            std::slice::from_ref(&completed),
            Some(TurnStatus::Completed),
            &HashSet::new(),
            true,
            None,
        );
        assert!(matches!(ordinary, Err(ForkBoundaryError::QueuedFollowUp)));

        let explicit = cut_at_settled_boundary(
            std::slice::from_ref(&completed),
            Some(TurnStatus::Completed),
            &HashSet::new(),
            true,
            Some(completed.id),
        )
        .expect("explicit settled boundary");
        assert_eq!(explicit.turns.len(), 1);
    }

    /// If the turn finishes after preparation read its row, the mixed
    /// running-row and terminal-event snapshot is refused. A fresh retry sees
    /// one finalized boundary and succeeds.
    #[test]
    fn retries_after_a_turn_finishes_during_preparation() {
        let session = session();
        let mut stale = turn(session.id, 1, "finish while forking");
        stale.status = TurnStatus::Running;
        let mixed = cut_at_settled_boundary(
            std::slice::from_ref(&stale),
            Some(TurnStatus::Completed),
            &HashSet::new(),
            false,
            None,
        );
        assert!(matches!(
            mixed,
            Err(ForkBoundaryError::Running { ordinal: 1 })
        ));

        let mut settled = stale;
        settled.status = TurnStatus::Completed;
        let settled_turns = [settled];
        let row_ahead = cut_at_settled_boundary(&settled_turns, None, &HashSet::new(), false, None);
        assert!(matches!(
            row_ahead,
            Err(ForkBoundaryError::Settling { ordinal: 1 })
        ));

        let retry = cut_at_settled_boundary(
            &settled_turns,
            Some(TurnStatus::Completed),
            &HashSet::new(),
            false,
            None,
        )
        .expect("fresh settled snapshot");
        assert_eq!(retry.turns.len(), 1);
    }

    /// The header names the fork point and the tail that ran after it: the
    /// worktree is shared, so the child must know the transcript is not the
    /// whole story of the files it sees.
    #[test]
    fn says_when_the_conversation_continued_past_the_fork_point() {
        let session = session();
        let turns: Vec<Turn> = (1..=2)
            .map(|ordinal| turn(session.id, ordinal, "work"))
            .collect();
        let records: HashSet<i64> = turns.iter().map(|turn| turn.ordinal).collect();
        let complete = all_turn_ids(&turns);

        let markdown = render_transcript_with_generation(
            Path::new("/private"),
            &session,
            &turns,
            3,
            &[],
            uuid::Uuid::nil(),
            &records,
            &HashSet::new(),
            &complete,
        )
        .markdown;

        assert!(markdown.contains("forked at the end of turn 2"));
        assert!(markdown.contains("continued for 3 more turns"));
        assert!(!markdown.contains("## Turn 3"));
    }

    /// Over budget, the oldest turns fall back to stubs that name their
    /// record files, and the header says how many were reduced. The child
    /// needs the end of the conversation in full, not the start of it.
    #[test]
    fn reduces_the_oldest_turns_to_stubs_and_says_so() {
        let session = session();
        let bulk = "x".repeat(200 * 1024);
        let turns: Vec<Turn> = (1..=5)
            .map(|ordinal| turn(session.id, ordinal, &bulk))
            .collect();

        let rendered = render_transcript(&session, &turns, &[]);
        assert!(rendered.truncated);
        assert_eq!(rendered.turns, 2);
        assert!(rendered.markdown.len() <= MAX_TRANSCRIPT_BYTES);
        assert!(rendered.markdown.contains("The 3 oldest turns appear"));
        assert!(rendered.markdown.contains("## Turn 5 — you"));
        assert!(!rendered.markdown.contains("## Turn 1 — you"));
        // The stub points the child at the full record it still has.
        assert!(rendered.markdown.contains("## Turn 1 (condensed)"));
        assert!(rendered.markdown.contains("`turn-0001.md`"));
    }

    /// A stub must not promise a record that was never written.
    #[test]
    fn a_stub_says_when_its_record_was_dropped() {
        let session = session();
        let bulk = "x".repeat(300 * 1024);
        let turns: Vec<Turn> = (1..=3)
            .map(|ordinal| turn(session.id, ordinal, &bulk))
            .collect();
        let records: HashSet<i64> = [3].into_iter().collect();
        let complete = all_turn_ids(&turns);

        let markdown = render_transcript_with_generation(
            Path::new("/private"),
            &session,
            &turns,
            0,
            &[],
            uuid::Uuid::nil(),
            &records,
            &HashSet::new(),
            &complete,
        )
        .markdown;

        assert!(markdown.contains("## Turn 1 (condensed)"));
        assert!(markdown.contains("No record file was retained"));
        assert!(!markdown.contains("`turn-0001.md`"));
    }

    /// The cap bounds the file, not the turns inside it, so the header has to
    /// come out of the same budget. Two turns that together sit just under it
    /// leave no room for the paragraph saying where the transcript came from.
    #[test]
    fn counts_the_header_against_the_cap() {
        let session = session();
        let bulk = "x".repeat(MAX_TRANSCRIPT_BYTES / 2 - 40);
        let turns: Vec<Turn> = (1..=2)
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

    /// The record keeps what the summary condenses away: tool output, the
    /// subagent's own calls and words, and the engine's reasoning.
    #[test]
    fn a_record_holds_previews_subagent_work_and_reasoning() {
        let session = session();
        let only = turn(session.id, 1, "audit the crate");
        let events = seq(vec![
            Event::TurnStarted { turn_id: only.id },
            Event::ReasoningDelta {
                text: "the tests look".to_owned(),
            },
            Event::ReasoningDelta {
                text: " flaky".to_owned(),
            },
            Event::ToolStarted {
                call_id: "task-1".to_owned(),
                name: "Task".to_owned(),
                detail: ToolDetail::Other {
                    summary: "audit".to_owned(),
                },
                parent_call_id: None,
            },
            Event::ToolStarted {
                call_id: "sub-1".to_owned(),
                name: "Bash".to_owned(),
                detail: ToolDetail::Command {
                    cmd: "cargo tree".to_owned(),
                    cwd: String::new(),
                },
                parent_call_id: Some("task-1".to_owned()),
            },
            Event::ToolCompleted {
                call_id: "sub-1".to_owned(),
                outcome: ToolOutcome::Succeeded,
                preview: "tidebreak-core v0.1.0".to_owned(),
                output: None,
                action: None,
                result: None,
                detail: None,
                parent_call_id: Some("task-1".to_owned()),
            },
            Event::AssistantMessage {
                text: "two crates look unused".to_owned(),
                parent_call_id: Some("task-1".to_owned()),
            },
            Event::ToolCompleted {
                call_id: "task-1".to_owned(),
                outcome: ToolOutcome::Succeeded,
                preview: "audit done".to_owned(),
                output: None,
                action: None,
                result: None,
                detail: None,
                parent_call_id: None,
            },
            Event::UserSteered {
                text: "skip the benches".to_owned(),
                message_id: None,
            },
        ]);

        let record = render_turn_record(
            Path::new("/private"),
            &session,
            &only,
            &events,
            "Claude Code",
            uuid::Uuid::nil(),
            false,
            true,
        );

        assert!(record.contains("# Turn 1 — full record"));
        assert!(record.contains("> _Thinking:_\n> the tests look flaky"));
        assert!(record.contains("  - `Bash` — cargo tree"));
        assert!(record.contains("tidebreak-core v0.1.0"));
        assert!(record.contains("  - The subagent said:\n    two crates look unused"));
        assert!(record.contains("audit done"));
        assert!(record.contains("**You, mid-turn:** skip the benches"));
    }

    /// A preview that carries its own fences must not break out of the block
    /// the record wraps it in.
    #[test]
    fn a_record_preview_cannot_escape_its_fence() {
        let session = session();
        let only = turn(session.id, 1, "show me");
        let events = seq(vec![
            Event::TurnStarted { turn_id: only.id },
            Event::ToolStarted {
                call_id: "call-1".to_owned(),
                name: "Read".to_owned(),
                detail: ToolDetail::Other {
                    summary: "README".to_owned(),
                },
                parent_call_id: None,
            },
            Event::ToolCompleted {
                call_id: "call-1".to_owned(),
                outcome: ToolOutcome::Succeeded,
                preview: "````\nfour ticks\n````".to_owned(),
                output: None,
                action: None,
                result: None,
                detail: None,
                parent_call_id: None,
            },
        ]);

        let record = render_turn_record(
            Path::new("/private"),
            &session,
            &only,
            &events,
            "Claude Code",
            uuid::Uuid::nil(),
            false,
            true,
        );

        let fence = "`````";
        let opens = record.matches(fence).count();
        assert_eq!(opens, 2, "the wrapping fence must outgrow the content");
    }

    /// A turn omitted from bounded replay still gets a request record that
    /// says why its engine events are missing.
    #[test]
    fn an_omitted_turn_record_says_why_its_events_are_missing() {
        let session = session();
        let old = turn(session.id, 1, "the first ask");
        let record = render_turn_record(
            Path::new("/private"),
            &session,
            &old,
            &[],
            "Claude Code",
            uuid::Uuid::nil(),
            false,
            false,
        );

        assert!(record.contains("the first ask"));
        assert!(record.contains("# Turn 1 — request record"));
        assert!(record.contains("omitted this turn's engine events as a whole"));
    }

    /// The condensed transcript must not make an omitted newest turn look as
    /// though the engine produced no work.
    #[test]
    fn marks_a_whole_turn_omitted_from_bounded_replay() {
        let session = session();
        let only = turn(session.id, 1, "inspect the failing run");

        let rendered = render_transcript_with_generation(
            Path::new("/private"),
            &session,
            std::slice::from_ref(&only),
            0,
            &[],
            uuid::Uuid::nil(),
            &HashSet::from([only.ordinal]),
            &HashSet::new(),
            &HashSet::new(),
        );

        assert_eq!(rendered.turns, 0);
        assert!(rendered.truncated);
        assert!(rendered
            .markdown
            .contains("omitted the engine events for 1 turn as a whole"));
        assert!(rendered
            .markdown
            .contains("This turn's engine events were omitted as a whole"));
    }

    /// Records are kept from the newest turn back within the total budget, so
    /// an enormous session loses its oldest records rather than its newest.
    #[test]
    fn drops_the_oldest_records_over_the_total_budget() {
        let session = session();
        let bulk = "x".repeat(MAX_RECORD_FILE_BYTES * 2);
        let turns: Vec<Turn> = (1..=40)
            .map(|ordinal| turn(session.id, ordinal, &bulk))
            .collect();
        let complete = all_turn_ids(&turns);

        let records = render_turn_records(
            Path::new("/private"),
            &session,
            &turns,
            &[],
            "Claude Code",
            uuid::Uuid::nil(),
            &HashSet::new(),
            &complete,
        );

        assert!(records.files.len() < turns.len());
        assert!(records.ordinals.contains(&40));
        assert!(!records.ordinals.contains(&1));
        let total: usize = records.files.iter().map(|(_, text)| text.len()).sum();
        assert!(total <= MAX_RECORD_TOTAL_BYTES);
        for (_, text) in &records.files {
            assert!(text.len() <= MAX_RECORD_FILE_BYTES);
        }
    }

    /// The child engine reads absolute paths outside every Git worktree: the
    /// condensed transcript, and one record per turn next to it.
    #[tokio::test]
    async fn writes_the_fork_directory_into_private_storage() {
        let private = tempfile::tempdir().expect("tempdir");
        let blob_root = tempfile::tempdir().expect("blob tempdir");
        let blobs = FsBlobStore::new(blob_root.path());
        let session = session();
        let turns = vec![
            turn(session.id, 1, "hello"),
            turn(session.id, 2, "and again"),
        ];
        let complete = all_turn_ids(&turns);
        let private_root = test_root(&private);

        let written = write_transcript(
            &private_root,
            &blobs,
            &session,
            ForkCut {
                turns: &turns,
                excluded: 0,
            },
            &[],
            &complete,
        )
        .await
        .expect("write");

        assert!(written.path.ends_with(SUMMARY_NAME));
        assert!(written.path.starts_with(&written.dir));
        assert_eq!(written.turns, 2);
        assert_eq!(written.total_turns, 2);
        assert_eq!(written.at_turn_ordinal, None);
        assert!(!written.truncated);
        let on_disk = std::fs::read_to_string(&written.path).expect("read");
        assert_eq!(written.byte_len, on_disk.len() as u64);
        assert!(on_disk.contains("hello"));
        let dir = Path::new(&written.dir);
        assert!(dir.join("turn-0001.md").is_file());
        assert!(dir.join("turn-0002.md").is_file());
    }

    /// A fork at an earlier turn keeps later turns — and their records — out
    /// of the handoff entirely, and reports the fork point.
    #[tokio::test]
    async fn a_fork_point_keeps_later_turns_out_of_the_directory() {
        let private = tempfile::tempdir().expect("tempdir");
        let blob_root = tempfile::tempdir().expect("blob tempdir");
        let blobs = FsBlobStore::new(blob_root.path());
        let session = session();
        let turns: Vec<Turn> = (1..=3)
            .map(|ordinal| turn(session.id, ordinal, "work"))
            .collect();
        let cut = cut_at(&turns, Some(turns[0].id)).expect("known turn");
        let complete = all_turn_ids(cut.turns);
        let private_root = test_root(&private);

        let written = write_transcript(&private_root, &blobs, &session, cut, &[], &complete)
            .await
            .expect("write");

        assert_eq!(written.turns, 1);
        assert_eq!(written.total_turns, 1);
        assert_eq!(written.at_turn_ordinal, Some(1));
        let dir = Path::new(&written.dir);
        assert!(dir.join("turn-0001.md").is_file());
        assert!(!dir.join("turn-0002.md").exists());
        let markdown = std::fs::read_to_string(&written.path).expect("read");
        assert!(markdown.contains("forked at the end of turn 1"));
    }

    /// A session can be forked again while the last child still reads its
    /// handoff. Generations are immutable: a re-fork writes a fresh directory
    /// and leaves every earlier one untouched.
    #[tokio::test]
    async fn a_refork_writes_a_new_generation_and_keeps_the_old() {
        let private = tempfile::tempdir().expect("tempdir");
        let blob_root = tempfile::tempdir().expect("blob tempdir");
        let blobs = FsBlobStore::new(blob_root.path());
        let session = session();
        let first_turns = vec![turn(session.id, 1, "hello")];
        let second_turns = vec![
            turn(session.id, 1, "hello"),
            turn(session.id, 2, "more work"),
        ];
        let first_complete = all_turn_ids(&first_turns);
        let second_complete = all_turn_ids(&second_turns);
        let private_root = test_root(&private);

        let first = write_transcript(
            &private_root,
            &blobs,
            &session,
            ForkCut {
                turns: &first_turns,
                excluded: 0,
            },
            &[],
            &first_complete,
        )
        .await
        .expect("first write");
        let second = write_transcript(
            &private_root,
            &blobs,
            &session,
            ForkCut {
                turns: &second_turns,
                excluded: 0,
            },
            &[],
            &second_complete,
        )
        .await
        .expect("second write");

        assert_ne!(first.dir, second.dir);
        let earlier = std::fs::read_to_string(&first.path).expect("earlier generation");
        assert!(earlier.contains("hello"));
        assert!(!earlier.contains("more work"));
        let session_dir = private.path().join(FORKS_DIR).join(session.id.to_string());
        assert_eq!(
            std::fs::read_dir(session_dir).expect("session dir").count(),
            2
        );
    }

    #[tokio::test]
    async fn materializes_images_for_an_ended_session_from_the_explicit_blob_store() {
        let private = tempfile::tempdir().unwrap();
        let blob_root = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::new(blob_root.path()));
        let mut session = session();
        session.lifecycle = SessionLifecycle::Ended;
        let mut only = turn(session.id, 1, "compare these screenshots");
        let first_bytes = b"\x89PNG\r\n\x1a\nfirst".to_vec();
        let second_bytes = b"GIF89asecond".to_vec();
        let first = ImageRef {
            blob_id: DocumentBlob::from_bytes(&first_bytes).id,
            media_type: ImageMediaType::Png,
            width: 1,
            height: 1,
            byte_len: first_bytes.len() as u64,
        };
        let second = ImageRef {
            blob_id: DocumentBlob::from_bytes(&second_bytes).id,
            media_type: ImageMediaType::Gif,
            width: 1,
            height: 1,
            byte_len: second_bytes.len() as u64,
        };
        blobs.put(first.blob_id, first_bytes.clone()).await.unwrap();
        blobs
            .put(second.blob_id, second_bytes.clone())
            .await
            .unwrap();
        only.attachments = vec![first, second];
        let turns = vec![only];
        let complete = all_turn_ids(&turns);
        let private_root = test_root(&private);

        let written = write_transcript(
            &private_root,
            blobs.as_ref(),
            &session,
            ForkCut {
                turns: &turns,
                excluded: 0,
            },
            &[],
            &complete,
        )
        .await
        .unwrap();

        let markdown = std::fs::read_to_string(written.path).unwrap();
        let paths: Vec<&str> = markdown
            .lines()
            .filter_map(|line| line.strip_prefix("- `")?.strip_suffix('`'))
            .collect();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].contains("turn-1-1-"));
        assert!(paths[1].contains("turn-1-2-"));
        assert_eq!(std::fs::read(paths[0]).unwrap(), first_bytes);
        assert_eq!(std::fs::read(paths[1]).unwrap(), second_bytes);
        assert!(!private.path().join("attachments").exists());
    }

    /// Images over the retention cap no longer fail the fork: the newest
    /// turns keep their bytes, older ones lose them, and both layers say so
    /// where the image would have been listed.
    #[tokio::test]
    async fn drops_the_oldest_images_over_the_retention_cap() {
        let private = tempfile::tempdir().unwrap();
        let blob_root = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::new(blob_root.path()));
        let session = session();
        let bytes = b"\x89PNG\r\n\x1a\nimage".to_vec();
        let real = ImageRef {
            blob_id: DocumentBlob::from_bytes(&bytes).id,
            media_type: ImageMediaType::Png,
            width: 1,
            height: 1,
            byte_len: bytes.len() as u64,
        };
        blobs.put(real.blob_id, bytes.clone()).await.unwrap();
        // The old turn's image claims (nearly) the whole budget, so retaining
        // the newest turn leaves no room for it.
        let huge = ImageRef {
            blob_id: DocumentBlob::from_bytes(b"never read").id,
            media_type: ImageMediaType::Png,
            width: 1,
            height: 1,
            byte_len: MAX_FORK_ATTACHMENT_BYTES,
        };
        let mut old = turn(session.id, 1, "look at this");
        old.attachments = vec![huge];
        let mut new = turn(session.id, 2, "and this");
        new.attachments = vec![real];
        let turns = vec![old, new];
        let complete = all_turn_ids(&turns);
        let private_root = test_root(&private);

        let written = write_transcript(
            &private_root,
            blobs.as_ref(),
            &session,
            ForkCut {
                turns: &turns,
                excluded: 0,
            },
            &[],
            &complete,
        )
        .await
        .unwrap();

        let markdown = std::fs::read_to_string(&written.path).unwrap();
        assert!(markdown.contains("were not retained"));
        let retained: Vec<&str> = markdown
            .lines()
            .filter_map(|line| line.strip_prefix("- `")?.strip_suffix('`'))
            .collect();
        assert_eq!(retained.len(), 1);
        assert_eq!(std::fs::read(retained[0]).unwrap(), bytes);
    }
}
