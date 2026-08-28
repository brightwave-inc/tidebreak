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
//! journal commit: a crash between the two leaves attention one event stale,
//! which the next batch corrects, never a journal gap.

use std::sync::Arc;

use serde_json::Value;
use tracing::warn;

use tidebreak_core::db::code::{
    ingest_incarnation_event, latest_incarnation, mark_incarnation_terminal_events_journaled,
    record_incarnation_task_output,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, BoundedError, CodeEvent, CodeIncarnationId,
    CodeSessionId, CodeTurnId, CodeUsage, DbStore, FenceReason, HarnessKind, HarnessNoticeLevel,
    OwnerId, SequencedCodeEvent, MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS,
};

use super::super::bus::CodeEventBus;
use super::wire::{SandboxEvent, SandboxEvents, SandboxState};

/// The session-side identity one cursor read is ingested under.
#[derive(Clone, Debug)]
pub(crate) struct IngestBinding {
    /// Owner of the session and the incarnation.
    pub owner: OwnerId,
    /// The remote session being projected into.
    pub session_id: CodeSessionId,
    /// The session's spawn epoch, fencing a superseded worker's writes.
    pub spawn_epoch: i64,
    /// The incarnation whose sandbox produced this stream.
    pub incarnation: CodeIncarnationId,
    /// The session's engine, for the started marker.
    pub harness_kind: HarnessKind,
    /// The turn the driver is servicing, when one is running.
    pub turn_id: Option<CodeTurnId>,
}

/// What one sandbox event becomes on the session.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Projection {
    /// Journal rows to append, in order.
    pub journal: Vec<CodeEvent>,
    /// Attention to apply after the journal commit.
    pub attention: Option<Attention>,
    /// The terminal deliverable to retain on the incarnation.
    pub task_output: Option<String>,
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
    /// Whether the supervisor's own stop marker has been journaled, in this
    /// batch or an earlier one.
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

fn notice(level: HarnessNoticeLevel, message: String) -> CodeEvent {
    CodeEvent::HarnessNotice {
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
pub(crate) fn project_event(binding: &IngestBinding, kind: &str, payload: &Value) -> Projection {
    let mut out = Projection::default();
    match kind {
        "supervisor_started" => {
            out.journal.push(CodeEvent::SessionStarted {
                harness_kind: binding.harness_kind,
                harness_version: payload_str(payload, "agent").unwrap_or("remote").to_owned(),
                resume_ref: None,
            });
        }
        "turn_started" => {
            if let Some(turn_id) = binding.turn_id {
                out.journal.push(CodeEvent::TurnStarted { turn_id });
            }
            out.attention = Some(Attention::working(AttentionSource::Lifecycle));
        }
        "assistant_record" => {
            let body = payload_str(payload, "body").unwrap_or_default();
            if !body.is_empty() {
                out.journal.push(CodeEvent::AssistantMessage {
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
                out.journal.push(CodeEvent::TurnCompleted {
                    usage: CodeUsage::default(),
                    checkpoint: None,
                });
                out.attention = Some(Attention::new(
                    AttentionState::DoneUnreviewed,
                    AttentionSource::Lifecycle,
                ));
            } else {
                out.journal.push(CodeEvent::TurnFailed {
                    error: BoundedError {
                        message: "the engine turn failed".to_owned(),
                    },
                });
                out.attention = Some(Attention::needs_you(
                    "the engine turn failed",
                    AttentionSource::Lifecycle,
                ));
            }
        }
        "turn_interrupted" => {
            out.journal.push(CodeEvent::TurnInterrupted);
            out.attention = Some(Attention::needs_you(
                "the turn was interrupted",
                AttentionSource::Lifecycle,
            ));
        }
        "wip_pushed" => {
            let reference = payload_str(payload, "reference").unwrap_or("a WIP ref");
            out.journal.push(notice(
                HarnessNoticeLevel::Info,
                format!("Work in progress was checkpointed to {reference}."),
            ));
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
    bus: &CodeEventBus,
    binding: &IngestBinding,
    read: &SandboxEvents,
) -> Result<IngestOutcome, tidebreak_core::AgentError> {
    let mut outcome = IngestOutcome::default();
    for event in &read.events {
        outcome.ingested += u64::from(apply_one(db, bus, binding, event, &mut outcome).await?);
    }
    let drained = read
        .events
        .last()
        .is_none_or(|event| event.seq >= read.latest_event_seq);
    if read.state.is_terminal() && drained && !outcome.terminal_flush_journaled {
        // The goodbye may have been journaled by an earlier batch; the
        // incarnation row remembers across restarts.
        let already = latest_incarnation(db, &binding.owner, binding.session_id)
            .await?
            .is_some_and(|row| row.id == binding.incarnation && row.terminal_events_journaled);
        outcome.terminal_flush_journaled = already;
        if !already {
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
    bus: &CodeEventBus,
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
    if projection.terminal_flush {
        outcome.terminal_flush_journaled = true;
    }
    let Some(seqs) = ingest_incarnation_event(
        db,
        &binding.owner,
        binding.session_id,
        binding.spawn_epoch,
        binding.incarnation,
        event.seq,
        &projection.journal,
    )
    .await?
    else {
        // The cursor already covers this sequence: a replay after a restart.
        // Its side effects committed with it the first time.
        return Ok(false);
    };
    for (seq, journal_event) in seqs.into_iter().zip(projection.journal) {
        bus.publish(
            binding.session_id,
            SequencedCodeEvent {
                seq,
                event: journal_event,
            },
        );
    }
    if let Some(body) = &projection.task_output {
        record_incarnation_task_output(db, &binding.owner, binding.incarnation, body).await?;
    }
    if projection.terminal_flush {
        mark_incarnation_terminal_events_journaled(db, &binding.owner, binding.incarnation).await?;
    }
    if let Some(next) = projection.attention {
        let _ = super::super::attention::apply_attention(
            db,
            bus,
            &binding.owner,
            binding.session_id,
            next,
            false,
        )
        .await?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use tidebreak_core::db::code::{
        activate_incarnation, create_incarnation_intent, get_session, insert_repo, insert_session,
        insert_workspace, latest_incarnation, list_events,
    };
    use tidebreak_core::{
        CodeRepo, CodeSession, CodeSessionKind, CodeSessionLifecycle, CodeWorkspace,
        CodeWorkspaceStatus, IncarnationAdmission, PermissionMode, RepoId, WorkspaceId,
    };

    use super::*;

    fn binding(session: &CodeSession, incarnation: CodeIncarnationId) -> IngestBinding {
        IngestBinding {
            owner: session.owner.clone(),
            session_id: session.id,
            spawn_epoch: session.spawn_epoch,
            incarnation,
            harness_kind: session.harness_kind,
            turn_id: Some(CodeTurnId::new()),
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
        assert!(matches!(started.journal[0], CodeEvent::TurnStarted { .. }));
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
            CodeEvent::AssistantMessage { text, parent_call_id: None } if text == "the answer"
        ));
        assert!(matches!(
            &record.journal[1],
            CodeEvent::HarnessNotice {
                level: HarnessNoticeLevel::Warning,
                ..
            }
        ));

        let failed = project_event(&b, "turn_completed", &json!({ "turn": 3, "exit_code": 1 }));
        assert!(matches!(&failed.journal[0], CodeEvent::TurnFailed { .. }));
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

        // Environment lifecycle events are recognized, not vocabulary drift.
        assert!(!project_event(&b, "running", &json!({})).unrecognized);
        assert!(project_event(&b, "brand_new_kind", &json!({})).unrecognized);
    }

    /// A driven stream produces the journal rows and attention transitions,
    /// and a restart that re-reads overlapping sequences duplicates nothing.
    #[tokio::test]
    async fn a_driven_stream_survives_a_restart_mid_stream() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, session) = seed(dir.path()).await;
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
        // 1..3 replay and write nothing; 4 and 5 land once.
        let second = read(
            SandboxState::Completed,
            5,
            vec![
                event(2, "turn_started", json!({ "turn": 1 })),
                event(3, "assistant_record", json!({ "body": "hello" })),
                event(4, "turn_completed", json!({ "turn": 1, "exit_code": 0 })),
                event(
                    5,
                    "supervisor_stopped",
                    json!({ "reason": "task_complete" }),
                ),
            ],
        );
        let outcome = ingest_events(&db, &bus, &b, &second).await.unwrap();
        assert_eq!(outcome.ingested, 2);
        assert!(outcome.terminal_flush_journaled);
        assert!(outcome.fence.is_none());

        let page = list_events(&db, &b.owner, b.session_id, 0, 50)
            .await
            .unwrap();
        let kinds: Vec<&'static str> = page
            .events
            .iter()
            .map(|row| match &row.event {
                CodeEvent::SessionStarted { .. } => "session_started",
                CodeEvent::TurnStarted { .. } => "turn_started",
                CodeEvent::AssistantMessage { .. } => "assistant_message",
                CodeEvent::TurnCompleted { .. } => "turn_completed",
                CodeEvent::HarnessNotice { .. } => "notice",
                other => panic!("unexpected journal row {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "session_started",
                "turn_started",
                "assistant_message",
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
        assert_eq!(row.events_cursor, 5);
    }

    /// A terminal environment state without the supervisor's goodbye demands
    /// a fence: lost for a violent end, a missing flush for a clean one.
    #[tokio::test]
    async fn a_terminal_state_without_a_goodbye_demands_a_fence() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, session) = seed(dir.path()).await;
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

    fn session_value() -> CodeSession {
        CodeSession {
            id: CodeSessionId::new(),
            owner: OwnerId::local(),
            workspace_id: WorkspaceId::new(),
            kind: CodeSessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: PermissionMode::Allow,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: CodeSessionLifecycle::Running,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(tidebreak_core::AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: chrono::Utc::now(),
        }
    }

    async fn seed(root: &Path) -> (Arc<DbStore>, CodeEventBus, CodeSession) {
        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                root.join("code.db").display()
            ))
            .await
            .unwrap(),
        );
        let owner = OwnerId::local();
        let repo_id = RepoId::new();
        insert_repo(
            &db,
            &CodeRepo {
                id: repo_id,
                owner: owner.clone(),
                root_path: root.join("repo").display().to_string(),
                display_name: "example".into(),
                default_base_ref: "main".into(),
                branch_prefix: "tidebreak/".into(),
                setup_script: None,
                archive_script: None,
                quick_actions: Vec::new(),
                created_at: chrono::Utc::now(),
                removed_at: None,
                cloned_from: None,
                origin_host: None,
                origin_owner: None,
                origin_name: None,
            },
        )
        .await
        .unwrap();
        let workspace_id = WorkspaceId::new();
        insert_workspace(
            &db,
            &CodeWorkspace {
                id: workspace_id,
                owner: owner.clone(),
                repo_id,
                title: "remote".into(),
                worktree_path: root.join("tree").display().to_string(),
                branch_name: "tidebreak/remote".into(),
                base_ref: "main".into(),
                status: CodeWorkspaceStatus::Active,
                pr: None,
                created_at: chrono::Utc::now(),
                archived_at: None,
                released_at: None,
                released_tip: None,
                bundle_bytes: None,
            },
        )
        .await
        .unwrap();
        let mut session = session_value();
        session.workspace_id = workspace_id;
        insert_session(&db, &session).await.unwrap();
        (db, CodeEventBus::default(), session)
    }

    async fn seeded_incarnation(db: &Arc<DbStore>, session: &CodeSession) -> CodeIncarnationId {
        let admission = create_incarnation_intent(db, &session.owner, session.id, 1, 4)
            .await
            .unwrap();
        let IncarnationAdmission::Admitted(row) = admission else {
            panic!("expected admission");
        };
        activate_incarnation(db, &session.owner, row.id, "sb-1")
            .await
            .unwrap();
        row.id
    }
}
