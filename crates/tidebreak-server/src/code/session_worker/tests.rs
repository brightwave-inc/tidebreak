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
            execution_location: tidebreak_core::ExecutionLocation::Machine,
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
    let (directory, store, bus, session_id) =
        seeded_session(HarnessKind::ClaudeCode, Some("2.1.237")).await;
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
