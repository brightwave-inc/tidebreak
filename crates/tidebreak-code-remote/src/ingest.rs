//! Project a sandbox's durable event stream into the session journal and
//! attention.
//!
//! The supervised agent reports lifecycle events to its environment, and the
//! environment persists them as a gap-free per-sandbox sequence. This module
//! turns each of those events into the journal rows and attention transitions
//! a first-class session shows, and it advances a durable per-incarnation
//! cursor in the same transaction as the journal write — so a server restart
//! resumes ingestion exactly where it stopped, replaying nothing and losing
//! nothing.
//!
//! The projection itself is pure ([`project_event`]); [`ingest_events`]
//! applies one cursor read. Attention and the bus publish ride after the
//! journal commit: a crash between the two leaves attention stale until the
//! next read replays the sequence, and attention — unlike the journal — is
//! re-applied on replay, so the replay is what corrects it.

use std::sync::Arc;

use serde_json::Value;
use tracing::warn;

use tidebreak_core::code::SequencedEvent;
use tidebreak_core::db::code::{
    ingest_incarnation_event, latest_incarnation, IncarnationSideEffects,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, BoundedError, CodeIncarnationId, DbStore, Event,
    FenceReason, HarnessKind, HarnessNoticeLevel, OwnerId, SessionId, TurnId, TurnUsage,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS,
};

use super::wire::{SandboxEvent, SandboxEvents, SandboxState};
use super::{apply_attention, RemoteSessionHost};

/// The session-side identity one cursor read is ingested under.
#[derive(Clone, Debug)]
pub(crate) struct IngestBinding {
    /// Owner of the session and the incarnation.
    pub owner: OwnerId,
    /// The remote session being projected into.
    pub session_id: SessionId,
    /// The session's spawn epoch, fencing a superseded worker's writes.
    pub spawn_epoch: i64,
    /// The incarnation whose sandbox produced this stream.
    pub incarnation: CodeIncarnationId,
    /// The session's engine, for the started marker.
    pub harness_kind: HarnessKind,
    /// The turn the driver is servicing, when one is running.
    pub turn_id: Option<TurnId>,
}

/// What one sandbox event becomes on the session.
#[derive(Debug, Default, PartialEq)]
struct Projection {
    /// Journal rows to append, in order.
    pub journal: Vec<Event>,
    /// Attention to apply after the journal commit.
    pub attention: Option<Attention>,
    /// The terminal deliverable to retain on the incarnation.
    pub task_output: Option<String>,
    /// The WIP checkpoint ref to retain on the incarnation, for resume.
    pub wip_ref: Option<String>,
    /// Whether this event closes the incarnation's stream: the supervisor
    /// said goodbye, so its terminal events are now journaled.
    pub terminal_flush: bool,
    /// Whether the event kind was not recognized.
    pub unrecognized: bool,
}

/// What one cursor read left behind.
#[derive(Debug, Default)]
pub(crate) struct IngestOutcome {
    /// Sandbox event sequences whose projection committed in this call.
    pub ingested: u64,
    /// Whether the supervisor's own stop marker has been journaled — read
    /// from the durable incarnation row after the batch, never inferred
    /// from a replayed projection.
    pub terminal_flush_journaled: bool,
    /// The fence this read demands, when the environment reports the sandbox
    /// gone without the supervisor having said goodbye.
    pub fence: Option<FenceReason>,
    /// Event kinds this server does not recognize.
    pub unrecognized: u64,
}

fn bound(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars).collect()
}

fn notice(level: HarnessNoticeLevel, message: String) -> Event {
    Event::HarnessNotice {
        level,
        message: bound(&message, MAX_NOTICE_CHARS),
    }
}

fn payload_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

/// The journal rows and attention one sandbox event implies.
///
/// Pure so the mapping is testable without a database. Lifecycle states the
/// environment appends under their own name (`running`, `failed`, ...) are
/// recognized but journal nothing here; [`ingest_events`] reads the terminal
/// ones off the cursor read's own `state` field instead, where they are
/// authoritative.
fn project_event(binding: &IngestBinding, kind: &str, payload: &Value) -> Projection {
    let mut out = Projection::default();
    match kind {
        "supervisor_started" => {
            out.journal.push(Event::SessionStarted {
                harness_kind: binding.harness_kind,
                harness_version: payload_str(payload, "agent").unwrap_or("remote").to_owned(),
                resume_ref: None,
            });
        }
        "turn_started" => {
            if let Some(turn_id) = binding.turn_id {
                out.journal.push(Event::TurnStarted { turn_id });
            }
            out.attention = Some(Attention::working(AttentionSource::Lifecycle));
        }
        "assistant_record" => {
            let body = payload_str(payload, "body").unwrap_or_default();
            if !body.is_empty() {
                out.journal.push(Event::AssistantMessage {
                    text: bound(body, MAX_EVENT_TEXT_CHARS),
                    parent_call_id: None,
                });
            }
            let cut_here = body.chars().count() > MAX_EVENT_TEXT_CHARS;
            if cut_here || payload.get("truncated").and_then(Value::as_bool) == Some(true) {
                out.journal.push(notice(
                    HarnessNoticeLevel::Warning,
                    "The engine's answer was truncated; the full text stayed in the sandbox."
                        .to_owned(),
                ));
            }
        }
        "turn_completed" => {
            let success = payload.get("exit_code").and_then(Value::as_i64) == Some(0);
            if success {
                out.journal.push(Event::TurnCompleted {
                    usage: TurnUsage::default(),
                    checkpoint: None,
                    stop_reason: None,
                });
                out.attention = Some(Attention::new(
                    AttentionState::DoneUnreviewed,
                    AttentionSource::Lifecycle,
                ));
            } else {
                out.journal.push(Event::TurnFailed {
                    error: BoundedError {
                        message: "the engine turn failed".to_owned(),
                    },
                    detail: None,
                });
                out.attention = Some(Attention::needs_you(
                    "the engine turn failed",
                    AttentionSource::Lifecycle,
                ));
            }
        }
        "turn_interrupted" => {
            out.journal.push(Event::TurnInterrupted { usage: None });
            out.attention = Some(Attention::needs_you(
                "the turn was interrupted",
                AttentionSource::Lifecycle,
            ));
        }
        "wip_pushed" => {
            let reference = payload_str(payload, "ref");
            out.journal.push(notice(
                HarnessNoticeLevel::Info,
                format!(
                    "Work in progress was checkpointed to {}.",
                    reference.unwrap_or("a WIP ref")
                ),
            ));
            out.wip_ref = reference.map(str::to_owned);
        }
        "wip_push_failed" | "wip_push_unavailable" => {
            let reason = payload_str(payload, "reason").unwrap_or(kind);
            out.journal.push(notice(
                HarnessNoticeLevel::Warning,
                format!("The sandbox could not checkpoint its work ({reason})."),
            ));
        }
        "task_complete" => {
            out.journal.push(notice(
                HarnessNoticeLevel::Info,
                "The remote task reported itself complete.".to_owned(),
            ));
        }
        "task_output" => {
            out.task_output = payload_str(payload, "body").map(str::to_owned);
        }
        "supervisor_stopped" => {
            let reason = payload_str(payload, "reason").unwrap_or("stopped");
            out.journal.push(notice(
                HarnessNoticeLevel::Info,
                format!("The remote supervisor stopped ({reason})."),
            ));
            out.terminal_flush = true;
        }
        "pod_lost" => {
            out.journal.push(notice(
                HarnessNoticeLevel::Warning,
                "The environment lost the sandbox's pod.".to_owned(),
            ));
        }
        other => {
            // Environment lifecycle events are named after the state itself;
            // recognize them so they are not counted as vocabulary drift.
            let known_lifecycle =
                serde_json::from_value::<SandboxState>(Value::String(other.to_owned()))
                    .map(|state| state != SandboxState::Unknown)
                    .unwrap_or(false);
            let known_pod_story = matches!(other, "pod_provisioned" | "pod_terminal_condition")
                || other.starts_with("supervisor_poll");
            out.unrecognized = !known_lifecycle && !known_pod_story;
        }
    }
    out
}

/// Ingest one cursor read: journal, attention, cursor, retention.
///
/// Each sandbox event commits its journal rows and the cursor advance in one
/// transaction, so an event is projected exactly once across restarts. When
/// the read's own state says the environment will run the sandbox no
/// further and the supervisor never said goodbye, the outcome carries the
/// [`FenceReason::SandboxLost`] the caller must apply — deciding what a
/// missing goodbye means for the session is the driver's call, not a
/// projection.
pub(crate) async fn ingest_events(
    db: &Arc<DbStore>,
    bus: &dyn RemoteSessionHost,
    binding: &IngestBinding,
    read: &SandboxEvents,
) -> Result<IngestOutcome, tidebreak_core::AgentError> {
    let mut outcome = IngestOutcome::default();
    for event in &read.events {
        outcome.ingested += u64::from(apply_one(db, bus, binding, event, &mut outcome).await?);
    }
    // The durable row is the truth about the goodbye — the batch that
    // carried it may have committed before a restart, and a replayed
    // sequence's projection must not claim a gate the row does not show.
    outcome.terminal_flush_journaled = latest_incarnation(db, &binding.owner, binding.session_id)
        .await?
        .is_some_and(|row| row.id == binding.incarnation && row.terminal_events_journaled);
    let drained = read
        .events
        .last()
        .is_none_or(|event| event.seq >= read.latest_event_seq);
    if read.state.is_terminal() && drained && !outcome.terminal_flush_journaled {
        {
            outcome.fence = Some(if read.state == SandboxState::Completed {
                // The run ended the way the environment expected, but its last
                // events never reached this journal: a resume would run without
                // its predecessor's final output.
                FenceReason::TerminalFlushMissing {
                    detail:
                        "The sandbox completed but its terminal events never reached the journal."
                            .to_owned(),
                }
            } else {
                FenceReason::SandboxLost {
                    detail: format!(
                        "The environment reports the sandbox {} without the supervisor finishing.",
                        terminal_state_words(read.state)
                    ),
                }
            });
        }
    }
    Ok(outcome)
}

fn terminal_state_words(state: SandboxState) -> &'static str {
    match state {
        SandboxState::Failed => "failed",
        SandboxState::Cancelled => "cancelled",
        SandboxState::Expired => "expired",
        SandboxState::CeilingExceeded => "stopped at its spend ceiling",
        _ => "gone",
    }
}

/// Returns whether the event was ingested now (false when the cursor had it).
async fn apply_one(
    db: &Arc<DbStore>,
    bus: &dyn RemoteSessionHost,
    binding: &IngestBinding,
    event: &SandboxEvent,
    outcome: &mut IngestOutcome,
) -> Result<bool, tidebreak_core::AgentError> {
    let projection = project_event(binding, &event.kind, &event.payload);
    if projection.unrecognized {
        outcome.unrecognized += 1;
        warn!(
            session = %binding.session_id,
            kind = %event.kind,
            "a sandbox event kind this server does not recognize was skipped"
        );
    }
    let ingested = ingest_incarnation_event(
        db,
        &binding.owner,
        binding.session_id,
        binding.spawn_epoch,
        binding.incarnation,
        event.seq,
        IncarnationSideEffects {
            journal: &projection.journal,
            task_output: projection.task_output.as_deref(),
            wip_ref: projection.wip_ref.as_deref(),
            terminal_events_journaled: projection.terminal_flush,
        },
    )
    .await?;
    if let Some(seqs) = &ingested {
        for (seq, journal_event) in seqs.iter().zip(&projection.journal) {
            bus.publish(
                binding.session_id,
                SequencedEvent {
                    seq: *seq,
                    event: journal_event.clone(),
                },
            );
        }
    }
    // Attention is applied even for a replayed sequence: the journal write
    // and the attention write are separate transactions, so a crash between
    // them leaves attention stale, and the replay is what corrects it.
    // Re-applying an attention the session already holds is a no-op.
    if let Some(next) = projection.attention {
        apply_attention(db, bus, &binding.owner, binding.session_id, next).await?;
    }
    Ok(ingested.is_some())
}

#[cfg(test)]
mod tests {

    use serde_json::json;

    use tidebreak_core::db::code::{
        get_session, latest_incarnation, list_events, replace_session_attention,
    };
    use tidebreak_core::Session;

    use super::super::fixtures::{seed, seeded_incarnation, session_value};
    use super::*;

    fn binding(session: &Session, incarnation: CodeIncarnationId) -> IngestBinding {
        IngestBinding {
            owner: session.owner.clone(),
            session_id: session.id,
            spawn_epoch: session.spawn_epoch,
            incarnation,
            harness_kind: session.harness_kind,
            turn_id: Some(TurnId::new()),
        }
    }

    fn event(seq: i64, kind: &str, payload: Value) -> SandboxEvent {
        SandboxEvent {
            seq,
            kind: kind.to_owned(),
            payload,
            created_at: String::new(),
        }
    }

    fn read(state: SandboxState, latest: i64, events: Vec<SandboxEvent>) -> SandboxEvents {
        SandboxEvents {
            sandbox_id: "sb-1".to_owned(),
            state,
            latest_event_seq: latest,
            events,
        }
    }

    #[test]
    fn the_projection_maps_the_supervisor_vocabulary() {
        let session = session_value();
        let b = binding(&session, CodeIncarnationId::new());

        let started = project_event(&b, "turn_started", &json!({ "turn": 3 }));
        assert!(matches!(started.journal[0], Event::TurnStarted { .. }));
        assert_eq!(
            started.attention,
            Some(Attention::working(AttentionSource::Lifecycle))
        );

        let record = project_event(
            &b,
            "assistant_record",
            &json!({ "body": "the answer", "truncated": true }),
        );
        assert!(matches!(
            &record.journal[0],
            Event::AssistantMessage { text, parent_call_id: None } if text == "the answer"
        ));
        assert!(matches!(
            &record.journal[1],
            Event::HarnessNotice {
                level: HarnessNoticeLevel::Warning,
                ..
            }
        ));

        let failed = project_event(&b, "turn_completed", &json!({ "turn": 3, "exit_code": 1 }));
        assert!(matches!(&failed.journal[0], Event::TurnFailed { .. }));
        assert!(matches!(
            failed.attention,
            Some(Attention {
                state: AttentionState::NeedsYou { .. },
                ..
            })
        ));

        let output = project_event(&b, "task_output", &json!({ "body": "findings" }));
        assert_eq!(output.task_output.as_deref(), Some("findings"));
        assert!(output.journal.is_empty());

        let stopped = project_event(&b, "supervisor_stopped", &json!({ "reason": "stopped" }));
        assert!(stopped.terminal_flush);

        let wip = project_event(&b, "wip_pushed", &json!({ "ref": "mg-wip/sb-1-i1" }));
        assert_eq!(wip.wip_ref.as_deref(), Some("mg-wip/sb-1-i1"));

        // Environment lifecycle events are recognized, not vocabulary drift.
        assert!(!project_event(&b, "running", &json!({})).unrecognized);
        assert!(project_event(&b, "brand_new_kind", &json!({})).unrecognized);
    }

    /// A driven stream produces the journal rows and attention transitions,
    /// and a restart that re-reads overlapping sequences duplicates nothing.
    #[tokio::test]
    async fn a_driven_stream_survives_a_restart_mid_stream() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, session, _workspace, _repo) = seed(dir.path()).await;
        let incarnation = seeded_incarnation(&db, &session).await;
        let b = binding(&session, incarnation);

        let first = read(
            SandboxState::Running,
            3,
            vec![
                event(1, "supervisor_started", json!({ "agent": "tidebreak" })),
                event(2, "turn_started", json!({ "turn": 1 })),
                event(3, "assistant_record", json!({ "body": "hello" })),
            ],
        );
        let outcome = ingest_events(&db, &bus, &b, &first).await.unwrap();
        assert_eq!(outcome.ingested, 3);
        assert!(outcome.fence.is_none());
        let live = get_session(&db, &b.owner, b.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.attention.state, AttentionState::Working);

        // The server restarts and re-reads from an older cursor: sequences
        // 1..3 replay and write nothing; 4 through 6 land once.
        let second = read(
            SandboxState::Completed,
            6,
            vec![
                event(2, "turn_started", json!({ "turn": 1 })),
                event(3, "assistant_record", json!({ "body": "hello" })),
                event(4, "wip_pushed", json!({ "ref": "mg-wip/sb-1-i1" })),
                event(5, "turn_completed", json!({ "turn": 1, "exit_code": 0 })),
                event(
                    6,
                    "supervisor_stopped",
                    json!({ "reason": "task_complete" }),
                ),
            ],
        );
        let outcome = ingest_events(&db, &bus, &b, &second).await.unwrap();
        assert_eq!(outcome.ingested, 3);
        assert!(outcome.terminal_flush_journaled);
        assert!(outcome.fence.is_none());

        let page = list_events(&db, &b.owner, b.session_id, 0, 50)
            .await
            .unwrap();
        let kinds: Vec<&'static str> = page
            .events
            .iter()
            .map(|row| match &row.event {
                Event::SessionStarted { .. } => "session_started",
                Event::TurnStarted { .. } => "turn_started",
                Event::AssistantMessage { .. } => "assistant_message",
                Event::TurnCompleted { .. } => "turn_completed",
                Event::HarnessNotice { .. } => "notice",
                other => panic!("unexpected journal row {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "session_started",
                "turn_started",
                "assistant_message",
                "notice",
                "turn_completed",
                "notice",
            ],
        );

        let live = get_session(&db, &b.owner, b.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.attention.state, AttentionState::DoneUnreviewed);
        let row = latest_incarnation(&db, &b.owner, b.session_id)
            .await
            .unwrap()
            .unwrap();
        assert!(row.terminal_events_journaled);
        assert_eq!(row.events_cursor, 6);
        assert_eq!(row.last_wip_ref.as_deref(), Some("mg-wip/sb-1-i1"));

        // A crash can land between the journal commit and the attention
        // write. Simulate the stale half: force the session back to Working,
        // then replay the whole batch — the journal takes nothing twice, and
        // the replayed attention corrects the session.
        replace_session_attention(
            &db,
            &b.owner,
            b.session_id,
            &Attention::working(AttentionSource::Lifecycle),
            false,
        )
        .await
        .unwrap();
        let outcome = ingest_events(&db, &bus, &b, &second).await.unwrap();
        assert_eq!(outcome.ingested, 0);
        assert!(outcome.terminal_flush_journaled);
        assert!(outcome.fence.is_none());
        let page = list_events(&db, &b.owner, b.session_id, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 6);
        let live = get_session(&db, &b.owner, b.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.attention.state, AttentionState::DoneUnreviewed);
    }

    /// A terminal environment state without the supervisor's goodbye demands
    /// a fence: lost for a violent end, a missing flush for a clean one.
    #[tokio::test]
    async fn a_terminal_state_without_a_goodbye_demands_a_fence() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, session, _workspace, _repo) = seed(dir.path()).await;
        let incarnation = seeded_incarnation(&db, &session).await;
        let b = binding(&session, incarnation);

        let lost = read(
            SandboxState::Failed,
            1,
            vec![event(1, "turn_started", json!({ "turn": 1 }))],
        );
        let outcome = ingest_events(&db, &bus, &b, &lost).await.unwrap();
        assert!(matches!(
            outcome.fence,
            Some(FenceReason::SandboxLost { .. })
        ));

        let silent_completion = read(SandboxState::Completed, 1, vec![]);
        let outcome = ingest_events(&db, &bus, &b, &silent_completion)
            .await
            .unwrap();
        assert!(matches!(
            outcome.fence,
            Some(FenceReason::TerminalFlushMissing { .. })
        ));

        // A terminal state with events still unread past this page waits for
        // the drain instead of fencing early.
        let undrained = read(
            SandboxState::Completed,
            9,
            vec![event(2, "running", json!({}))],
        );
        let outcome = ingest_events(&db, &bus, &b, &undrained).await.unwrap();
        assert!(outcome.fence.is_none());
    }
}
