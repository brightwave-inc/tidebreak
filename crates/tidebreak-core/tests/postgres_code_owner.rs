#![cfg(feature = "postgres")]

//! Owner scoping for code mode, on the backend a shared deployment runs.
//!
//! The SQLite suite in `db::tests::code` proves the same partitioning, but a
//! shared machine is exactly the case owner scoping exists for (decisions 47
//! and 48 step 1), and that machine runs PostgreSQL. The filters here are
//! generated SQL, so backends can disagree — an owner column that is not
//! actually compared would still pass on one and not the other.
//!
//! Skipped silently without `TIDEBREAK_POSTGRES_TEST_URL`, and hard-failed in
//! CI, where `TIDEBREAK_REQUIRE_POSTGRES_TEST` is set.

use chrono::Utc;
use tidebreak_core::db::code::{
    append_event, get_approval, get_repo, get_repo_by_root_path, get_session, get_turn,
    get_workspace, insert_approval, insert_repo, insert_session, insert_turn, insert_workspace,
    list_approvals, list_events, list_repos, list_sessions, list_turns, mark_repo_removed,
    save_turn, set_workspace_title_if, MAX_REPLAY_EVENTS,
};
use tidebreak_core::{
    Approval, ApprovalId, ApprovalKind, ApprovalState, Attention, AttentionSource, CodeRepo,
    CodeWorkspace, CodeWorkspaceStatus, DbStore, Event, HarnessKind, OwnerId, PermissionMode,
    RepoId, Session, SessionId, SessionKind, SessionLifecycle, Turn, TurnId, TurnParkWait,
    TurnStatus, WorkspaceId,
};

static POSTGRES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Seed one owner's whole code graph and hand back the ids.
async fn seed_owner(
    store: &DbStore,
    owner: &OwnerId,
    label: &str,
) -> (RepoId, WorkspaceId, SessionId, TurnId) {
    seed_owner_with_execution(store, owner, label, false).await
}

async fn seed_owner_with_execution(
    store: &DbStore,
    owner: &OwnerId,
    label: &str,
    managed: bool,
) -> (RepoId, WorkspaceId, SessionId, TurnId) {
    let repo_id = RepoId::new();
    insert_repo(
        store,
        &CodeRepo {
            id: repo_id,
            owner: owner.clone(),
            root_path: format!("/srv/{label}-repo"),
            display_name: label.to_owned(),
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
        store,
        &CodeWorkspace {
            id: workspace_id,
            owner: owner.clone(),
            repo_id,
            title: "first".into(),
            worktree_path: format!("/srv/{label}-worktree"),
            branch_name: format!("tidebreak/{label}"),
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
        store,
        &Session {
            visibility: tidebreak_core::SessionVisibility::Private,
            id: session_id,
            owner: owner.clone(),
            owner_kind: None,
            workspace_id: Some(workspace_id),
            kind: SessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: if managed {
                PermissionMode::Allow
            } else {
                PermissionMode::Ask
            },
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: if managed {
                SessionLifecycle::Running
            } else {
                SessionLifecycle::Idle
            },
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
            execution_location: if managed {
                tidebreak_core::ExecutionLocation::Sandbox
            } else {
                tidebreak_core::ExecutionLocation::Machine
            },
            acts_as: None,
        },
    )
    .await
    .unwrap();
    let turn_id = TurnId::new();
    insert_turn(
        store,
        owner,
        &Turn {
            actor: None,
            id: turn_id,
            session_id,
            ordinal: 1,
            status: TurnStatus::Running,
            model: None,
            fast_mode: false,
            user_input: format!("{label} asked for something"),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            rewrite: None,
            park_ref: None,
            park_wait: None,
            started_at: Utc::now(),
            ended_at: None,
        },
    )
    .await
    .unwrap();
    (repo_id, workspace_id, session_id, turn_id)
}

#[tokio::test]
async fn postgres_code_queries_partition_by_owner() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    // Unique keys per run: the lane shares one database across tests.
    let run = uuid::Uuid::new_v4().simple().to_string();
    let alice = OwnerId::new(&format!("alice-{run}")).unwrap();
    let bob = OwnerId::new(&format!("bob-{run}")).unwrap();
    let (alice_repo, alice_workspace, alice_session, alice_turn) =
        seed_owner(&store, &alice, &format!("alice-{run}")).await;
    let (_bob_repo, _bob_workspace, bob_session, _bob_turn) =
        seed_owner(&store, &bob, &format!("bob-{run}")).await;

    // Each owner's listings hold only their own rows.
    let alice_repos = list_repos(&store, &alice).await.unwrap();
    assert_eq!(alice_repos.len(), 1);
    assert_eq!(alice_repos[0].id, alice_repo);
    assert_eq!(list_sessions(&store, &bob).await.unwrap().len(), 1);

    // By-id reads across owners resolve to nothing on this backend too.
    assert!(get_repo(&store, &bob, alice_repo).await.unwrap().is_none());
    assert!(get_workspace(&store, &bob, alice_workspace)
        .await
        .unwrap()
        .is_none());
    assert!(get_session(&store, &bob, alice_session)
        .await
        .unwrap()
        .is_none());
    assert!(get_turn(&store, &bob, alice_turn).await.unwrap().is_none());
    assert!(get_session(&store, &alice, bob_session)
        .await
        .unwrap()
        .is_none());
    assert!(list_turns(&store, &bob, alice_session)
        .await
        .unwrap()
        .is_empty());

    // The journal.
    append_event(
        &store,
        &alice,
        alice_session,
        0,
        &Event::TurnInterrupted { usage: None },
    )
    .await
    .unwrap();
    assert_eq!(
        list_events(&store, &alice, alice_session, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events
            .len(),
        1
    );
    assert!(
        list_events(&store, &bob, alice_session, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events
            .is_empty()
    );

    // Approvals.
    let approval_id = ApprovalId::new();
    insert_approval(
        &store,
        &alice,
        &Approval {
            actor: None,
            id: approval_id,
            session_id: alice_session,
            turn_id: alice_turn,
            kind: ApprovalKind::FileWrite {
                paths: vec!["secret.txt".into()],
            },
            harness_raw: serde_json::json!({"tool":"Write"}),
            native_call_id: Some("toolu_postgres".into()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(0),
            decision_claim: None,
            claimed_at: None,
            state: ApprovalState::Pending,
            feedback: None,
            requested_at: Utc::now(),
            decided_at: None,
            auto_judge_status: None,
        },
    )
    .await
    .unwrap();
    assert!(get_approval(&store, &bob, approval_id)
        .await
        .unwrap()
        .is_none());
    assert!(list_approvals(&store, &bob, None, Some(alice_session))
        .await
        .unwrap()
        .is_empty());

    // Writes do not cross either.
    assert!(
        !set_workspace_title_if(&store, &bob, alice_workspace, "first", "stolen")
            .await
            .unwrap()
    );
    assert_eq!(
        get_workspace(&store, &alice, alice_workspace)
            .await
            .unwrap()
            .unwrap()
            .title,
        "first"
    );
}

/// The digest's proposal count matches provenance in the database, and that
/// SQL is PostgreSQL's own: jsonb operators rather than SQLite's `json_each`.
#[tokio::test]
async fn postgres_counts_only_the_proposals_a_session_journal_justifies() {
    use tidebreak_core::db::code::count_session_memory_proposals;
    use tidebreak_core::memory::{
        MemoryAuthor, MemoryEvidence, MemoryKind, MemoryOrigin, MemoryProvenance, MemoryRecord,
        MemoryRecordId, MemoryScope, MemoryStatus,
    };
    use tidebreak_core::MemoryBackend;

    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let owner = OwnerId::new(&format!("memory-{run}")).unwrap();
    let (_, _, session, _) = seed_owner(&store, &owner, &format!("memory-{run}")).await;
    let (_, _, other, _) = seed_owner(&store, &owner, &format!("memory-other-{run}")).await;
    for id in [session, other] {
        append_event(
            &store,
            &owner,
            id,
            0,
            &Event::AssistantMessage {
                text: "Use the staging database for migrations.".to_owned(),
                parent_call_id: None,
            },
        )
        .await
        .unwrap();
    }
    let proposal = |origin: Option<SessionId>, cited: SessionId, status: MemoryStatus| {
        let now = Utc::now();
        MemoryRecord {
            id: MemoryRecordId::new(),
            scope: MemoryScope::Personal,
            kind: MemoryKind::Fact,
            status,
            title: "Staging".to_owned(),
            body: "Migrate on staging.".to_owned(),
            provenance: MemoryProvenance {
                author: MemoryAuthor::Model,
                origin: MemoryOrigin {
                    code_session_id: origin,
                    ..Default::default()
                },
                evidence: vec![MemoryEvidence::Event {
                    session_id: cited,
                    seq: 1,
                }],
            },
            links: Vec::new(),
            expires_at: None,
            superseded_by: None,
            observation_count: 0,
            revision: 1,
            created_at: now,
            updated_at: now,
        }
    };
    for record in [
        proposal(Some(session), session, MemoryStatus::Proposed),
        proposal(Some(session), session, MemoryStatus::Proposed),
        proposal(Some(session), other, MemoryStatus::Proposed),
        proposal(None, session, MemoryStatus::Proposed),
        proposal(Some(other), session, MemoryStatus::Proposed),
        proposal(Some(session), session, MemoryStatus::Active),
    ] {
        store.put(&owner, record).await.unwrap();
    }

    assert_eq!(
        count_session_memory_proposals(&store, &owner, session)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        count_session_memory_proposals(&store, &owner, other)
            .await
            .unwrap(),
        0
    );
}

/// Repository registration is unique per owner on PostgreSQL as well: the
/// composite index is what allows a second user to register a path the first
/// user already has.
/// The park columns and the widened status check are the one piece of this
/// slice whose PostgreSQL branch is a different statement from SQLite's: the
/// constraint is swapped in place rather than rebuilt with the table. A turn
/// that cannot park on this backend would strand the engine's checkpoint.
#[tokio::test]
async fn postgres_stores_a_parked_turn_and_reads_its_wait_back() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let owner = OwnerId::new(&format!("parker-{run}")).unwrap();
    let (_repo, _workspace, _session, turn_id) =
        seed_owner(&store, &owner, &format!("parker-{run}")).await;

    let mut turn = get_turn(&store, &owner, turn_id).await.unwrap().unwrap();
    assert_eq!(turn.park_ref, None);
    turn.status = TurnStatus::Waiting;
    turn.park_ref = Some("cp-1".to_owned());
    turn.park_wait = Some(TurnParkWait::Approval {
        call_id: "call-1".to_owned(),
    });
    assert!(save_turn(&store, &owner, &turn).await.unwrap());

    let parked = get_turn(&store, &owner, turn_id).await.unwrap().unwrap();
    assert_eq!(parked.status, TurnStatus::Waiting);
    assert_eq!(parked.park_ref.as_deref(), Some("cp-1"));
    assert_eq!(
        parked.park_wait,
        Some(TurnParkWait::Approval {
            call_id: "call-1".to_owned()
        })
    );
}

#[tokio::test]
async fn postgres_repository_paths_are_unique_per_owner() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let alice = OwnerId::new(&format!("alice-{run}")).unwrap();
    let bob = OwnerId::new(&format!("bob-{run}")).unwrap();
    let shared_path = format!("/srv/shared-{run}");

    let repo = |owner: &OwnerId| CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: shared_path.clone(),
        display_name: "shared".into(),
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
    };
    let removed = repo(&alice);
    insert_repo(&store, &removed).await.unwrap();
    insert_repo(&store, &repo(&bob)).await.unwrap();

    let found = get_repo_by_root_path(&store, &bob, &shared_path)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.owner, bob);

    // The same owner registering the same path twice is still refused.
    assert!(insert_repo(&store, &repo(&alice)).await.is_err());

    // A removed row preserves old history but releases the path for a fresh
    // live registration.
    assert!(mark_repo_removed(&store, &alice, removed.id, Utc::now())
        .await
        .unwrap());
    let replacement = repo(&alice);
    insert_repo(&store, &replacement).await.unwrap();
    let found = get_repo_by_root_path(&store, &alice, &shared_path)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, replacement.id);
}

#[tokio::test]
async fn postgres_native_tool_receipts_claim_once_and_replay() {
    use tidebreak_core::db::code::*;
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Ok(url) = std::env::var("TIDEBREAK_POSTGRES_TEST_URL") else {
        assert!(
            std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_none(),
            "TIDEBREAK_POSTGRES_TEST_URL is required"
        );
        return;
    };
    let store = DbStore::connect(&url).await.unwrap();
    let label = format!("receipt-{}", uuid::Uuid::new_v4());
    let owner = OwnerId::new(&label).unwrap();
    let (_, _, session, _) = seed_owner(&store, &owner, &label).await;
    let grant = mint_external_grant(
        &store,
        &owner,
        MintGrantSubject {
            channel_kind: "slack",
            external_identity: &label,
            workspace_identity: "receipts-test",
            kind: tidebreak_core::code::CodeGrantKind::Person,
        },
        &"a".repeat(64),
        &"b".repeat(64),
    )
    .await
    .unwrap();
    bind_external_session(&store, &owner, grant.id, "slack", &label, session)
        .await
        .unwrap();
    let tidebreak_core::code::IncarnationAdmission::Admitted(inc) =
        create_incarnation_intent(&store, &owner, session, 1, 10)
            .await
            .unwrap()
    else {
        panic!("expected admission")
    };
    activate_incarnation(&store, &owner, inc.id, "test-receipts")
        .await
        .unwrap();
    let arguments = serde_json::json!({"task":"inspect", "repo":"one"});
    let receipt = enqueue_native_tool_request(
        &store,
        &owner,
        session,
        inc.id,
        "one",
        "code_run_turn",
        &arguments,
    )
    .await
    .unwrap();
    let same = enqueue_native_tool_request(
        &store,
        &owner,
        session,
        inc.id,
        "one",
        "code_run_turn",
        &serde_json::json!({"repo":"one", "task":"inspect"}),
    )
    .await
    .unwrap();
    assert_eq!(same.call_id, receipt.call_id);
    assert!(enqueue_native_tool_request(
        &store,
        &owner,
        session,
        inc.id,
        "one",
        "code_run_turn",
        &serde_json::json!({})
    )
    .await
    .is_err());
    let (a, b) = tokio::join!(
        claim_native_tool_request(&store, &owner, &receipt),
        claim_native_tool_request(&store, &owner, &same)
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|v| matches!(v, NativeToolClaim::Claimed(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|v| matches!(v, NativeToolClaim::Running(_)))
            .count(),
        1
    );
    let result = serde_json::json!({"request_id":"one", "output":{"ok":true}, "artifacts":[]});
    let completed = complete_native_tool_request(&store, &owner, &receipt, &result)
        .await
        .unwrap();
    let NativeToolClaim::Completed(cached) = claim_native_tool_request(&store, &owner, &receipt)
        .await
        .unwrap()
    else {
        panic!("expected cached result")
    };
    assert_eq!(cached.result, Some(result));
    assert_eq!(cached.call_id, receipt.call_id);
    mark_native_tool_request_delivered(&store, &owner, &completed)
        .await
        .unwrap();
    assert!(list_native_tool_requests(&store, &owner, session, inc.id)
        .await
        .unwrap()
        .is_empty());
    revoke_external_grant(&store, &owner, grant.id, "test revoked")
        .await
        .unwrap();
    assert!(claim_native_tool_request(&store, &owner, &receipt)
        .await
        .is_err());
}

#[tokio::test]
async fn postgres_managed_human_decisions_serialize_answers_and_cancellation() {
    use tidebreak_core::code::{SupervisorToolRequest, SupervisorToolTurn};
    use tidebreak_core::db::code::*;
    use tidebreak_core::storage::Store;
    use tidebreak_core::{ApprovalDecisionKind, TurnActor, UserQuestionAnswer};
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Ok(url) = std::env::var("TIDEBREAK_POSTGRES_TEST_URL") else {
        assert!(
            std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_none(),
            "TIDEBREAK_POSTGRES_TEST_URL is required"
        );
        return;
    };
    let store = DbStore::connect(&url).await.unwrap();
    let label = format!("human-{}", uuid::Uuid::new_v4());
    let owner = OwnerId::new(&label).unwrap();
    let (_, _, session_id, _) = seed_owner_with_execution(&store, &owner, &label, true).await;
    let grant = mint_external_grant(
        &store,
        &owner,
        MintGrantSubject {
            channel_kind: "slack",
            external_identity: &label,
            workspace_identity: "human-test",
            kind: tidebreak_core::code::CodeGrantKind::Person,
        },
        &uuid::Uuid::new_v4().simple().to_string().repeat(2),
        &uuid::Uuid::new_v4().simple().to_string().repeat(2),
    )
    .await
    .unwrap();
    bind_external_session(&store, &owner, grant.id, "slack", &label, session_id)
        .await
        .unwrap();
    let tidebreak_core::IncarnationAdmission::Admitted(inc) =
        create_incarnation_intent(&store, &owner, session_id, 1, 10)
            .await
            .unwrap()
    else {
        panic!("expected admission")
    };
    activate_incarnation(&store, &owner, inc.id, "human-fixture")
        .await
        .unwrap();
    let identity = SupervisorToolTurn {
        native_turn: 1,
        runtime_id: uuid::Uuid::new_v4(),
    };
    store.set_setting(&format!("code.incarnations.{}.steering_protocol",inc.id),&serde_json::json!({"version":1,"runtime_id":identity.runtime_id,"sandbox_id":"human-fixture"})).await.unwrap();
    let mut request = SupervisorToolRequest {
        request_id: "human".into(),
        tool: "ask_user_questions".into(),
        arguments: serde_json::json!({"questions":[{"id":"target","header":"Target","question":"Which target?","allow_free_form":true}]}),
        turn: Some(identity),
        cancelled: false,
    };
    let (approval, _) = enqueue_managed_decision(&store, &owner, session_id, inc.id, &request)
        .await
        .unwrap();
    assert_eq!(
        enqueue_managed_decision(&store, &owner, session_id, inc.id, &request)
            .await
            .unwrap(),
        (approval, None)
    );
    let response = |text: &str| ApprovalDecisionKind::Answered {
        answers: vec![UserQuestionAnswer {
            question_id: "target".into(),
            selected_option_ids: vec![],
            custom_answer: Some(text.into()),
        }],
    };
    let (a, b) = tokio::join!(
        settle_managed_decision(
            &store,
            &owner,
            approval,
            response("first"),
            Some(TurnActor {
                display: Some("Alex".into()),
                ..Default::default()
            })
        ),
        settle_managed_decision(
            &store,
            &owner,
            approval,
            response("second"),
            Some(TurnActor {
                display: Some("Blair".into()),
                ..Default::default()
            })
        ),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let actor = get_approval(&store, &owner, approval)
        .await
        .unwrap()
        .unwrap()
        .actor;
    assert!(actor.is_some());
    assert_eq!(
        managed_decision_results(&store, &owner, session_id, inc.id)
            .await
            .unwrap()
            .len(),
        1
    );
    request.cancelled = true;
    assert!(
        cancel_managed_decision(&store, &owner, session_id, inc.id, &request)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        get_approval(&store, &owner, approval)
            .await
            .unwrap()
            .unwrap()
            .actor,
        actor
    );
    assert!(managed_decision_results(&store, &owner, session_id, inc.id)
        .await
        .unwrap()
        .is_empty());
    request.request_id = "never-admitted".into();
    cancel_managed_decision(&store, &owner, session_id, inc.id, &request)
        .await
        .unwrap();
    request.cancelled = false;
    assert!(
        enqueue_managed_decision(&store, &owner, session_id, inc.id, &request)
            .await
            .is_err()
    );
    assert_eq!(
        list_approvals(&store, &owner, None, Some(session_id))
            .await
            .unwrap()
            .len(),
        1
    );
}

/// Sixteen reads of pull request 7: versions one to four, the head moving at
/// version three and never coming back, and every group observed at its own
/// time. Their keys do not follow their order in the list.
fn converging_reads(owner: &OwnerId, repo_name: &str) -> Vec<tidebreak_core::PullRequestRead> {
    use chrono::TimeZone;
    use tidebreak_core::{
        CodePullRequestState, PullRequestCheck, PullRequestCheckBucket, PullRequestChecksRead,
        PullRequestMergeability, PullRequestObjectRead, PullRequestRead, PullRequestReviewRead,
        PullRequestSnapshot,
    };

    let base = Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap();
    let at = |seconds: i64| base + chrono::Duration::seconds(seconds);
    (0..16i64)
        .map(|index| {
            let version = 1 + index % 4;
            let head = if version >= 3 { "bbb222" } else { "aaa111" };
            let mut read = PullRequestRead::new(owner.clone(), "github.com", "acme", repo_name, 7);
            read.object = Some(PullRequestObjectRead {
                snapshot: PullRequestSnapshot {
                    url: format!("https://github.com/acme/{repo_name}/pull/7"),
                    title: format!("Version {version}"),
                    state: CodePullRequestState::Open,
                    draft: false,
                    author: Some("octocat".into()),
                    head_branch: "feature".into(),
                    base_branch: "main".into(),
                    head_sha: Some(head.into()),
                    created_at: at(0),
                    updated_at: at(version),
                    merged_at: None,
                    closed_at: None,
                },
                mergeability: (index % 3 != 1).then(|| PullRequestMergeability {
                    mergeable: Some(
                        if index % 2 == 0 {
                            "mergeable"
                        } else {
                            "conflicting"
                        }
                        .into(),
                    ),
                    merge_state_status: Some(if index % 2 == 0 { "clean" } else { "dirty" }.into()),
                }),
                auto_merge_enabled: Some(index % 5 == 0),
                observed_at: at(1_000 + (index * 5) % 16),
                etag: None,
            });
            read.checks = (index % 2 == 0).then(|| PullRequestChecksRead {
                head_sha: Some(head.into()),
                checks: vec![PullRequestCheck {
                    name: "ci".into(),
                    bucket: if (index / 4) % 2 == 0 {
                        PullRequestCheckBucket::Pass
                    } else {
                        PullRequestCheckBucket::Fail
                    },
                    detail: None,
                    url: None,
                }],
                observed_at: at(2_000 + (index * 7) % 16),
                etag: None,
            });
            read.review = (index % 3 != 2).then(|| PullRequestReviewRead {
                decision: (index % 3 == 1).then(|| "changes_requested".to_owned()),
                observed_at: at(3_000 + (index * 3) % 16),
                etag: None,
            });
            read
        })
        .collect()
}

/// Reads of one pull request land at once on the backend a shared deployment
/// runs, where writers really are concurrent. The row lock serializes them, a
/// racing first sighting merges instead of failing on the unique identity,
/// and the row ends where the same reads applied in any order put it.
#[tokio::test]
async fn postgres_concurrent_pull_request_reads_converge() {
    use tidebreak_core::db::code::{
        adopt_workspace_pull_request, get_stored_pull_request, save_pull_request_read,
        PullRequestReadOptions,
    };
    use tidebreak_core::{
        merge_pull_request_read, CodePullRequestId, PullRequestDigest, PullRequestSnapshot,
        StoredPullRequest,
    };

    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let owner = OwnerId::new(&format!("merger-{run}")).unwrap();
    let (_repo, workspace_id, _session, _turn) =
        seed_owner(&store, &owner, &format!("merger-{run}")).await;
    let repo_name = format!("tools-{run}");
    let shown: PullRequestDigest = serde_json::from_value(serde_json::json!({
        "number": 7,
        "url": format!("https://github.com/acme/{repo_name}/pull/7"),
        "state": "open"
    }))
    .unwrap();
    assert!(
        adopt_workspace_pull_request(&store, &owner, workspace_id, &shown)
            .await
            .unwrap()
    );

    let reads = converging_reads(&owner, &repo_name);
    let landed: Vec<_> = reads
        .iter()
        .cloned()
        .map(|read| {
            let store = store.clone();
            tokio::spawn(async move {
                save_pull_request_read(
                    &store,
                    &read,
                    PullRequestReadOptions {
                        mint_row: true,
                        adopt: None,
                    },
                )
                .await
                .unwrap()
                .unwrap()
            })
        })
        .collect();
    for applied in landed {
        assert!(applied.await.unwrap().stored);
    }

    let expected = reads.iter().fold(
        StoredPullRequest::first_sighting(&reads[0], CodePullRequestId::new()).unwrap(),
        |state, read| merge_pull_request_read(&state, read),
    );
    let stored = get_stored_pull_request(&store, &owner, "github.com", "acme", &repo_name, 7)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        PullRequestSnapshot::of_fact(&stored.fact),
        PullRequestSnapshot::of_fact(&expected.fact)
    );
    assert_eq!(stored.fact.head_sha.as_deref(), Some("bbb222"));
    assert_eq!(stored.fact.last_seen_at, expected.fact.last_seen_at);
    assert_eq!(stored.fact.live, expected.fact.live);
    assert_eq!(stored.observed, expected.observed);
    assert_eq!(
        get_workspace(&store, &owner, workspace_id)
            .await
            .unwrap()
            .unwrap()
            .pr,
        Some(stored.fact.digest()),
        "the workspace column is the projection of the row"
    );
}
