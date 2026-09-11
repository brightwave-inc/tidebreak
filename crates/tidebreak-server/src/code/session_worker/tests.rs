use super::*;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use chrono::Utc;
use tidebreak_core::db::code::{
    enqueue_queued_turn, get_session, insert_repo, insert_session, insert_workspace,
    list_approvals, list_events, list_turns, replace_session_attention,
    replace_session_execution_settings, MAX_REPLAY_EVENTS,
};
use tidebreak_core::{
    AttentionState, CodeRepo, CodeWorkspace, CodeWorkspaceStatus, HarnessKind, ImageMediaType,
    ImageRef, PermissionMode, ReasoningEffort, RepoId, SessionKind, ToolDetail, TurnUsage,
    WorkspaceId,
};
use tidebreak_harness::{HarnessAdapter as _, SessionSpec};

fn subagent(call_id: &str, status: CodeSubagentStatus) -> CodeSubagentSummary {
    CodeSubagentSummary {
        call_id: call_id.into(),
        name: call_id.into(),
        status,
    }
}

#[test]
fn parent_boundaries_settle_only_running_subagents() {
    let mut completed = vec![
        subagent("running", CodeSubagentStatus::Running),
        subagent("done", CodeSubagentStatus::Done),
        subagent("failed", CodeSubagentStatus::Failed),
    ];
    assert!(settle_running_subagents(
        &mut completed,
        CodeSubagentStatus::Done
    ));
    assert_eq!(completed[0].status, CodeSubagentStatus::Done);
    assert_eq!(completed[1].status, CodeSubagentStatus::Done);
    assert_eq!(completed[2].status, CodeSubagentStatus::Failed);
    assert!(!settle_running_subagents(
        &mut completed,
        CodeSubagentStatus::Failed
    ));

    let mut failed = vec![subagent("running", CodeSubagentStatus::Running)];
    assert!(settle_running_subagents(
        &mut failed,
        CodeSubagentStatus::Failed
    ));
    assert_eq!(failed[0].status, CodeSubagentStatus::Failed);
}

#[tokio::test]
async fn an_aborted_permission_mode_settlement_rejects_a_turn_queued_behind_it() {
    let (commands, mut pending) = mpsc::channel(1);
    let (reply, outcome) = oneshot::channel();
    assert!(commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "must not disappear".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply,
        })
        .await
        .is_ok());
    let (settle, settlement) = oneshot::channel();
    assert!(settle.send(PermissionModeSettlement::Abort).is_ok());

    assert!(!await_permission_mode_settlement(settlement, &mut pending).await);
    match outcome.await.unwrap() {
        Err(WorkerError::Conflict(message)) => assert_eq!(
            message,
            "the turn was not accepted because the permission mode change did not commit"
        ),
        Err(error) => panic!("unexpected turn rejection: {error:?}"),
        Ok(_) => panic!("the queued turn unexpectedly ran"),
    }
    assert!(commands.is_closed());
}

async fn seeded_session(
    harness_kind: HarnessKind,
    harness_version: Option<&str>,
) -> (
    tempfile::TempDir,
    Arc<DbStore>,
    Arc<CodeEventBus>,
    SessionId,
) {
    seeded_session_at(
        harness_kind,
        harness_version,
        tidebreak_core::ExecutionLocation::Machine,
    )
    .await
}

async fn seeded_session_at(
    harness_kind: HarnessKind,
    harness_version: Option<&str>,
    location: tidebreak_core::ExecutionLocation,
) -> (
    tempfile::TempDir,
    Arc<DbStore>,
    Arc<CodeEventBus>,
    SessionId,
) {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let owner = OwnerId::local();
    let repo_id = RepoId::new();
    insert_repo(
        &store,
        &CodeRepo {
            id: repo_id,
            owner: owner.clone(),
            root_path: directory.path().join("repo").display().to_string(),
            display_name: "example".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: Utc::now(),
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
        &store,
        &CodeWorkspace {
            id: workspace_id,
            owner: owner.clone(),
            repo_id,
            title: "first".into(),
            worktree_path: directory.path().join("wt").display().to_string(),
            branch_name: "tidebreak/first".into(),
            base_ref: "main".into(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
            setup_error: None,
        },
    )
    .await
    .unwrap();
    let session_id = SessionId::new();
    insert_session(
        &store,
        &Session {
            visibility: tidebreak_core::SessionVisibility::Private,
            id: session_id,
            owner: owner.clone(),
            owner_kind: None,
            workspace_id: Some(workspace_id),
            kind: SessionKind::Interactive,
            harness_kind,
            harness_version: harness_version.map(str::to_owned),
            harness_resume_ref: None,
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: SessionLifecycle::Running,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
            execution_location: location,
            acts_as: None,
        },
    )
    .await
    .unwrap();
    (
        directory,
        store,
        Arc::new(CodeEventBus::default()),
        session_id,
    )
}

async fn seeded_sink() -> (tempfile::TempDir, Arc<DbStore>, Arc<LiveSink>, SessionId) {
    seeded_sink_at(tidebreak_core::ExecutionLocation::Machine).await
}

async fn seeded_sink_at(
    location: tidebreak_core::ExecutionLocation,
) -> (tempfile::TempDir, Arc<DbStore>, Arc<LiveSink>, SessionId) {
    let (directory, store, bus, session_id) =
        seeded_session_at(HarnessKind::ClaudeCode, Some("2.1.237"), location).await;
    let sink = sink_for(
        store.clone(),
        bus,
        OwnerId::local(),
        session_id,
        1,
        HarnessKind::ClaudeCode,
        false,
        None,
        Vec::new(),
        None,
        None,
        None,
        None,
        crate::code::pr_refresh::HotPullRequests::default(),
    );
    (directory, store, sink, session_id)
}

#[tokio::test]
async fn a_stale_local_worker_leaves_sandbox_queue_rows_for_the_remote_driver() {
    let (directory, db, sink, session_id) =
        seeded_sink_at(tidebreak_core::ExecutionLocation::Sandbox).await;
    let owner = OwnerId::local();
    let mut session = get_session(&db, &owner, session_id).await.unwrap().unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&db, &session).await.unwrap());
    // A stale worker copy says Machine while the durable session says Sandbox.
    session.execution_location = tidebreak_core::ExecutionLocation::Machine;
    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let adapter = ScriptedAdapter::new(plain_text_script());
    let engine = adapter
        .launch(SessionSpec {
            owner: owner.clone(),
            session_id,
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let now = Utc::now();
    let queued = enqueue_queued_turn(
        &db,
        &owner,
        &QueuedTurn {
            actor: None,
            id: TurnId::new(),
            session_id,
            message: "stay in the sandbox".into(),
            attachments: Vec::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    let store = AttachmentStore {
        blobs: None,
        private_root: super::super::scratch::ScratchRoot::open_for_test(&private).unwrap(),
        engine_reads_images: false,
    };
    let queue = TurnQueue {
        worktree: Arc::new(tokio::sync::Mutex::new(())),
        wake: Arc::new(tokio::sync::Notify::new()),
        quiesce: tokio::sync::watch::channel(false).1,
    };
    let (_sender, mut commands) = mpsc::channel(1);
    drain_queued(
        &mut session,
        engine.as_ref(),
        &sink,
        &queue,
        &store,
        &mut commands,
    )
    .await;
    assert_eq!(
        queued_turn_head(&db, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        queued.id
    );
    assert!(list_turns(&db, &owner, session_id)
        .await
        .unwrap()
        .is_empty());
}

/// An engine that answers its own approval — a standing grant, an
/// auto-approval judge — reports the decision on the stream, and that
/// report is the only settlement the row it opened will ever get.
#[tokio::test]
async fn an_engine_observed_decision_settles_its_own_approval_row() {
    let (directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());

    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
    let script = vec![
        HarnessEvent::TurnStarted,
        HarnessEvent::ApprovalRequested {
            harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
            raw: serde_json::Value::Null,
            kind: Some(tidebreak_core::ApprovalKind::Other {
                summary: "exec".into(),
            }),
        },
        HarnessEvent::ApprovalResolved {
            harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
            decision: ApprovalDecision::Approve,
        },
        HarnessEvent::AssistantMessage {
            text: "ran under a standing grant".into(),
            parent_call_id: None,
        },
        HarnessEvent::TurnCompleted {
            usage: TurnUsage::default(),
        },
    ];
    let adapter = ScriptedAdapter::new(script).with_unattended_approvals();
    let engine = adapter
        .launch(SessionSpec {
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let handle = spawn_session_worker(
        session.clone(),
        engine,
        sink,
        AttachmentStore {
            blobs: None,
            private_root,
            engine_reads_images: false,
        },
        Arc::new(tokio::sync::Mutex::new(())),
        tokio::sync::watch::channel(false).1,
    );

    let (turn_reply, turn_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "run it".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply: turn_reply,
        })
        .await
        .unwrap();
    let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
        .await
        .expect("the turn completes")
        .unwrap()
        .unwrap();
    assert_eq!(turn.status, TurnStatus::Completed);

    let approvals = list_approvals(&store, &owner, None, Some(session_id))
        .await
        .unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].state, ApprovalState::Approved);
    assert_eq!(approvals[0].native_call_id.as_deref(), Some("call-1"));
    let events = list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap();
    assert!(
        events.events.iter().any(|event| matches!(
            &event.event,
            Event::ApprovalResolved {
                approval_id,
                decision: tidebreak_core::ApprovalDecisionKind::Approve,
                ..
            } if *approval_id == approvals[0].id
        )),
        "the journal records the engine's own decision"
    );
    let _ = handle.commands.send(WorkerCommand::Shutdown).await;
}

/// An internal turn parked for a client hands its lease back and leaves
/// the session idle while the row stays open. A send in that gap must be
/// refused rather than inserted beside it, where it could never take
/// its transcript message.
#[tokio::test]
async fn a_send_over_an_internal_turn_waiting_on_a_client_is_refused() {
    let (directory, store, bus, session_id) = seeded_session(HarnessKind::Internal, None).await;
    let owner = OwnerId::local();
    let sink = sink_for(
        store.clone(),
        bus,
        owner.clone(),
        session_id,
        1,
        HarnessKind::Internal,
        false,
        None,
        Vec::new(),
        None,
        None,
        None,
        None,
        crate::code::pr_refresh::HotPullRequests::default(),
    );
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());
    let waiting = Turn {
        actor: None,
        id: TurnId::new(),
        session_id,
        ordinal: 1,
        status: TurnStatus::WaitingForClient,
        model: None,
        fast_mode: false,
        user_input: "needs the client".into(),
        user_input_blob_id: None,
        attachments: Vec::new(),
        checkpoint_ref: None,
        diffstat: None,
        usage: None,
        narrative: None,
        rewrite: None,
        started_at: Utc::now(),
        ended_at: None,
        park_ref: None,
        park_wait: None,
    };
    insert_turn(&store, &owner, &waiting).await.unwrap();

    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
    let adapter = ScriptedAdapter::new(vec![
        HarnessEvent::TurnStarted,
        HarnessEvent::TurnCompleted {
            usage: TurnUsage::default(),
        },
    ]);
    let engine = adapter
        .launch(SessionSpec {
            owner: owner.clone(),
            session_id,
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let handle = spawn_session_worker(
        session.clone(),
        engine,
        sink,
        AttachmentStore {
            blobs: None,
            private_root,
            engine_reads_images: true,
        },
        Arc::new(tokio::sync::Mutex::new(())),
        tokio::sync::watch::channel(false).1,
    );

    let (turn_reply, turn_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "and now this".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply: turn_reply,
        })
        .await
        .unwrap();
    match tokio::time::timeout(Duration::from_secs(5), turn_response)
        .await
        .expect("the send is answered")
        .unwrap()
    {
        Err(WorkerError::Conflict(message)) => assert_eq!(
            message,
            format!(
                "turn {} is still waiting_for_client; finish it before sending again",
                waiting.id
            )
        ),
        Err(error) => panic!("unexpected turn rejection: {error:?}"),
        Ok(turn) => panic!("the send ran as turn {}", turn.id),
    }
    let turns = list_turns(&store, &owner, session_id).await.unwrap();
    assert_eq!(turns.len(), 1, "no second row was inserted: {turns:?}");
    assert_eq!(turns[0].status, TurnStatus::WaitingForClient);
    let _ = handle.commands.send(WorkerCommand::Shutdown).await;
}

#[tokio::test]
async fn a_parked_turn_waits_durably_and_resumes_on_the_awaited_decision() {
    let (directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());

    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
    let script = vec![
        HarnessEvent::TurnStarted,
        HarnessEvent::ApprovalRequested {
            harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
            raw: serde_json::json!({ "tool_name": "Write" }),
            kind: None,
        },
        HarnessEvent::AssistantMessage {
            text: "resumed after the decision".into(),
            parent_call_id: None,
        },
        HarnessEvent::TurnCompleted {
            usage: TurnUsage::default(),
        },
    ];
    // The engine checkpoints after the request instead of blocking on it.
    let adapter = ScriptedAdapter::new(script)
        .with_unattended_approvals()
        .with_parked_turn(
            2,
            "cp-1",
            tidebreak_harness::ParkWait::Approval {
                call_id: "call-1".into(),
            },
        );
    let engine = adapter
        .launch(SessionSpec {
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let handle = spawn_session_worker(
        session.clone(),
        engine,
        sink,
        AttachmentStore {
            blobs: None,
            private_root,
            engine_reads_images: false,
        },
        Arc::new(tokio::sync::Mutex::new(())),
        tokio::sync::watch::channel(false).1,
    );

    let (turn_reply, turn_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "park then resume".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply: turn_reply,
        })
        .await
        .unwrap();

    // The turn must reach the durable park before the decision arrives.
    let parked = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let turns = list_turns(&store, &owner, session_id).await.unwrap();
            if let Some(turn) = turns.iter().find(|t| t.status == TurnStatus::Waiting) {
                break turn.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the turn parks");
    assert_eq!(parked.park_ref.as_deref(), Some("cp-1"));
    assert_eq!(
        parked.park_wait,
        Some(tidebreak_core::TurnParkWait::Approval {
            call_id: "call-1".into()
        })
    );

    let (decide_reply, decide_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::Decide {
            approval: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
            decision: Box::new(tidebreak_harness::ApprovalDecision::Approve),
            reply: decide_reply,
        })
        .await
        .unwrap();
    decide_response.await.unwrap().unwrap();

    let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
        .await
        .expect("the turn completes after the resume")
        .unwrap()
        .unwrap();
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(turn.park_ref, None, "the resume clears the park");
    assert_eq!(turn.park_wait, None);
    assert_eq!(adapter.resumes().len(), 1, "one resume for one park");
    let events = list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap();
    assert!(
        events
            .events
            .iter()
            .any(|event| matches!(event.event, Event::TurnCompleted { .. })),
        "the resumed leg reaches the journal"
    );
    let _ = handle.commands.send(WorkerCommand::Shutdown).await;
}

#[tokio::test]
async fn client_and_agent_run_parks_resume_after_a_worker_restart() {
    let client_call = tidebreak_core::CallId::new().to_string();
    let agent_wait = tidebreak_core::CallId::new().to_string();
    let run_ids = vec![
        tidebreak_core::AgentRunId::new().to_string(),
        tidebreak_core::AgentRunId::new().to_string(),
    ];
    let cases = vec![
        (
            client_call.clone(),
            tidebreak_harness::ParkWait::ClientToolCall {
                call_id: client_call.clone(),
            },
            tidebreak_harness::ResumeInput::ClientToolCompleted {
                call_id: client_call,
            },
        ),
        (
            agent_wait.clone(),
            tidebreak_harness::ParkWait::AgentRuns {
                run_ids: run_ids.clone(),
            },
            tidebreak_harness::ResumeInput::AgentRunsSettled { run_ids },
        ),
    ];

    for (park_ref, waiting_on, expected_resume) in cases {
        let (directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        session.lifecycle = SessionLifecycle::Idle;
        assert!(save_session(&store, &session).await.unwrap());

        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let adapter = ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantMessage {
                text: "resumed after the restart".into(),
                parent_call_id: None,
            },
            HarnessEvent::TurnCompleted {
                usage: TurnUsage::default(),
            },
        ])
        .with_parked_turn(1, park_ref.clone(), waiting_on.clone());
        let first_engine = adapter
            .launch(SessionSpec {
                owner: owner.clone(),
                session_id,
                worktree: worktree.clone(),
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
                native: None,
                tool_bridge: None,
                apps: None,
            })
            .await
            .unwrap();
        let first_private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let first = spawn_session_worker(
            session.clone(),
            first_engine,
            sink.clone(),
            AttachmentStore {
                blobs: None,
                private_root: first_private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );
        let (turn_reply, _turn_response) = oneshot::channel();
        first
            .commands
            .send(WorkerCommand::RunTurn {
                actor: None,
                message: "park across a restart".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply: turn_reply,
            })
            .await
            .unwrap();
        let parked = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(turn) = get_open_turn(&store, &owner, session_id).await.unwrap() {
                    if turn.status == TurnStatus::Waiting {
                        break turn;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the first worker stores the park");
        assert_eq!(parked.park_ref.as_deref(), Some(park_ref.as_str()));
        first.abort.abort();
        tokio::time::timeout(Duration::from_secs(5), first.commands.closed())
            .await
            .expect("the crashed worker releases its command channel");

        session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        let second_engine = adapter
            .launch(SessionSpec {
                owner: owner.clone(),
                session_id,
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
                native: None,
                tool_bridge: None,
                apps: None,
            })
            .await
            .unwrap();
        let second_private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let second = spawn_session_worker(
            session,
            second_engine,
            sink,
            AttachmentStore {
                blobs: None,
                private_root: second_private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );

        let mut resolved = tidebreak_core::db::code::get_turn(&store, &owner, parked.id)
            .await
            .unwrap()
            .unwrap();
        resolved.status = TurnStatus::Resuming;
        assert!(save_turn(&store, &owner, &resolved).await.unwrap());

        let completed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let turn = tidebreak_core::db::code::get_turn(&store, &owner, parked.id)
                    .await
                    .unwrap()
                    .unwrap();
                if turn.status == TurnStatus::Completed {
                    break turn;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the relaunched worker resumes the park");
        assert_eq!(completed.park_ref, None);
        assert_eq!(completed.park_wait, None);
        assert_eq!(
            adapter.resumes(),
            vec![(park_ref, expected_resume)],
            "the recovered park resumes once with its exact dependency"
        );
        let _ = second.commands.send(WorkerCommand::Shutdown).await;
    }
}

#[tokio::test]
async fn a_decision_on_the_running_leg_resumes_the_park() {
    let (directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());

    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
    let script = vec![
        HarnessEvent::TurnStarted,
        HarnessEvent::ApprovalRequested {
            harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
            raw: serde_json::json!({ "tool_name": "Write" }),
            kind: None,
        },
        // Holds the opening leg open after the request is journaled so
        // Decide is admitted there, before `Parked` is persisted.
        HarnessEvent::AssistantDelta {
            text: "still running".into(),
        },
        HarnessEvent::AssistantMessage {
            text: "resumed after the decision".into(),
            parent_call_id: None,
        },
        HarnessEvent::TurnCompleted {
            usage: TurnUsage::default(),
        },
    ];
    let adapter = ScriptedAdapter::new(script)
        .with_unattended_approvals()
        .with_delay(Duration::from_millis(150))
        .with_approval_ack_delay(Duration::from_millis(150))
        .with_parked_turn(
            3,
            "cp-1",
            tidebreak_harness::ParkWait::Approval {
                call_id: "call-1".into(),
            },
        );
    let engine = adapter
        .launch(SessionSpec {
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let handle = spawn_session_worker(
        session.clone(),
        engine,
        sink,
        AttachmentStore {
            blobs: None,
            private_root,
            engine_reads_images: false,
        },
        Arc::new(tokio::sync::Mutex::new(())),
        tokio::sync::watch::channel(false).1,
    );

    let (turn_reply, turn_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "decide before the park".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply: turn_reply,
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = list_approvals(
                &store,
                &owner,
                Some(ApprovalState::Pending),
                Some(session_id),
            )
            .await
            .unwrap();
            if !pending.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the approval is published");
    let turns = list_turns(&store, &owner, session_id).await.unwrap();
    assert!(
        turns.iter().any(|turn| turn.status == TurnStatus::Running),
        "Decide must reach the opening leg while the turn is still running"
    );

    let (decide_reply, decide_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::Decide {
            approval: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
            decision: Box::new(tidebreak_harness::ApprovalDecision::Approve),
            reply: decide_reply,
        })
        .await
        .unwrap();
    decide_response.await.unwrap().unwrap();

    let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
        .await
        .expect("the turn completes from the already-delivered decision")
        .unwrap()
        .unwrap();
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(turn.park_ref, None, "the resume clears the park");
    assert_eq!(turn.park_wait, None);
    assert_eq!(adapter.resumes().len(), 1, "one resume for one park");
    let _ = handle.commands.send(WorkerCommand::Shutdown).await;
}

#[tokio::test]
async fn an_interrupt_closes_a_parked_turn() {
    let (directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());
    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
    let adapter = ScriptedAdapter::new(vec![HarnessEvent::TurnStarted]).with_parked_turn(
        1,
        "cp-1",
        tidebreak_harness::ParkWait::Approval {
            call_id: "call-1".into(),
        },
    );
    let engine = adapter
        .launch(SessionSpec {
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let handle = spawn_session_worker(
        session.clone(),
        engine,
        sink,
        AttachmentStore {
            blobs: None,
            private_root,
            engine_reads_images: false,
        },
        Arc::new(tokio::sync::Mutex::new(())),
        tokio::sync::watch::channel(false).1,
    );
    let (turn_reply, turn_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "park then interrupt".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply: turn_reply,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(turn) = get_open_turn(&store, &owner, session_id).await.unwrap() {
                if turn.status == TurnStatus::Waiting {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn parks");
    let (stop_reply, stop_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::Interrupt { reply: stop_reply })
        .await
        .unwrap();
    stop_response.await.unwrap().unwrap();
    let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
        .await
        .expect("the parked turn closes")
        .unwrap()
        .unwrap();
    assert_eq!(turn.status, TurnStatus::Interrupted);
    let _ = handle.commands.send(WorkerCommand::Shutdown).await;
}

#[tokio::test]
async fn a_confirmed_setting_reservation_wins_over_an_already_queued_idle_turn() {
    let (directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());
    let stale = SessionExecutionSettings {
        model: Some("stale".into()),
        reasoning_effort: Some(ReasoningEffort::High),
        fast_mode: true,
    };
    session = replace_session_execution_settings(&store, &owner, &session, &stale)
        .await
        .unwrap()
        .expect("the initial settings commit");

    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
    let adapter = ScriptedAdapter::new(plain_text_script());
    let engine = adapter
        .launch(SessionSpec {
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let handle = spawn_session_worker(
        session.clone(),
        engine,
        sink,
        AttachmentStore {
            blobs: None,
            private_root,
            engine_reads_images: false,
        },
        Arc::new(tokio::sync::Mutex::new(())),
        tokio::sync::watch::channel(false).1,
    );

    let committed = SessionExecutionSettings {
        model: Some("committed".into()),
        reasoning_effort: None,
        fast_mode: false,
    };
    let (reservation_reply, reservation_response) = oneshot::channel();
    let (settlement, release) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::SetExecutionSettings {
            settings: committed.clone(),
            settlement: release,
            reply: reservation_reply,
        })
        .await
        .unwrap();
    reservation_response.await.unwrap().unwrap();

    let (turn_reply, turn_response) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "use the committed settings".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply: turn_reply,
        })
        .await
        .unwrap();

    let updated = replace_session_execution_settings(&store, &owner, &session, &committed)
        .await
        .unwrap()
        .expect("the reserved settings commit");
    assert!(settlement
        .send(ExecutionSettingsSettlement::Confirmed)
        .is_ok());

    let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
        .await
        .expect("the turn completes")
        .unwrap()
        .unwrap();
    assert_eq!(turn.model, updated.model);
    assert_eq!(turn.fast_mode, updated.fast_mode);
    assert_eq!(adapter.turn_efforts(), vec![updated.reasoning_effort]);
    let inputs = adapter.turn_inputs();
    assert_eq!(inputs[0].model, updated.model);
    assert_eq!(inputs[0].fast_mode, updated.fast_mode);
    let _ = handle.commands.send(WorkerCommand::Shutdown).await;
}

#[tokio::test]
async fn a_queued_turn_uses_a_later_setting_committed_before_promotion() {
    let (directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());
    let held = SessionExecutionSettings {
        model: Some("held".into()),
        reasoning_effort: Some(ReasoningEffort::High),
        fast_mode: true,
    };
    session = replace_session_execution_settings(&store, &owner, &session, &held)
        .await
        .unwrap()
        .expect("the held settings commit");

    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
    let adapter = ScriptedAdapter::new(plain_text_script());
    let engine = adapter
        .launch(SessionSpec {
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let now = Utc::now();
    enqueue_queued_turn(
        &store,
        &owner,
        &QueuedTurn {
            actor: None,
            id: TurnId::new(),
            session_id,
            message: "use the held settings".into(),
            attachments: Vec::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    let worktree_lock = Arc::new(tokio::sync::Mutex::new(()));
    let checkout = worktree_lock.lock().await;
    let handle = spawn_session_worker(
        session.clone(),
        engine,
        sink,
        AttachmentStore {
            blobs: None,
            private_root,
            engine_reads_images: false,
        },
        worktree_lock.clone(),
        tokio::sync::watch::channel(false).1,
    );

    let later = SessionExecutionSettings {
        model: Some("later".into()),
        reasoning_effort: None,
        fast_mode: false,
    };
    let (reservation_reply, reservation_response) = oneshot::channel();
    let (settlement, release) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::SetExecutionSettings {
            settings: later.clone(),
            settlement: release,
            reply: reservation_reply,
        })
        .await
        .unwrap();
    reservation_response.await.unwrap().unwrap();
    let _updated = replace_session_execution_settings(&store, &owner, &session, &later)
        .await
        .unwrap()
        .expect("the later settings commit");
    assert!(settlement
        .send(ExecutionSettingsSettlement::Confirmed)
        .is_ok());

    drop(checkout);
    let turn = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let turns = list_turns(&store, &owner, session_id).await.unwrap();
            if let Some(turn) = turns
                .into_iter()
                .find(|turn| turn.status != TurnStatus::Running && !turn.user_input.is_empty())
            {
                return turn;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the queued turn completes");
    assert_eq!(turn.model, later.model);
    assert_eq!(turn.fast_mode, later.fast_mode);
    assert_eq!(adapter.turn_efforts(), vec![later.reasoning_effort]);
    let inputs = adapter.turn_inputs();
    assert_eq!(inputs[0].model, later.model);
    assert_eq!(inputs[0].fast_mode, later.fast_mode);
    let _ = handle.commands.send(WorkerCommand::Shutdown).await;
}

#[tokio::test]
async fn codex_attachment_journals_one_start_after_the_thread_is_known() {
    let (_directory, store, bus, session_id) =
        seeded_session(HarnessKind::Codex, Some("codex-cli 0.147.0")).await;
    let attached = attach_engine(
        &store,
        &bus,
        session_id,
        HarnessKind::Codex,
        Some("0.147.0".into()),
        None,
    )
    .await
    .unwrap();
    let owner = OwnerId::local();
    assert_eq!(attached.harness_version.as_deref(), Some("0.147.0"));
    assert_eq!(attached.lifecycle, SessionLifecycle::Idle);
    assert_eq!(attached.attention.state, AttentionState::Idle);
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .attention
            .state,
        AttentionState::Idle
    );

    assert!(
        list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events
            .iter()
            .all(|event| !matches!(&event.event, Event::SessionStarted { .. }))
    );

    let sink = sink_for(
        store.clone(),
        bus,
        owner.clone(),
        session_id,
        attached.spawn_epoch,
        HarnessKind::Codex,
        false,
        None,
        attached.subagents,
        None,
        None,
        None,
        None,
        crate::code::pr_refresh::HotPullRequests::default(),
    );
    sink.emit(HarnessEvent::SessionStarted {
        harness_kind: HarnessKind::Codex,
        harness_version: "0.147.0".into(),
        resume_ref: Some("thread-1".into()),
    })
    .await;

    let started = list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap()
        .events
        .into_iter()
        .filter_map(|event| match event.event {
            Event::SessionStarted {
                harness_kind,
                harness_version,
                resume_ref,
            } => Some((harness_kind, harness_version, resume_ref)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        started,
        vec![(
            HarnessKind::Codex,
            "0.147.0".into(),
            Some("thread-1".into())
        )]
    );
}

#[tokio::test]
async fn engine_attachment_preserves_unreviewed_work() {
    let (_directory, store, bus, session_id) =
        seeded_session(HarnessKind::ClaudeCode, Some("2.1.237")).await;
    let owner = OwnerId::local();
    let unreviewed = Attention::new(AttentionState::DoneUnreviewed, AttentionSource::Lifecycle);
    replace_session_attention(&store, &owner, session_id, &unreviewed, false)
        .await
        .unwrap();

    let attached = attach_engine(
        &store,
        &bus,
        session_id,
        HarnessKind::ClaudeCode,
        Some("2.1.237".into()),
        None,
    )
    .await
    .unwrap();

    assert_eq!(attached.lifecycle, SessionLifecycle::Idle);
    assert_eq!(attached.attention, unreviewed);
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .attention,
        unreviewed
    );
}

#[tokio::test]
async fn non_codex_attachment_keeps_the_eager_session_start() {
    let (_directory, store, bus, session_id) =
        seeded_session(HarnessKind::ClaudeCode, Some("2.1.237")).await;

    attach_engine(
        &store,
        &bus,
        session_id,
        HarnessKind::ClaudeCode,
        Some("2.1.237".into()),
        None,
    )
    .await
    .unwrap();

    let events = list_events(&store, &OwnerId::local(), session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap()
        .events;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.event, Event::SessionStarted { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn sink_settles_unclosed_tasks_at_each_terminal_parent_boundary() {
    let (_directory, store, sink, session_id) = seeded_sink().await;
    let cases = [
        (
            "completed",
            HarnessEvent::TurnCompleted {
                usage: TurnUsage::default(),
            },
            CodeSubagentStatus::Done,
        ),
        (
            "failed",
            HarnessEvent::TurnFailed {
                error: BoundedError {
                    message: "engine failed".into(),
                },
            },
            CodeSubagentStatus::Failed,
        ),
        (
            "interrupted",
            HarnessEvent::TurnInterrupted,
            CodeSubagentStatus::Failed,
        ),
    ];

    for (call_id, boundary, expected) in cases {
        sink.emit(HarnessEvent::ToolStarted {
            call_id: call_id.into(),
            name: "Task".into(),
            detail: ToolDetail::Other {
                summary: format!("{call_id} child"),
            },
            parent_call_id: None,
        })
        .await;
        sink.emit(boundary).await;
        let session = get_session(&store, &OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            session
                .subagents
                .iter()
                .find(|entry| entry.call_id == call_id)
                .map(|entry| entry.status),
            Some(expected)
        );

        // Codex can publish a child result after the parent boundary.
        // The parent already settled the span, so that late result must
        // not revise the recorded outcome.
        sink.emit(HarnessEvent::ToolCompleted {
            call_id: call_id.into(),
            outcome: if expected == CodeSubagentStatus::Done {
                ToolOutcome::Failed
            } else {
                ToolOutcome::Succeeded
            },
            preview: "late child result".into(),
            detail: None,
            parent_call_id: None,
        })
        .await;
        let session = get_session(&store, &OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            session
                .subagents
                .iter()
                .find(|entry| entry.call_id == call_id)
                .map(|entry| entry.status),
            Some(expected)
        );
    }
}

#[tokio::test]
async fn sink_persists_a_resume_ref_only_after_turn_activity_starts() {
    let (_directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();

    sink.emit(HarnessEvent::SessionStarted {
        harness_kind: HarnessKind::Codex,
        harness_version: "0.147.0".into(),
        resume_ref: Some("thread-1".into()),
    })
    .await;
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .harness_resume_ref,
        None,
        "an unused Codex thread is not a safe resume target"
    );

    sink.emit(HarnessEvent::TurnStarted).await;
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .harness_resume_ref
            .as_deref(),
        Some("thread-1")
    );
}

#[tokio::test]
async fn assistant_activity_persists_resume_refs_for_harnesses_without_turn_started() {
    let (_directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();
    let mut worker_session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(worker_session.harness_resume_ref, None);

    sink.emit(HarnessEvent::SessionStarted {
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: "2.1.237".into(),
        resume_ref: Some("session-1".into()),
    })
    .await;
    sink.emit(HarnessEvent::AssistantDelta {
        text: "Working".into(),
    })
    .await;

    // A child pid may arrive after the sink writes the resume ref. Mirror
    // the real worker path and prove that its stale session copy keeps the
    // ref instead of replacing it with NULL during the full-row save.
    worker_session.child_pid = Some(4242);
    assert!(save_session(&store, &worker_session).await.unwrap());
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .harness_resume_ref
            .as_deref(),
        Some("session-1")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_pre_turn_attachment_sweep_fences_the_session() {
    use std::os::unix::fs::PermissionsExt as _;

    let (directory, store, sink, session_id) = seeded_sink().await;
    let private_path = directory.path().join("private");
    std::fs::create_dir(&private_path).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private_path).expect("scratch root");
    let attachment_root = private_root.path().join(ATTACHMENTS_DIR);
    let leftover = attachment_root.join(SessionId::new().to_string());
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("private.png"), b"private").unwrap();
    std::fs::set_permissions(&attachment_root, std::fs::Permissions::from_mode(0o500)).unwrap();

    let mut session = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    let result =
        sweep_attachment_leftovers_or_fence(&store, &sink.bus, &mut session, &private_root).await;

    std::fs::set_permissions(&attachment_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(WorkerError::Failed(_))));
    let stored = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.lifecycle, SessionLifecycle::Fenced);
    assert!(matches!(
        stored.fence_reason,
        Some(FenceReason::ProbeAmbiguous { ref detail })
            if detail.starts_with("sweep attachments:")
    ));
}

#[test]
fn cap_raw_truncates_on_a_char_boundary() {
    // `{"xx":"` is 7 bytes; a string of `é` (2 bytes each) then places a
    // mid-character byte at MAX_HARNESS_RAW_BYTES. Slicing there panics.
    let raw = serde_json::json!({ "xx": "é".repeat(MAX_HARNESS_RAW_BYTES) });
    assert!(raw.to_string().len() > MAX_HARNESS_RAW_BYTES);
    assert!(!raw.to_string().is_char_boundary(MAX_HARNESS_RAW_BYTES));
    let capped = cap_raw(&raw);
    assert_eq!(capped["truncated"], true);
    let preview = capped["preview"].as_str().expect("preview is a string");
    assert!(preview.len() <= MAX_HARNESS_RAW_BYTES);
    assert!(preview.is_char_boundary(preview.len()));
}

#[test]
fn persist_harness_raw_keeps_call_id_when_the_payload_is_capped() {
    let raw = serde_json::json!({
        "tool_name": "Write",
        "input": {
            "file_path": "/workspace/big.txt",
            "content": "x".repeat(MAX_HARNESS_RAW_BYTES + 64),
        },
        "tool_use_id": "toolu_oversized",
    });
    let stored = persist_harness_raw("toolu_oversized", &raw);
    assert_eq!(stored["truncated"], true);
    assert_eq!(stored["call_id"], "toolu_oversized");
    assert!(stored.get("tool_use_id").is_none());
}

#[test]
fn kind_from_raw_reads_codex_and_opencode_payloads() {
    assert_eq!(
        kind_from_raw(&serde_json::json!({
            "command": "/bin/zsh -lc rg foo",
            "cwd": "/workspace",
        })),
        ApprovalKind::Command {
            cmd: "/bin/zsh -lc rg foo".into(),
            cwd: Some("/workspace".into()),
        }
    );
    assert_eq!(
        kind_from_raw(&serde_json::json!({
            "permission": "bash",
            "metadata": { "command": "rg foo" },
        })),
        ApprovalKind::Command {
            cmd: "rg foo".into(),
            cwd: None,
        }
    );
    assert_eq!(
        kind_from_raw(&serde_json::json!({
            "permission": "edit",
            "metadata": {
                "filepath": "/workspace/docs/approval.md",
                "cwd": "/worktree"
            },
            "patterns": ["docs/approval.md", "*.md"]
        })),
        ApprovalKind::FileWrite {
            paths: vec!["/worktree/docs/approval.md".into()],
        }
    );
    assert_eq!(
        kind_from_raw(&serde_json::json!({
            "permission": "edit",
            "cwd": "/worktree",
            "patterns": ["docs/fallback.md", "*"]
        })),
        ApprovalKind::FileWrite {
            paths: vec!["/worktree/docs/fallback.md".into()],
        }
    );
    assert_eq!(
        kind_from_raw(&serde_json::json!({
            "permission": "edit",
            "patterns": ["*"]
        })),
        ApprovalKind::FileWrite { paths: Vec::new() }
    );
    assert_eq!(
        kind_from_raw(&serde_json::json!({
            "tool_name": "Read",
            "input": { "file_path": "/workspace/README.md" },
        })),
        ApprovalKind::Other {
            summary: "Read /workspace/README.md".into(),
        }
    );
    assert_eq!(
        kind_from_raw(&serde_json::Value::Null),
        ApprovalKind::Other {
            summary: "The engine needs approval".into(),
        }
    );
}

#[tokio::test]
async fn fallback_images_live_only_in_the_session_turn_scope() {
    let private = tempfile::tempdir().unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(private.path()).expect("scratch root");
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let attachment = ImageRef {
        blob_id: uuid::Uuid::new_v4(),
        media_type: ImageMediaType::Png,
        width: 1,
        height: 1,
        byte_len: 4,
    };
    let staged = write_turn_attachments(
        &private_root,
        session_id,
        turn_id,
        std::slice::from_ref(&attachment),
        &[TurnImage {
            media_type: "image/png".into(),
            bytes: vec![1, 2, 3, 4],
        }],
    )
    .await
    .unwrap();
    let expected = private
        .path()
        .join(ATTACHMENTS_DIR)
        .join(session_id.to_string())
        .join(turn_id.to_string())
        .join(format!("{}.png", attachment.blob_id))
        .display()
        .to_string();
    assert_eq!(staged.paths, vec![expected.clone()]);
    assert_eq!(std::fs::read(&expected).unwrap(), [1, 2, 3, 4]);

    let mut staged = staged;
    staged.scope.cleanup().unwrap();

    assert!(!private
        .path()
        .join(ATTACHMENTS_DIR)
        .join(session_id.to_string())
        .exists());
}

#[test]
fn attachment_paths_are_named_after_the_message_in_order() {
    let message =
        message_naming_attachments("compare these", &["first.png".into(), "second.png".into()]);
    assert_eq!(
        message,
        "compare these\n\nimages attached to this message:\n- `first.png`\n- `second.png`"
    );
}

#[test]
fn provider_auth_failures_are_recognized_and_other_failures_are_not() {
    // The vendor bodies the shipped engines actually pass through.
    assert!(provider_auth_failure(
        "Missing bearer or basic authentication in header"
    ));
    assert!(provider_auth_failure(
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#
    ));
    assert!(provider_auth_failure("Invalid API key · Please run /login"));
    assert!(provider_auth_failure("HTTP 401 Unauthorized"));
    // Everything else keeps the engine's own words.
    assert!(!provider_auth_failure(
        "overloaded_error: try again shortly"
    ));
    assert!(!provider_auth_failure("engine exited with status 1"));
}

#[tokio::test]
async fn a_raw_401_turn_failure_reads_as_a_sign_in_problem() {
    let (_directory, _store, sink, _session_id) = seeded_sink().await;
    let mapped = sink.legible_turn_error("Missing bearer or basic authentication in header".into());
    assert!(
        mapped
            .message
            .starts_with("Claude Code is not signed in on this machine."),
        "got: {}",
        mapped.message
    );
    // The engine's own words survive for whoever debugs the transcript.
    assert!(mapped
        .message
        .contains("Missing bearer or basic authentication in header"));
    let untouched = sink.legible_turn_error("engine exited with status 1".into());
    assert_eq!(untouched.message, "engine exited with status 1");
}

#[tokio::test]
async fn a_relay_wired_session_keeps_the_relays_own_refusal() {
    let (_directory, store, bus, session_id) =
        seeded_session(HarnessKind::Codex, Some("0.147.0")).await;
    let sink = sink_for(
        store,
        bus,
        OwnerId::local(),
        session_id,
        1,
        HarnessKind::Codex,
        true,
        None,
        Vec::new(),
        None,
        None,
        None,
        None,
        crate::code::pr_refresh::HotPullRequests::default(),
    );
    // The relay's refusals already name the gateway; "sign in in your
    // own terminal" would be wrong on a hosted machine.
    let kept = sink.legible_turn_error("authentication_error: sign in required".into());
    assert_eq!(kept.message, "authentication_error: sign in required");
}

#[tokio::test]
async fn an_update_quiesce_refuses_new_turns_until_resumed() {
    let (directory, store, sink, session_id) = seeded_sink().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = SessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());

    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let private = directory.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let private_root =
        super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
    let adapter = ScriptedAdapter::new(plain_text_script());
    let engine = adapter
        .launch(SessionSpec {
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree,
            allowed_read_roots: Vec::new(),
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
        })
        .await
        .unwrap();
    let (quiesce_tx, quiesce_rx) = watch::channel(true);
    let handle = spawn_session_worker(
        session.clone(),
        engine,
        sink,
        AttachmentStore {
            blobs: None,
            private_root,
            engine_reads_images: false,
        },
        Arc::new(tokio::sync::Mutex::new(())),
        quiesce_rx,
    );

    // While the quiesce holds, a send must not start a turn: the caller
    // (submit_turn) parks it as a durable queue row instead.
    let (reply, refused) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "hello".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply,
        })
        .await
        .unwrap();
    match refused.await.unwrap() {
        Err(WorkerError::UpdateQuiesced) => {}
        other => panic!("expected an update-quiesced refusal, got {other:?}"),
    }
    assert!(list_turns(&store, &owner, session_id)
        .await
        .unwrap()
        .is_empty());

    // Ending the quiesce (a failed install) reopens admission; the same
    // send now runs to completion.
    quiesce_tx.send_replace(false);
    let (reply, ran) = oneshot::channel();
    handle
        .commands
        .send(WorkerCommand::RunTurn {
            actor: None,
            message: "hello again".into(),
            attachments: Vec::new(),
            trigger_delivery: None,
            reply,
        })
        .await
        .unwrap();
    ran.await.unwrap().expect("the turn runs after the resume");
    assert_eq!(
        list_turns(&store, &owner, session_id).await.unwrap().len(),
        1
    );
}

#[derive(Clone, Copy)]
enum SteerControlAnswer {
    Ack,
    Unsupported,
    Rejected,
    Unknown,
    Timeout,
}

/// A native control channel whose acknowledgment can be held independently
/// of the worker reply and database commit.
struct SteerControlHarness {
    answer: SteerControlAnswer,
    entered: Notify,
    acknowledge: Notify,
    acknowledged: Notify,
    requests: std::sync::Mutex<Vec<(String, Option<uuid::Uuid>)>>,
    events: std::sync::Mutex<Vec<HarnessEvent>>,
}

impl SteerControlHarness {
    fn new(answer: SteerControlAnswer) -> Self {
        Self {
            answer,
            entered: Notify::new(),
            acknowledge: Notify::new(),
            acknowledged: Notify::new(),
            requests: std::sync::Mutex::new(Vec::new()),
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl HarnessSession for SteerControlHarness {
    async fn run_turn(&self, _: TurnInput) -> Result<TurnOutcome, HarnessError> {
        panic!("a steering control must not start another native turn");
    }

    async fn decide(&self, _: HarnessApprovalRef, _: ApprovalDecision) -> Result<(), HarnessError> {
        panic!("a steering control must not decide an approval");
    }

    async fn interrupt(&self) -> Result<(), HarnessError> {
        panic!("a steering control must not interrupt the native turn");
    }

    async fn steer_with_correlation(
        &self,
        text: String,
        correlation_uuid: Option<uuid::Uuid>,
    ) -> Result<(), HarnessError> {
        self.requests
            .lock()
            .unwrap()
            .push((text.clone(), correlation_uuid));
        self.entered.notify_one();
        match self.answer {
            SteerControlAnswer::Ack => {
                self.acknowledge.notified().await;
                self.events.lock().unwrap().push(HarnessEvent::UserSteered {
                    text,
                    correlation_uuid,
                });
                self.acknowledged.notify_one();
                Ok(())
            }
            SteerControlAnswer::Unsupported => Err(HarnessError::SteeringUnsupported),
            SteerControlAnswer::Rejected => Err(HarnessError::SteeringRejected(
                "native turn rejected the request".into(),
            )),
            SteerControlAnswer::Unknown => Err(HarnessError::Other(
                "the request was written but its acknowledgment was lost".into(),
            )),
            SteerControlAnswer::Timeout => std::future::pending().await,
        }
    }

    fn resume_ref(&self) -> Option<String> {
        None
    }
    fn unrecognized_events(&self) -> u64 {
        0
    }
    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        Ok(())
    }
}

async fn pending_worker_steer(
    db: &Arc<DbStore>,
    session_id: SessionId,
) -> (SteerAdmission, QueuedTurn) {
    let expected_turn_id = TurnId::new();
    let correlation_uuid = Some(uuid::Uuid::new_v4());
    let owner = OwnerId::local();
    let record = tidebreak_core::db::code::record_external_message_with_steer(
        db,
        &owner,
        session_id,
        "EvWorkerSteer",
        "1700000001.000100",
        "use the corrected fixture",
        &tidebreak_core::TurnActor::default(),
        None,
        tidebreak_core::db::code::ExternalSteerAdmissionInput {
            request_steer: true,
            expected_turn_id: Some(expected_turn_id),
            correlation_uuid,
        },
    )
    .await
    .unwrap();
    let tidebreak_core::ExternalMessageRecord::Recorded(row) = record else {
        panic!("first worker steer must create its receipt");
    };
    (
        SteerAdmission {
            db: Arc::clone(db),
            owner,
            session_id,
            spawn_epoch: 1,
            event_id: "EvWorkerSteer".into(),
            expected_turn_id,
            correlation_uuid,
        },
        *row,
    )
}

async fn worker_steer_receipt(admission: &SteerAdmission) -> tidebreak_core::ExternalMessageRecord {
    tidebreak_core::db::code::external_steer_admission(
        &admission.db,
        &admission.owner,
        admission.session_id,
        &admission.event_id,
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn durable_steer_ack_commits_before_the_worker_replies() {
    use sea_orm::{ConnectionTrait, TransactionTrait};
    use tidebreak_core::code::{ExternalMessageRecord, ExternalSteerAdmission};
    let (directory, db, _, session_id) = seeded_session(HarnessKind::Codex, None).await;
    let (admission, row) = pending_worker_steer(&db, session_id).await;
    insert_running_steer_target(&db, &admission).await;
    let engine = Arc::new(SteerControlHarness::new(SteerControlAnswer::Ack));
    // Hold SQLite's writer lock so the native ACK cannot immediately become
    // a durable receipt. A premature worker reply is observable in this gap.
    let writer = sea_orm::Database::connect(format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("t.db").display()
    ))
    .await
    .unwrap();
    let transaction = writer.begin().await.unwrap();
    transaction
        .execute_unprepared(
            "UPDATE session SET unrecognized_event_count = unrecognized_event_count",
        )
        .await
        .unwrap();
    let (reply, mut response) = oneshot::channel();
    let worker = {
        let engine = Arc::clone(&engine);
        let admission = admission.clone();
        tokio::spawn(async move {
            apply_control(
                engine.as_ref(),
                WorkerCommand::Steer {
                    expected_turn_id: admission.expected_turn_id,
                    message: row.message,
                    admission: Some(admission.clone()),
                    reply,
                },
                Some(admission.expected_turn_id),
                None,
            )
            .await;
        })
    };
    engine.entered.notified().await;
    assert!(matches!(
        worker_steer_receipt(&admission).await,
        ExternalMessageRecord::Replay {
            admission: None,
            ..
        }
    ));
    assert!(matches!(
        response.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    engine.acknowledge.notify_one();
    engine.acknowledged.notified().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut response)
            .await
            .is_err(),
        "native ACK alone must not complete the worker reply before persistence"
    );
    transaction.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), response)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    worker.await.unwrap();
    assert!(matches!(
        worker_steer_receipt(&admission).await,
        ExternalMessageRecord::Replay {
            admission: Some(ExternalSteerAdmission::Steered),
            ..
        }
    ));
    assert!(
        tidebreak_core::db::code::list_queued_turns(&db, &admission.owner, session_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        engine.requests.lock().unwrap().as_slice(),
        &[(
            "use the corrected fixture".into(),
            admission.correlation_uuid
        )]
    );
    assert_eq!(engine.events.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn durable_steer_ack_survives_a_dropped_worker_reply() {
    use tidebreak_core::code::{ExternalMessageRecord, ExternalSteerAdmission};
    let (directory, db, _, session_id) = seeded_session(HarnessKind::Codex, None).await;
    let (admission, row) = pending_worker_steer(&db, session_id).await;
    insert_running_steer_target(&db, &admission).await;
    let engine = SteerControlHarness::new(SteerControlAnswer::Ack);
    engine.acknowledge.notify_one();
    let (reply, response) = oneshot::channel();
    drop(response);
    apply_control(
        &engine,
        WorkerCommand::Steer {
            expected_turn_id: admission.expected_turn_id,
            message: row.message,
            admission: Some(admission.clone()),
            reply,
        },
        Some(admission.expected_turn_id),
        None,
    )
    .await;
    assert_eq!(engine.events.lock().unwrap().len(), 1);
    // A reconnecting HTTP delivery reads the stored admission, even when its
    // original request no longer waits for the worker's response.
    let reopened = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("t.db").display()
    ))
    .await
    .unwrap();
    let replay = tidebreak_core::db::code::record_external_message_with_steer(
        &reopened,
        &admission.owner,
        session_id,
        &admission.event_id,
        "1700000001.000100",
        "use the corrected fixture",
        &tidebreak_core::TurnActor::default(),
        None,
        tidebreak_core::db::code::ExternalSteerAdmissionInput::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        replay,
        ExternalMessageRecord::Replay {
            turn_id: row.id,
            steer_requested: Some(true),
            expected_turn_id: Some(admission.expected_turn_id),
            correlation_uuid: admission.correlation_uuid,
            admission: Some(ExternalSteerAdmission::Steered),
            queued_reason: None,
        }
    );
    assert!(
        tidebreak_core::db::code::list_queued_turns(&reopened, &admission.owner, session_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn durable_steer_only_proven_rejection_releases_the_queue() {
    use tidebreak_core::code::{
        ExternalMessageRecord, ExternalSteerAdmission, ExternalSteerQueuedReason,
    };
    for (answer, released) in [
        (SteerControlAnswer::Unsupported, true),
        (SteerControlAnswer::Rejected, true),
        (SteerControlAnswer::Unknown, false),
        (SteerControlAnswer::Timeout, false),
    ] {
        let (_directory, db, _, session_id) = seeded_session(HarnessKind::Codex, None).await;
        let (admission, row) = pending_worker_steer(&db, session_id).await;
        let engine = SteerControlHarness::new(answer);
        let (reply, response) = oneshot::channel();
        apply_control(
            &engine,
            WorkerCommand::Steer {
                expected_turn_id: admission.expected_turn_id,
                message: row.message,
                admission: Some(admission.clone()),
                reply,
            },
            Some(admission.expected_turn_id),
            None,
        )
        .await;
        let error = response.await.unwrap().unwrap_err();
        if released {
            assert!(matches!(
                error,
                WorkerError::SteeringUnavailable(_) | WorkerError::SteeringRejected(_)
            ));
            assert!(matches!(
                worker_steer_receipt(&admission).await,
                ExternalMessageRecord::Replay {
                    admission: Some(ExternalSteerAdmission::Queued),
                    queued_reason: Some(ExternalSteerQueuedReason::SteerUnsupported),
                    ..
                }
            ));
        } else {
            assert!(matches!(error, WorkerError::Failed(_)));
            assert!(matches!(
                worker_steer_receipt(&admission).await,
                ExternalMessageRecord::Replay {
                    admission: None,
                    queued_reason: None,
                    ..
                }
            ));
        }
        assert_eq!(
            queued_turn_head(&db, &admission.owner, session_id)
                .await
                .unwrap()
                .is_some(),
            released
        );
        assert_eq!(
            tidebreak_core::db::code::list_queued_turns(&db, &admission.owner, session_id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(engine.events.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn durable_steer_stale_target_falls_back_without_native_delivery() {
    use tidebreak_core::code::{
        ExternalMessageRecord, ExternalSteerAdmission, ExternalSteerQueuedReason,
    };
    let (_directory, db, _, session_id) = seeded_session(HarnessKind::Codex, None).await;
    let (admission, row) = pending_worker_steer(&db, session_id).await;
    let engine = SteerControlHarness::new(SteerControlAnswer::Ack);
    let (reply, response) = oneshot::channel();
    apply_control(
        &engine,
        WorkerCommand::Steer {
            expected_turn_id: admission.expected_turn_id,
            message: row.message,
            admission: Some(admission.clone()),
            reply,
        },
        Some(TurnId::new()),
        None,
    )
    .await;
    assert!(matches!(
        response.await.unwrap(),
        Err(WorkerError::StaleTurn(_))
    ));
    assert!(engine.requests.lock().unwrap().is_empty());
    assert!(matches!(
        worker_steer_receipt(&admission).await,
        ExternalMessageRecord::Replay {
            admission: Some(ExternalSteerAdmission::Queued),
            queued_reason: Some(ExternalSteerQueuedReason::StaleTurn),
            ..
        }
    ));
    assert_eq!(
        queued_turn_head(&db, &admission.owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        row.id
    );
}

async fn insert_running_steer_target(db: &DbStore, admission: &SteerAdmission) -> Turn {
    let turn = Turn {
        actor: None,
        id: admission.expected_turn_id,
        session_id: admission.session_id,
        ordinal: 1,
        status: TurnStatus::Running,
        model: None,
        fast_mode: false,
        user_input: "original work".into(),
        user_input_blob_id: None,
        attachments: Vec::new(),
        checkpoint_ref: None,
        diffstat: None,
        usage: None,
        narrative: None,
        rewrite: None,
        started_at: Utc::now(),
        ended_at: None,
        park_ref: None,
        park_wait: None,
    };
    insert_turn(db, &admission.owner, &turn).await.unwrap();
    turn
}

#[tokio::test]
async fn durable_steer_late_stream_ack_consumes_the_original_queue_row() {
    use tidebreak_core::code::{ExternalMessageRecord, ExternalSteerAdmission};
    let (_directory, db, sink, session_id) = seeded_sink().await;
    let (admission, row) = pending_worker_steer(&db, session_id).await;
    insert_running_steer_target(&db, &admission).await;
    sink.set_turn(admission.expected_turn_id);
    let (reply, response) = oneshot::channel();
    apply_control(
        &SteerControlHarness::new(SteerControlAnswer::Timeout),
        WorkerCommand::Steer {
            expected_turn_id: admission.expected_turn_id,
            message: row.message.clone(),
            admission: Some(admission.clone()),
            reply,
        },
        Some(admission.expected_turn_id),
        None,
    )
    .await;
    assert!(matches!(
        response.await.unwrap(),
        Err(WorkerError::Failed(_))
    ));
    assert!(matches!(
        worker_steer_receipt(&admission).await,
        ExternalMessageRecord::Replay {
            admission: None,
            ..
        }
    ));

    sink.emit(HarnessEvent::UserSteered {
        text: row.message.clone(),
        correlation_uuid: admission.correlation_uuid,
    })
    .await;
    assert!(matches!(
        worker_steer_receipt(&admission).await,
        ExternalMessageRecord::Replay {
            admission: Some(ExternalSteerAdmission::Steered),
            ..
        }
    ));
    assert!(
        tidebreak_core::db::code::list_queued_turns(&db, &admission.owner, session_id)
            .await
            .unwrap()
            .is_empty()
    );
    let events = list_events(&db, &admission.owner, session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap()
        .events;
    assert_eq!(
        events
            .iter()
            .filter(|item| matches!(&item.event,
        Event::UserSteered { text, .. } if text == &row.message))
            .count(),
        1
    );
}

#[tokio::test]
async fn durable_steer_stream_ack_rejects_stale_or_foreign_targets() {
    use tidebreak_core::code::{
        ExternalMessageRecord, ExternalSteerAdmission, ExternalSteerQueuedReason,
    };
    for invalid in [
        "epoch",
        "turn",
        "terminal",
        "location",
        "remote_claim",
        "correlation",
        "owner",
        "session",
        "queued",
    ] {
        let location = if invalid == "location" {
            tidebreak_core::ExecutionLocation::Sandbox
        } else {
            tidebreak_core::ExecutionLocation::Machine
        };
        let (_directory, db, mut sink, session_id) = seeded_sink_at(location).await;
        let (admission, row) = pending_worker_steer(&db, session_id).await;
        let mut turn = insert_running_steer_target(&db, &admission).await;
        sink.set_turn(admission.expected_turn_id);
        let mut correlation_uuid = admission.correlation_uuid;
        match invalid {
            "epoch" => {
                tidebreak_core::db::code::bump_spawn_epoch(&db, session_id, None)
                    .await
                    .unwrap();
            }
            "turn" => {
                turn.id = TurnId::new();
                turn.ordinal = 2;
                insert_turn(&db, &admission.owner, &turn).await.unwrap();
                sink.set_turn(turn.id);
            }
            "terminal" => {
                turn.status = TurnStatus::Completed;
                turn.ended_at = Some(Utc::now());
                tidebreak_core::db::code::save_turn(&db, &admission.owner, &turn)
                    .await
                    .unwrap();
            }
            "remote_claim" => assert!(tidebreak_core::db::code::claim_external_steer_target(
                &db,
                &admission.owner,
                session_id,
                &admission.event_id,
                admission.expected_turn_id,
                correlation_uuid.unwrap(),
                "remote-sandbox",
                2,
                uuid::Uuid::new_v4(),
            )
            .await
            .unwrap()),
            "correlation" => correlation_uuid = Some(uuid::Uuid::new_v4()),
            "owner" => {
                Arc::get_mut(&mut sink).unwrap().owner = OwnerId::new("foreign-owner").unwrap()
            }
            "session" => Arc::get_mut(&mut sink).unwrap().session_id = SessionId::new(),
            "queued" => assert!(tidebreak_core::db::code::settle_external_steer_admission(
                &db,
                &admission.owner,
                session_id,
                &admission.event_id,
                admission.expected_turn_id,
                ExternalSteerAdmission::Queued,
                Some(ExternalSteerQueuedReason::SteerUnsupported),
            )
            .await
            .unwrap()),
            "location" => {}
            _ => unreachable!(),
        }
        sink.emit(HarnessEvent::UserSteered {
            text: row.message,
            correlation_uuid,
        })
        .await;
        let expected = (invalid == "queued").then_some(ExternalSteerAdmission::Queued);
        assert!(
            matches!(worker_steer_receipt(&admission).await,
            ExternalMessageRecord::Replay { admission, .. } if admission == expected),
            "{invalid}"
        );
        assert_eq!(
            tidebreak_core::db::code::list_queued_turns(&db, &admission.owner, session_id)
                .await
                .unwrap()
                .len(),
            1,
            "{invalid}"
        );
        let events = list_events(&db, &admission.owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events;
        assert!(
            !events
                .iter()
                .any(|item| matches!(item.event, Event::UserSteered { .. })),
            "{invalid}"
        );
    }
}

#[tokio::test]
async fn durable_steer_stream_ack_waits_for_receipt_commit_before_publishing() {
    use sea_orm::{ConnectionTrait, TransactionTrait};
    let (directory, db, sink, session_id) = seeded_sink().await;
    let (admission, row) = pending_worker_steer(&db, session_id).await;
    insert_running_steer_target(&db, &admission).await;
    sink.set_turn(admission.expected_turn_id);
    let writer = sea_orm::Database::connect(format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("t.db").display(),
    ))
    .await
    .unwrap();
    let transaction = writer.begin().await.unwrap();
    transaction
        .execute_unprepared(
            "UPDATE session SET unrecognized_event_count = unrecognized_event_count",
        )
        .await
        .unwrap();
    let (mut live, _) = sink.bus.attach(session_id);
    let mut emitting = tokio::spawn(async move {
        sink.emit(HarnessEvent::UserSteered {
            text: row.message,
            correlation_uuid: admission.correlation_uuid,
        })
        .await;
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut emitting)
            .await
            .is_err()
    );
    assert!(matches!(
        live.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    transaction.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), emitting)
        .await
        .unwrap()
        .unwrap();
    assert!(
        tidebreak_core::db::code::list_queued_turns(&db, &OwnerId::local(), session_id)
            .await
            .unwrap()
            .is_empty()
    );
    let events = list_events(&db, &OwnerId::local(), session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap()
        .events;
    assert_eq!(
        events
            .iter()
            .filter(|item| matches!(item.event, Event::UserSteered { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn durable_steer_native_ack_after_handoff_cannot_settle_the_receipt() {
    use tidebreak_core::code::ExternalMessageRecord;
    let (_directory, db, _, session_id) = seeded_session(HarnessKind::Codex, None).await;
    let (admission, row) = pending_worker_steer(&db, session_id).await;
    insert_running_steer_target(&db, &admission).await;
    let engine = Arc::new(SteerControlHarness::new(SteerControlAnswer::Ack));
    let (reply, response) = oneshot::channel();
    let control = {
        let engine = Arc::clone(&engine);
        let admission = admission.clone();
        tokio::spawn(async move {
            apply_control(
                engine.as_ref(),
                WorkerCommand::Steer {
                    expected_turn_id: admission.expected_turn_id,
                    message: row.message,
                    admission: Some(admission.clone()),
                    reply,
                },
                Some(admission.expected_turn_id),
                None,
            )
            .await;
        })
    };
    engine.entered.notified().await;
    tidebreak_core::db::code::bump_spawn_epoch(&db, session_id, None)
        .await
        .unwrap();
    engine.acknowledge.notify_one();
    assert!(matches!(
        response.await.unwrap(),
        Err(WorkerError::Failed(_))
    ));
    control.await.unwrap();
    assert_eq!(
        engine.events.lock().unwrap().len(),
        1,
        "native ACK was delivered"
    );
    assert!(matches!(
        worker_steer_receipt(&admission).await,
        ExternalMessageRecord::Replay {
            admission: None,
            ..
        }
    ));
    assert_eq!(
        tidebreak_core::db::code::list_queued_turns(&db, &admission.owner, session_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(queued_turn_head(&db, &admission.owner, session_id)
        .await
        .unwrap()
        .is_none());
}
