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
    set_workspace_title_if, MAX_REPLAY_EVENTS,
};
use tidebreak_core::{
    Attention, AttentionSource, CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState,
    CodeEvent, CodePermissionMode, CodeRepo, CodeSession, CodeSessionId, CodeSessionKind,
    CodeSessionLifecycle, CodeTurn, CodeTurnId, CodeTurnStatus, CodeWorkspace, CodeWorkspaceStatus,
    DbStore, HarnessKind, OwnerId, RepoId, WorkspaceId,
};

static POSTGRES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Seed one owner's whole code graph and hand back the ids.
async fn seed_owner(
    store: &DbStore,
    owner: &OwnerId,
    label: &str,
) -> (RepoId, WorkspaceId, CodeSessionId, CodeTurnId) {
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
        },
    )
    .await
    .unwrap();
    let session_id = CodeSessionId::new();
    insert_session(
        store,
        &CodeSession {
            id: session_id,
            owner: owner.clone(),
            workspace_id,
            kind: CodeSessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: CodePermissionMode::Ask,
            model: None,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    let turn_id = CodeTurnId::new();
    insert_turn(
        store,
        owner,
        &CodeTurn {
            id: turn_id,
            session_id,
            ordinal: 1,
            status: CodeTurnStatus::Running,
            user_input: format!("{label} asked for something"),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
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
        &CodeEvent::TurnInterrupted,
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
    let approval_id = CodeApprovalId::new();
    insert_approval(
        &store,
        &alice,
        &CodeApproval {
            id: approval_id,
            session_id: alice_session,
            turn_id: alice_turn,
            kind: CodeApprovalKind::FileWrite {
                paths: vec!["secret.txt".into()],
            },
            harness_raw: serde_json::json!({"tool":"Write"}),
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: Utc::now(),
            decided_at: None,
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

/// Repository registration is unique per owner on PostgreSQL as well: the
/// composite index is what allows a second user to register a path the first
/// user already has.
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
