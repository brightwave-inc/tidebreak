use super::temp_store;
use crate::attention::{Attention, AttentionSource, FenceReason};
use crate::code::{
    CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState, CodeEvent, CodeQueuedTurn,
    CodeRepo, CodeSession, CodeSessionId, CodeSessionKind, CodeSessionLifecycle,
    CodeSubagentStatus, CodeSubagentSummary, CodeTurn, CodeTurnId, CodeTurnStatus, CodeWorkspace,
    CodeWorkspaceStatus, HarnessKind, PullRequestDigest, RepoId, WorkspaceId,
};
use crate::db::code::{
    abandon_pending_approval, abandon_pending_approvals_for_stopped_session, append_event,
    append_event_with_notification, begin_permission_mode_change, bump_spawn_epoch,
    cancel_permission_mode_change, claim_approval, clear_session_harness_resume_ref,
    confirm_permission_mode_change, delete_queued_turn, delete_session_queued_turns,
    discard_permission_mode_change, enqueue_queued_turn, fence_permission_mode_change,
    get_approval, get_repo, get_repo_by_root_path, get_session, get_turn, get_workspace,
    insert_approval, insert_approval_for_worker, insert_repo, insert_session, insert_turn,
    insert_workspace, list_approvals, list_events, list_pending_permission_mode_changes,
    list_queued_turns, list_repos, list_sessions, list_turn_metrics, list_turns, mark_repo_removed,
    promote_queued_turn, queue_paused, queued_turn_head, recover_interrupted_session,
    replace_session_attention, replace_session_execution_settings, save_session, save_turn,
    save_workspace, search_repo_transcripts, set_active_workspace_pull_request, set_queue_paused,
    set_session_harness_resume_ref, set_session_subagents, set_turn_narrative,
    set_workspace_title_if, settle_approval_claim, update_queued_turn, ClaimedApprovalSettlement,
    CodeJournalError, CodeSessionExecutionSettings, CodeTranscriptSearchSource, MAX_REPLAY_EVENTS,
};
use crate::{
    BlobRetirementStatus, ImageMediaType, ImageRef, OwnerId, PermissionMode, ReasoningEffort, Store,
};
use chrono::Utc;
use sea_orm::ConnectionTrait;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn trigger_payload(
    action: crate::code::CodeTriggerAction,
    condition: crate::code::CodeTriggerCondition,
) -> crate::code::CodeTriggerFirePayload {
    crate::code::CodeTriggerFirePayload {
        action,
        condition,
        message: format!("trigger test payload for {}", condition.as_str()),
    }
}

async fn seeded_session() -> (
    tempfile::TempDir,
    crate::db::DbStore,
    CodeSessionId,
    CodeTurnId,
) {
    let (dir, store) = temp_store().await;
    let (session_id, turn_id) = seed_owner(&store, &OwnerId::local(), "example").await;
    (dir, store, session_id, turn_id)
}

#[tokio::test]
async fn repository_transcript_search_survives_workspace_release() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    let mut workspace = get_workspace(&store, &owner, session.workspace_id)
        .await
        .unwrap()
        .unwrap();
    let repo_id = workspace.repo_id;
    workspace.status = CodeWorkspaceStatus::Released;
    workspace.archived_at = Some(now());
    workspace.released_at = Some(now());
    assert!(save_workspace(&store, &workspace).await.unwrap());

    let mut turn = get_turn(&store, &owner, turn_id).await.unwrap().unwrap();
    turn.user_input = "Find Needle % literal in the old conversation".into();
    assert!(save_turn(&store, &owner, &turn).await.unwrap());
    append_event(
        &store,
        &owner,
        session_id,
        0,
        &CodeEvent::AssistantMessage {
            text: "The archived result contains Needle % literal too".into(),
            parent_call_id: None,
        },
    )
    .await
    .unwrap();

    let (other_session, _) = seed_owner(&store, &owner, "other-repo").await;
    append_event(
        &store,
        &owner,
        other_session,
        0,
        &CodeEvent::AssistantMessage {
            text: "Needle % literal belongs to another repository".into(),
            parent_call_id: None,
        },
    )
    .await
    .unwrap();

    let searched = search_repo_transcripts(&store, &owner, repo_id, "needle % literal", 10)
        .await
        .unwrap();
    assert!(!searched.truncated);
    assert_eq!(searched.matches.len(), 2);
    assert!(searched.matches.iter().all(|matched| {
        matched.workspace_id == workspace.id
            && matched.workspace_title == workspace.title
            && matched.session_id == session_id
            && matched.preview.to_lowercase().contains("needle % literal")
    }));
    assert!(searched.matches.iter().any(|matched| {
        matched.source == CodeTranscriptSearchSource::TurnUserInput
            && matched.turn_id == Some(turn_id)
    }));
    assert!(searched.matches.iter().any(|matched| {
        matched.source == CodeTranscriptSearchSource::Event && matched.turn_id.is_none()
    }));

    let hidden = search_repo_transcripts(
        &store,
        &OwnerId::new("another-owner").unwrap(),
        repo_id,
        "needle % literal",
        10,
    )
    .await
    .unwrap();
    assert!(hidden.matches.is_empty());
}

/// Seed one owner's whole code-mode graph into an existing store: repo,
/// workspace, session, turn. Two calls with different owners give the
/// cross-owner fixture the isolation tests need.
async fn seed_owner(
    store: &crate::db::DbStore,
    owner: &OwnerId,
    label: &str,
) -> (CodeSessionId, CodeTurnId) {
    let repo_id = RepoId::new();
    insert_repo(
        store,
        &CodeRepo {
            id: repo_id,
            owner: owner.clone(),
            root_path: format!("/tmp/{label}-repo"),
            display_name: label.to_owned(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: now(),
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
            worktree_path: format!("/tmp/{label}-worktree"),
            branch_name: "tidebreak/first".into(),
            base_ref: "main".into(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: now(),
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
            harness_version: Some("2.1.233".into()),
            harness_resume_ref: None,
            permission_mode: PermissionMode::Ask,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: now(),
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
            model: None,
            fast_mode: false,
            user_input: "hello".into(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            started_at: now(),
            ended_at: None,
        },
    )
    .await
    .unwrap();
    (session_id, turn_id)
}

async fn claimed_trigger_delivery(
    store: &crate::db::DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    condition: crate::code::CodeTriggerCondition,
    action: crate::code::CodeTriggerAction,
) -> (crate::code::CodeTriggerDeliveryId, uuid::Uuid) {
    use crate::code::{CodeTrigger, CodeTriggerFireIdentity, CodeTriggerId};
    use crate::db::code::{
        arm_trigger, insert_or_load_trigger_fire, lease_trigger_fire_delivery,
        list_triggers_for_repo,
    };

    let session = get_session(store, owner, session_id)
        .await
        .unwrap()
        .unwrap();
    let workspace = get_workspace(store, owner, session.workspace_id)
        .await
        .unwrap()
        .unwrap();
    let at = now();
    arm_trigger(
        store,
        owner,
        &CodeTrigger {
            id: CodeTriggerId::new(),
            owner: owner.clone(),
            repo_id: workspace.repo_id,
            condition,
            action,
            enabled: true,
            created_at: at,
            updated_at: at,
        },
    )
    .await
    .unwrap();
    let trigger = list_triggers_for_repo(store, owner, workspace.repo_id)
        .await
        .unwrap()
        .into_iter()
        .find(|trigger| trigger.condition == condition)
        .unwrap();
    let fire = insert_or_load_trigger_fire(
        store,
        &CodeTriggerFireIdentity {
            trigger_id: trigger.id,
            owner: owner.clone(),
            workspace_id: session.workspace_id,
            pr_number: 42,
            head_sha: uuid::Uuid::new_v4().to_string(),
        },
        &trigger_payload(action, condition),
        at,
    )
    .await
    .unwrap();
    let fire = fire.expect("enabled trigger accepts a fire");
    let lease_token = uuid::Uuid::new_v4();
    lease_trigger_fire_delivery(
        store,
        owner,
        fire.delivery_id,
        lease_token,
        at,
        at + chrono::Duration::minutes(10),
    )
    .await
    .unwrap()
    .unwrap();
    (fire.delivery_id, lease_token)
}

/// Subagent visibility rides the session row (decision 52): the targeted
/// write must round-trip, survive a full-row save from a stale in-memory
/// copy, and read back empty for rows that never had any.
#[tokio::test]
async fn session_subagents_round_trip_through_the_targeted_write() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(session.subagents.is_empty());
    let subagents = vec![CodeSubagentSummary {
        call_id: "toolu_task".into(),
        name: "Find the config parser".into(),
        status: CodeSubagentStatus::Running,
    }];
    assert!(
        set_session_subagents(&store, &owner, session_id, &subagents)
            .await
            .unwrap()
    );
    // A concurrent full-row save from a copy that predates the targeted
    // write must not clobber the list.
    assert!(save_session(&store, &session).await.unwrap());
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .subagents,
        subagents
    );
    // Another owner's session is indistinguishable from a missing one.
    assert!(!set_session_subagents(
        &store,
        &OwnerId::new("other").unwrap(),
        session_id,
        &subagents
    )
    .await
    .unwrap());
}

/// A worker can learn its resume ref after it snapshots the session row. A
/// later pid save from that stale copy must preserve the targeted write, while
/// an engine rejection still needs an explicit way to clear the ref.
#[tokio::test]
async fn stale_session_saves_preserve_resume_refs_until_an_explicit_clear() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let mut stale = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    stale.lifecycle = CodeSessionLifecycle::Running;
    assert!(save_session(&store, &stale).await.unwrap());
    assert!(
        set_session_harness_resume_ref(&store, &owner, session_id, 0, "session-ref")
            .await
            .unwrap()
    );

    stale.child_pid = Some(4242);
    assert!(save_session(&store, &stale).await.unwrap());
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .harness_resume_ref
            .as_deref(),
        Some("session-ref")
    );

    assert!(
        clear_session_harness_resume_ref(&store, &owner, session_id, 0)
            .await
            .unwrap()
    );
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .harness_resume_ref,
        None
    );
}

#[tokio::test]
async fn execution_settings_replace_atomically_and_survive_stale_worker_saves() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let stale = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    let next = CodeSessionExecutionSettings {
        model: Some("claude-opus-5".into()),
        reasoning_effort: Some(ReasoningEffort::High),
        fast_mode: true,
    };
    let changed = replace_session_execution_settings(&store, &owner, &stale, &next)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(CodeSessionExecutionSettings::from(&changed), next);

    let conflicting = CodeSessionExecutionSettings {
        model: Some("other".into()),
        reasoning_effort: None,
        fast_mode: false,
    };
    assert!(
        replace_session_execution_settings(&store, &owner, &stale, &conflicting)
            .await
            .unwrap()
            .is_none()
    );

    let mut stale_worker = stale;
    stale_worker.child_pid = Some(4242);
    assert!(save_session(&store, &stale_worker).await.unwrap());
    let stored = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(CodeSessionExecutionSettings::from(&stored), next);
    assert_eq!(stored.child_pid, Some(4242));
}

#[tokio::test]
async fn permission_mode_changes_reserve_and_confirm_one_revision_at_a_time() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let stale = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    let intent = begin_permission_mode_change(&store, &owner, &stale, PermissionMode::Auto)
        .await
        .unwrap()
        .expect("the first change owns the row");
    assert_eq!(intent.revision, 1);
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .permission_mode,
        PermissionMode::Ask,
        "an intent is not a confirmed mode"
    );
    assert!(
        begin_permission_mode_change(&store, &owner, &stale, PermissionMode::Plan)
            .await
            .unwrap()
            .is_none(),
        "a second change cannot replace the pending revision"
    );
    assert!(confirm_permission_mode_change(&store, &owner, &intent)
        .await
        .unwrap());

    let confirmed = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(confirmed.permission_mode, PermissionMode::Auto);
    assert!(list_pending_permission_mode_changes(&store, &owner)
        .await
        .unwrap()
        .is_empty());

    assert!(save_session(&store, &stale).await.unwrap());
    let confirmed = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        confirmed.permission_mode,
        PermissionMode::Auto,
        "a stale full-row save cannot restore the previous mode"
    );
    let next = begin_permission_mode_change(&store, &owner, &confirmed, PermissionMode::Plan)
        .await
        .unwrap()
        .expect("the confirmed revision releases the next change");
    assert_eq!(next.revision, 2);
}

#[tokio::test]
async fn a_concurrent_session_end_makes_permission_mode_confirmation_zero_rows() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    let intent = begin_permission_mode_change(&store, &owner, &session, PermissionMode::Auto)
        .await
        .unwrap()
        .unwrap();

    session.lifecycle = CodeSessionLifecycle::Ended;
    assert!(save_session(&store, &session).await.unwrap());
    assert!(!confirm_permission_mode_change(&store, &owner, &intent)
        .await
        .unwrap());
    let ended = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ended.lifecycle, CodeSessionLifecycle::Ended);
    assert_eq!(ended.permission_mode, PermissionMode::Ask);
    assert!(discard_permission_mode_change(&store, &owner, &intent)
        .await
        .unwrap());
}

#[tokio::test]
async fn a_newer_worker_epoch_makes_permission_mode_confirmation_zero_rows() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    let intent = begin_permission_mode_change(&store, &owner, &session, PermissionMode::Auto)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(bump_spawn_epoch(&store, session_id, None).await.unwrap(), 1);
    assert!(!confirm_permission_mode_change(&store, &owner, &intent)
        .await
        .unwrap());
    let newer = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(newer.spawn_epoch, 1);
    assert_eq!(newer.permission_mode, PermissionMode::Ask);
    assert!(discard_permission_mode_change(&store, &owner, &intent)
        .await
        .unwrap());
}

#[tokio::test]
async fn another_owner_cannot_see_or_settle_permission_mode_changes() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("alice").unwrap();
    let bob = OwnerId::new("bob").unwrap();
    let (alice_session_id, _alice_turn) = seed_owner(&store, &alice, "alice").await;
    let (_bob_session_id, _bob_turn) = seed_owner(&store, &bob, "bob").await;
    let alice_session = get_session(&store, &alice, alice_session_id)
        .await
        .unwrap()
        .unwrap();
    let mut forged_session = alice_session.clone();
    forged_session.owner = bob.clone();

    assert!(
        begin_permission_mode_change(&store, &bob, &forged_session, PermissionMode::Auto)
            .await
            .unwrap()
            .is_none(),
        "the query must match the caller's owner in storage"
    );
    let intent = begin_permission_mode_change(&store, &alice, &alice_session, PermissionMode::Auto)
        .await
        .unwrap()
        .unwrap();

    assert!(list_pending_permission_mode_changes(&store, &bob)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        list_pending_permission_mode_changes(&store, &alice)
            .await
            .unwrap()
            .len(),
        1
    );
    let mut forged_intent = intent.clone();
    forged_intent.owner = bob.clone();
    assert!(
        !confirm_permission_mode_change(&store, &bob, &forged_intent)
            .await
            .unwrap()
    );
    assert!(!cancel_permission_mode_change(&store, &bob, &forged_intent)
        .await
        .unwrap());
    assert!(
        !discard_permission_mode_change(&store, &bob, &forged_intent)
            .await
            .unwrap()
    );
    let reason = FenceReason::ProbeAmbiguous {
        detail: "foreign owner must not fence this session".into(),
    };
    assert!(
        fence_permission_mode_change(&store, &bob, &forged_intent, &reason)
            .await
            .unwrap()
            .is_none()
    );

    let unchanged = get_session(&store, &alice, alice_session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.permission_mode, PermissionMode::Ask);
    assert_eq!(unchanged.lifecycle, CodeSessionLifecycle::Idle);
    assert_eq!(
        list_pending_permission_mode_changes(&store, &alice)
            .await
            .unwrap()
            .len(),
        1,
        "foreign writes must leave the owner's intent intact"
    );

    assert!(confirm_permission_mode_change(&store, &alice, &intent)
        .await
        .unwrap());
    assert_eq!(
        get_session(&store, &alice, alice_session_id)
            .await
            .unwrap()
            .unwrap()
            .permission_mode,
        PermissionMode::Auto
    );
}

/// A full session save may carry attention read before a trigger notification.
/// The targeted attention write must survive that later stale save.
#[tokio::test]
async fn stale_session_save_does_not_erase_targeted_attention() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let stale = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    let notification = Attention::needs_you("checks failed", AttentionSource::Structured);
    assert_eq!(
        replace_session_attention(&store, &owner, session_id, &notification, false)
            .await
            .unwrap(),
        Some(notification.clone())
    );

    assert!(save_session(&store, &stale).await.unwrap());
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .attention,
        notification
    );
}

/// Background workspace naming writes through this swap: it must replace
/// exactly the placeholder it was derived against and lose to any other
/// title, or a name the user typed could be silently overwritten.
#[tokio::test]
async fn workspace_title_swap_replaces_only_the_expected_title() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let workspace_id = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    assert!(set_workspace_title_if(
        &store,
        &OwnerId::local(),
        workspace_id,
        "first",
        "Derived name"
    )
    .await
    .unwrap());
    assert_eq!(
        get_workspace(&store, &OwnerId::local(), workspace_id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Derived name"
    );
    // A second writer holding the stale expectation no longer matches: the
    // title that landed first keeps the floor.
    assert!(!set_workspace_title_if(
        &store,
        &OwnerId::local(),
        workspace_id,
        "first",
        "Late derived name"
    )
    .await
    .unwrap());
    assert_eq!(
        get_workspace(&store, &OwnerId::local(), workspace_id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Derived name"
    );
}

/// A pull-request refresh holds its workspace snapshot across host I/O. Its
/// eventual PR-column write must not erase a title changed in the meantime.
#[tokio::test]
async fn pull_request_write_preserves_a_concurrent_workspace_rename() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    let refresh_snapshot = get_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refresh_snapshot.title, "first");

    assert!(set_workspace_title_if(
        &store,
        &owner,
        workspace_id,
        "first",
        "Renamed while refresh waited"
    )
    .await
    .unwrap());
    let digest = PullRequestDigest {
        number: 42,
        url: Some("https://github.com/acme/demo/pull/42".into()),
        state: "open".into(),
        title: Some("PR title".into()),
        checks_summary: None,
        checks: None,
        draft: Some(false),
        merged: Some(false),
        review_decision: None,
        mergeable: None,
        merge_state_status: None,
        head_branch: Some("feature".into()),
        base_branch: Some("main".into()),
        head_sha: Some("feedfeed".into()),
        auto_merge_enabled: Some(false),
        in_merge_queue: Some(false),
    };
    assert!(
        set_active_workspace_pull_request(&store, &owner, refresh_snapshot.id, &digest)
            .await
            .unwrap()
    );
    let stored = get_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.title, "Renamed while refresh waited");
    assert_eq!(stored.pr.as_ref(), Some(&digest));
}

#[tokio::test]
async fn entity_graph_round_trips() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let session = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.harness_kind, HarnessKind::ClaudeCode);
    assert_eq!(session.spawn_epoch, 0);
    let turn = get_turn(&store, &OwnerId::local(), turn_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(turn.user_input, "hello");
    let workspace = get_workspace(&store, &OwnerId::local(), session.workspace_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(workspace.status, CodeWorkspaceStatus::Active);
    let repo = get_repo(&store, &OwnerId::local(), workspace.repo_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(repo.branch_prefix, "tidebreak/");

    let approval_id = CodeApprovalId::new();
    insert_approval(
        &store,
        &OwnerId::local(),
        &CodeApproval {
            id: approval_id,
            session_id,
            turn_id,
            kind: CodeApprovalKind::FileWrite {
                paths: vec!["probe.txt".into()],
            },
            harness_raw: serde_json::json!({"tool":"Write"}),
            native_call_id: Some("toolu_local".into()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(0),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: now(),
            decided_at: None,
        },
    )
    .await
    .unwrap();
    let approval = get_approval(&store, &OwnerId::local(), approval_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(approval.state, CodeApprovalState::Pending);
}

#[tokio::test]
async fn approval_claim_and_abandonment_have_one_winner() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Running;
    assert!(save_session(&store, &session).await.unwrap());
    let approval_id = CodeApprovalId::new();
    insert_approval(
        &store,
        &owner,
        &CodeApproval {
            id: approval_id,
            session_id,
            turn_id,
            kind: CodeApprovalKind::Other {
                summary: "run command".into(),
            },
            harness_raw: serde_json::json!({"call_id":"toolu_claim"}),
            native_call_id: Some("toolu_claim".into()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(session.spawn_epoch),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: now(),
            decided_at: None,
        },
    )
    .await
    .unwrap();

    let claim = uuid::Uuid::new_v4();
    let (claimed, abandoned) = tokio::join!(
        claim_approval(
            &store,
            &owner,
            approval_id,
            session_id,
            session.spawn_epoch,
            claim,
            now(),
        ),
        abandon_pending_approval(
            &store,
            &owner,
            approval_id,
            session_id,
            session.spawn_epoch,
            now(),
        ),
    );
    let claimed = claimed.unwrap();
    let abandoned = abandoned.unwrap();
    assert_ne!(
        claimed.is_some(),
        abandoned.is_some(),
        "exactly one transition wins"
    );

    if claimed.is_some() {
        assert!(settle_approval_claim(
            &store,
            &owner,
            ClaimedApprovalSettlement {
                approval_id,
                session_id,
                worker_epoch: session.spawn_epoch,
                claim,
                decision: crate::code::ApprovalDecisionKind::Approve,
                decided_at: now(),
            },
        )
        .await
        .unwrap()
        .is_some());
        assert_eq!(
            get_approval(&store, &owner, approval_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            CodeApprovalState::Approved
        );
    } else {
        assert_eq!(
            get_approval(&store, &owner, approval_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            CodeApprovalState::Abandoned
        );
    }
}

#[tokio::test]
async fn approval_request_rolls_back_when_its_journal_event_fails() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Running;
    assert!(save_session(&store, &session).await.unwrap());
    let approval_id = CodeApprovalId::new();
    let approval = CodeApproval {
        id: approval_id,
        session_id,
        turn_id,
        kind: CodeApprovalKind::Other {
            summary: "run command".into(),
        },
        harness_raw: serde_json::json!({"call_id":"toolu_request_rollback"}),
        native_call_id: Some("toolu_request_rollback".into()),
        server_capability: Some("cap_request_rollback".into()),
        request_sha256: Some("sha_request_rollback".into()),
        worker_epoch: Some(session.spawn_epoch),
        decision_claim: None,
        claimed_at: None,
        state: CodeApprovalState::Pending,
        feedback: None,
        requested_at: now(),
        decided_at: None,
    };
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_approval_request_event BEFORE INSERT ON code_event \
             BEGIN SELECT RAISE(ABORT, 'forced approval request journal failure'); END",
        )
        .await
        .unwrap();

    assert!(insert_approval_for_worker(&store, &owner, &approval)
        .await
        .is_err());
    assert!(get_approval(&store, &owner, approval_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn approval_settlement_rolls_back_when_its_journal_event_fails() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Running;
    assert!(save_session(&store, &session).await.unwrap());
    let approval_id = CodeApprovalId::new();
    insert_approval(
        &store,
        &owner,
        &CodeApproval {
            id: approval_id,
            session_id,
            turn_id,
            kind: CodeApprovalKind::Other {
                summary: "run command".into(),
            },
            harness_raw: serde_json::json!({"call_id":"toolu_settle_rollback"}),
            native_call_id: Some("toolu_settle_rollback".into()),
            server_capability: Some("cap_settle_rollback".into()),
            request_sha256: Some("sha_settle_rollback".into()),
            worker_epoch: Some(session.spawn_epoch),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: now(),
            decided_at: None,
        },
    )
    .await
    .unwrap();
    let claim = uuid::Uuid::new_v4();
    assert!(claim_approval(
        &store,
        &owner,
        approval_id,
        session_id,
        session.spawn_epoch,
        claim,
        now(),
    )
    .await
    .unwrap()
    .is_some());
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_approval_resolution_event BEFORE INSERT ON code_event \
             BEGIN SELECT RAISE(ABORT, 'forced approval resolution journal failure'); END",
        )
        .await
        .unwrap();

    assert!(settle_approval_claim(
        &store,
        &owner,
        ClaimedApprovalSettlement {
            approval_id,
            session_id,
            worker_epoch: session.spawn_epoch,
            claim,
            decision: crate::code::ApprovalDecisionKind::Approve,
            decided_at: now(),
        },
    )
    .await
    .is_err());
    let approval = get_approval(&store, &owner, approval_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(approval.state, CodeApprovalState::Pending);
    assert_eq!(approval.decision_claim, Some(claim));
    assert!(
        list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events
            .is_empty()
    );
}

#[tokio::test]
async fn a_replaced_worker_cannot_insert_a_late_approval() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Running;
    assert!(save_session(&store, &session).await.unwrap());
    let stale_epoch = session.spawn_epoch;
    assert_eq!(bump_spawn_epoch(&store, session_id, None).await.unwrap(), 1);
    let approval_id = CodeApprovalId::new();
    let approval = CodeApproval {
        id: approval_id,
        session_id,
        turn_id,
        kind: CodeApprovalKind::Other {
            summary: "run command".into(),
        },
        harness_raw: serde_json::json!({"call_id":"toolu_stale"}),
        native_call_id: Some("toolu_stale".into()),
        server_capability: Some("cap_stale".into()),
        request_sha256: Some("sha_stale".into()),
        worker_epoch: Some(stale_epoch),
        decision_claim: None,
        claimed_at: None,
        state: CodeApprovalState::Pending,
        feedback: None,
        requested_at: now(),
        decided_at: None,
    };

    assert!(insert_approval_for_worker(&store, &owner, &approval)
        .await
        .unwrap()
        .is_none());
    assert!(get_approval(&store, &owner, approval_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn restart_abandons_claimed_and_unclaimed_approvals_for_the_stopped_worker() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Running;
    assert!(save_session(&store, &session).await.unwrap());
    let claimed_id = CodeApprovalId::new();
    insert_approval(
        &store,
        &owner,
        &CodeApproval {
            id: claimed_id,
            session_id,
            turn_id,
            kind: CodeApprovalKind::Other {
                summary: "run command".into(),
            },
            harness_raw: serde_json::json!({"call_id":"toolu_restart"}),
            native_call_id: Some("toolu_restart".into()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(session.spawn_epoch),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: now(),
            decided_at: None,
        },
    )
    .await
    .unwrap();
    let claim = uuid::Uuid::new_v4();
    assert!(claim_approval(
        &store,
        &owner,
        claimed_id,
        session_id,
        session.spawn_epoch,
        claim,
        now(),
    )
    .await
    .unwrap()
    .is_some());

    let unclaimed_id = CodeApprovalId::new();
    insert_approval(
        &store,
        &owner,
        &CodeApproval {
            id: unclaimed_id,
            session_id,
            turn_id,
            kind: CodeApprovalKind::Other {
                summary: "edit file".into(),
            },
            harness_raw: serde_json::json!({"call_id":"toolu_restart_unclaimed"}),
            native_call_id: Some("toolu_restart_unclaimed".into()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(session.spawn_epoch),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: now(),
            decided_at: None,
        },
    )
    .await
    .unwrap();

    let stale_id = CodeApprovalId::new();
    insert_approval(
        &store,
        &owner,
        &CodeApproval {
            id: stale_id,
            session_id,
            turn_id,
            kind: CodeApprovalKind::Other {
                summary: "stale worker command".into(),
            },
            harness_raw: serde_json::json!({"call_id":"toolu_restart_stale"}),
            native_call_id: Some("toolu_restart_stale".into()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(session.spawn_epoch - 1),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: now(),
            decided_at: None,
        },
    )
    .await
    .unwrap();

    assert!(abandon_pending_approvals_for_stopped_session(
        &store,
        &owner,
        session_id,
        session.spawn_epoch,
        now(),
    )
    .await
    .unwrap()
    .is_empty());
    let still_claimed = get_approval(&store, &owner, claimed_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still_claimed.state, CodeApprovalState::Pending);
    assert_eq!(still_claimed.decision_claim, Some(claim));
    assert_eq!(
        get_approval(&store, &owner, unclaimed_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        CodeApprovalState::Pending
    );

    session.lifecycle = CodeSessionLifecycle::Idle;
    assert!(save_session(&store, &session).await.unwrap());

    let abandoned = abandon_pending_approvals_for_stopped_session(
        &store,
        &owner,
        session_id,
        session.spawn_epoch,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(abandoned.len(), 2);
    assert!(abandoned.iter().any(|row| row.approval.id == claimed_id));
    assert!(abandoned.iter().any(|row| row.approval.id == unclaimed_id));
    let approval = get_approval(&store, &owner, claimed_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(approval.state, CodeApprovalState::Abandoned);
    assert!(approval.decision_claim.is_none());
    assert!(approval.decided_at.is_some());
    assert_eq!(
        get_approval(&store, &owner, unclaimed_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        CodeApprovalState::Abandoned
    );
    assert_eq!(
        get_approval(&store, &owner, stale_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        CodeApprovalState::Pending,
        "restart cleanup must not cross worker epochs"
    );
}

#[tokio::test]
async fn interrupted_recovery_rolls_back_every_row_when_the_journal_fails() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let mut session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Running;
    assert!(save_session(&store, &session).await.unwrap());
    set_session_subagents(
        &store,
        &owner,
        session_id,
        &[CodeSubagentSummary {
            call_id: "task-recovery".into(),
            name: "Still running".into(),
            status: CodeSubagentStatus::Running,
        }],
    )
    .await
    .unwrap();
    let approval_id = CodeApprovalId::new();
    insert_approval(
        &store,
        &owner,
        &CodeApproval {
            id: approval_id,
            session_id,
            turn_id,
            kind: CodeApprovalKind::Other {
                summary: "run command".into(),
            },
            harness_raw: serde_json::json!({"call_id":"toolu_rollback"}),
            native_call_id: Some("toolu_rollback".into()),
            server_capability: Some("cap_rollback".into()),
            request_sha256: Some("sha_rollback".into()),
            worker_epoch: Some(session.spawn_epoch),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: now(),
            decided_at: None,
        },
    )
    .await
    .unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_code_recovery_event BEFORE INSERT ON code_event \
             BEGIN SELECT RAISE(ABORT, 'forced recovery journal failure'); END",
        )
        .await
        .unwrap();

    assert!(
        recover_interrupted_session(&store, &owner, session_id, session.spawn_epoch,)
            .await
            .is_err()
    );
    let session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.lifecycle, CodeSessionLifecycle::Running);
    assert_eq!(session.subagents[0].status, CodeSubagentStatus::Running);
    assert_eq!(
        get_turn(&store, &owner, turn_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        CodeTurnStatus::Running
    );
    assert_eq!(
        get_approval(&store, &owner, approval_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        CodeApprovalState::Pending
    );
    assert!(
        list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events
            .is_empty()
    );
}

/// A worker that has been superseded still holds a snapshot of the row and
/// writes it on the way out. If that write lands, it puts the old epoch back:
/// the outgoing worker un-fences itself, the live worker becomes the stale one,
/// and everything the live worker appends is dropped while the session still
/// reads as healthy.
#[tokio::test]
async fn a_superseded_worker_cannot_regress_the_session_row() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let outgoing = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        bump_spawn_epoch(&store, session_id, Some(99))
            .await
            .unwrap(),
        1
    );
    let mut live = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    live.lifecycle = CodeSessionLifecycle::Running;
    assert!(save_session(&store, &live).await.unwrap());

    let mut unwinding = outgoing;
    unwinding.lifecycle = CodeSessionLifecycle::Ended;
    unwinding.child_pid = None;
    assert!(!save_session(&store, &unwinding).await.unwrap());

    let row = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.spawn_epoch, 1);
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Running);
    assert_eq!(row.child_pid, Some(99));
    // The live worker is still the one that owns the journal.
    append_event(
        &store,
        &OwnerId::local(),
        session_id,
        1,
        &CodeEvent::TurnInterrupted,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_terminal_code_event_mints_one_notification_in_its_transaction() {
    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let event = CodeEvent::TurnCompleted {
        usage: Default::default(),
        checkpoint: None,
    };

    append_event_with_notification(&store, &owner, session_id, 0, turn_id, &event)
        .await
        .unwrap();

    let notifications = store
        .list_notifications_scoped(&owner, None, 50)
        .await
        .unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].title, "first finished");
    assert_eq!(
        notifications[0].context,
        crate::NotificationContext::Code {
            session_id,
            workspace_id: get_session(&store, &owner, session_id)
                .await
                .unwrap()
                .unwrap()
                .workspace_id,
        }
    );
}

/// Archive marks the row Ended without bumping spawn_epoch. A same-epoch
/// worker persist of Running or Idle must not revive it, or a late write
/// leaves an archived workspace with a live-looking session (and pid).
#[tokio::test]
async fn an_ended_session_cannot_be_revived_by_a_same_epoch_persist() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let mut ended = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    ended.lifecycle = CodeSessionLifecycle::Ended;
    ended.child_pid = None;
    assert!(save_session(&store, &ended).await.unwrap());

    let mut running = ended.clone();
    running.lifecycle = CodeSessionLifecycle::Running;
    running.child_pid = Some(4242);
    assert!(!save_session(&store, &running).await.unwrap());

    let mut idle = ended.clone();
    idle.lifecycle = CodeSessionLifecycle::Idle;
    idle.child_pid = Some(7);
    assert!(!save_session(&store, &idle).await.unwrap());

    let row = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Ended);
    assert_eq!(row.child_pid, None);
    assert_eq!(row.spawn_epoch, 0);

    // Re-asserting Ended at the same epoch is how archive writes after the wait.
    assert!(save_session(&store, &ended).await.unwrap());
}

fn published_image() -> ImageRef {
    ImageRef {
        blob_id: uuid::Uuid::new_v4(),
        media_type: ImageMediaType::Png,
        width: 16,
        height: 12,
        byte_len: 128,
    }
}

#[tokio::test]
async fn code_session_publication_cancels_retirement_and_keeps_a_draft_live() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let image = published_image();
    assert!(store
        .ensure_orphan_blob_retirement(image.blob_id)
        .await
        .unwrap());

    assert!(store
        .publish_code_session_image(&OwnerId::local(), session_id, &image, now())
        .await
        .unwrap());

    assert_eq!(
        store
            .get_blob_retirement(image.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled
    );
    assert!(!store
        .ensure_orphan_blob_retirement(image.blob_id)
        .await
        .unwrap());
}

#[tokio::test]
async fn an_exact_publication_retry_cancels_a_new_retirement_episode() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let image = published_image();
    assert!(store
        .publish_code_session_image(&OwnerId::local(), session_id, &image, now())
        .await
        .unwrap());
    crate::db::ops::blob::enqueue_on(&store.conn, image.blob_id)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_blob_retirement(image.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );

    assert!(store
        .publish_code_session_image(&OwnerId::local(), session_id, &image, now())
        .await
        .unwrap());

    assert_eq!(
        store
            .get_blob_retirement(image.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled
    );
}

#[tokio::test]
async fn another_owner_cannot_publish_or_pin_a_session_image() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let image = published_image();
    assert!(store
        .ensure_orphan_blob_retirement(image.blob_id)
        .await
        .unwrap());

    assert!(!store
        .publish_code_session_image(
            &OwnerId::new("another-owner").unwrap(),
            session_id,
            &image,
            now(),
        )
        .await
        .unwrap());

    assert_eq!(
        store
            .get_blob_retirement(image.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );
    assert!(store
        .get_published_code_session_image(&OwnerId::local(), session_id, image.blob_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn an_ended_session_cannot_publish_or_pin_a_new_image() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let mut session = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Ended;
    assert!(save_session(&store, &session).await.unwrap());
    let image = published_image();
    assert!(store
        .ensure_orphan_blob_retirement(image.blob_id)
        .await
        .unwrap());

    assert!(!store
        .publish_code_session_image(&OwnerId::local(), session_id, &image, now())
        .await
        .unwrap());

    assert_eq!(
        store
            .get_blob_retirement(image.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );
    assert!(store
        .get_published_code_session_image(&OwnerId::local(), session_id, image.blob_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn an_ended_session_does_not_pin_an_unsent_publication() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let image = published_image();
    store
        .publish_code_session_image(&OwnerId::local(), session_id, &image, now())
        .await
        .unwrap();
    let mut session = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Ended;
    assert!(save_session(&store, &session).await.unwrap());

    assert!(store
        .ensure_orphan_blob_retirement(image.blob_id)
        .await
        .unwrap());
}

#[tokio::test]
async fn a_turn_attachment_stays_live_after_its_session_ends() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let image = published_image();
    store
        .publish_code_session_image(&OwnerId::local(), session_id, &image, now())
        .await
        .unwrap();
    insert_turn(
        &store,
        &OwnerId::local(),
        &CodeTurn {
            id: CodeTurnId::new(),
            session_id,
            ordinal: 2,
            status: CodeTurnStatus::Completed,
            model: None,
            fast_mode: false,
            user_input: "look at this".into(),
            user_input_blob_id: None,
            attachments: vec![image],
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            started_at: now(),
            ended_at: Some(now()),
        },
    )
    .await
    .unwrap();
    let mut session = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    session.lifecycle = CodeSessionLifecycle::Ended;
    assert!(save_session(&store, &session).await.unwrap());

    assert!(!store
        .ensure_orphan_blob_retirement(image.blob_id)
        .await
        .unwrap());
}

#[tokio::test]
async fn journal_rejects_stale_spawn_epoch() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let event = CodeEvent::TurnInterrupted;
    append_event(&store, &OwnerId::local(), session_id, 0, &event)
        .await
        .unwrap();
    let new_epoch = bump_spawn_epoch(&store, session_id, Some(42))
        .await
        .unwrap();
    assert_eq!(new_epoch, 1);
    let err = append_event(&store, &OwnerId::local(), session_id, 0, &event)
        .await
        .unwrap_err();
    match err {
        CodeJournalError::StaleSpawnEpoch {
            attempted, current, ..
        } => {
            assert_eq!(attempted, 0);
            assert_eq!(current, 1);
        }
        other => panic!("expected stale epoch, got {other:?}"),
    }
    append_event(&store, &OwnerId::local(), session_id, 1, &event)
        .await
        .unwrap();
    let events = list_events(&store, &OwnerId::local(), session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap();
    assert_eq!(events.events.len(), 2);
    assert_eq!(events.events[0].seq, 1);
    assert_eq!(events.events[1].seq, 2);
}

#[tokio::test]
async fn spawn_epoch_bumps_are_serialized() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let n = 16_i64;
    let mut joins = Vec::new();
    for _ in 0..n {
        let store = store.clone();
        joins.push(tokio::spawn(async move {
            bump_spawn_epoch(&store, session_id, None).await.unwrap()
        }));
    }
    let mut epochs = Vec::new();
    for join in joins {
        epochs.push(join.await.unwrap());
    }
    epochs.sort_unstable();
    assert_eq!(epochs, (1..=n).collect::<Vec<_>>());
    let session = get_session(&store, &OwnerId::local(), session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.spawn_epoch, n);
}

#[tokio::test]
async fn journal_seq_is_monotonic_under_concurrent_appends() {
    let (_dir, store, session_id, _) = seeded_session().await;
    let n = 24;
    let mut joins = Vec::new();
    for i in 0..n {
        let store = store.clone();
        joins.push(tokio::spawn(async move {
            append_event(
                &store,
                &OwnerId::local(),
                session_id,
                0,
                &CodeEvent::AssistantDelta {
                    text: format!("chunk-{i}"),
                },
            )
            .await
            .unwrap()
        }));
    }
    let mut seqs = Vec::new();
    for join in joins {
        seqs.push(join.await.unwrap());
    }
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=n).collect::<Vec<_>>());
    let events = list_events(&store, &OwnerId::local(), session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap();
    assert_eq!(events.events.len(), n as usize);
    assert!(events
        .events
        .windows(2)
        .all(|pair| pair[0].seq + 1 == pair[1].seq));
}

/// A capped replay keeps the newest events and says the head is missing.
///
/// Dropping the tail instead would be worse than useless — a reader would
/// resume before the live stream and never catch up — and dropping the head
/// silently would let a session open on its middle and read as if that were
/// the beginning.
#[tokio::test]
async fn a_capped_replay_keeps_the_newest_events_and_admits_the_head_is_gone() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    for index in 0..6 {
        append_event(
            &store,
            &owner,
            session_id,
            0,
            &CodeEvent::HarnessNotice {
                level: crate::code::HarnessNoticeLevel::Info,
                message: format!("notice {index}"),
            },
        )
        .await
        .unwrap();
    }

    let capped = list_events(&store, &owner, session_id, 0, 4).await.unwrap();
    assert!(capped.truncated);
    assert_eq!(
        capped
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![3, 4, 5, 6]
    );

    // A window that fits says nothing was dropped, including the exact fit.
    let exact = list_events(&store, &owner, session_id, 0, 6).await.unwrap();
    assert!(!exact.truncated);
    assert_eq!(exact.events.len(), 6);

    // The cursor still applies: the cap windows what is above it.
    let tail = list_events(&store, &owner, session_id, 4, 4).await.unwrap();
    assert!(!tail.truncated);
    assert_eq!(
        tail.events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![5, 6]
    );
}

/// ADR 0030: no chat entity references a code-mode table or id type, and no
/// code-mode entity references a chat table or id type.
#[test]
fn chat_and_code_entities_do_not_cross_reference() {
    fn without_comments(source: &str) -> String {
        let mut stripped = String::with_capacity(source.len());
        let mut chars = source.chars().peekable();
        let mut block_depth = 0;
        while let Some(character) = chars.next() {
            if block_depth > 0 {
                match (character, chars.peek().copied()) {
                    ('/', Some('*')) => {
                        chars.next();
                        stripped.push_str("  ");
                        block_depth += 1;
                    }
                    ('*', Some('/')) => {
                        chars.next();
                        stripped.push_str("  ");
                        block_depth -= 1;
                    }
                    ('\n', _) => stripped.push('\n'),
                    _ => stripped.push(' '),
                }
                continue;
            }
            match (character, chars.peek().copied()) {
                ('/', Some('/')) => {
                    chars.next();
                    stripped.push_str("  ");
                    for comment in chars.by_ref() {
                        if comment == '\n' {
                            stripped.push('\n');
                            break;
                        }
                        stripped.push(' ');
                    }
                }
                ('/', Some('*')) => {
                    chars.next();
                    stripped.push_str("  ");
                    block_depth = 1;
                }
                _ => stripped.push(character),
            }
        }
        stripped
    }

    let sample = "/// ChatId in prose is allowed.\n\
                  /* AgentEvent in a nested /* TurnId */ comment is allowed. */\n\
                  pub use crate::id::ChatId as ConversationId;";
    let stripped_sample = without_comments(sample);
    assert!(!stripped_sample.contains("ChatId in prose"));
    assert!(!stripped_sample.contains("AgentEvent in a nested"));
    assert!(stripped_sample.contains("crate::id::ChatId as ConversationId"));

    let entities = without_comments(include_str!("../entities.rs"));
    let mut current_mod = "";
    for line in entities.lines() {
        if let Some(name) = line.strip_prefix("pub mod ") {
            current_mod = name.trim_end_matches(" {").trim();
            continue;
        }
        let is_code = current_mod.starts_with("code_");
        if is_code {
            assert!(
                !line.contains("chat_id") && !line.contains("ChatId"),
                "{current_mod} references a chat id: {line}"
            );
        } else if current_mod != "code_event" {
            assert!(
                !line.contains("code_session")
                    && !line.contains("code_workspace")
                    && !line.contains("code_repo")
                    && !line.contains("CodeSessionId")
                    && !line.contains("RepoId")
                    && !line.contains("WorkspaceId"),
                "{current_mod} references a code-mode type: {line}"
            );
        }
    }

    fn walk_rs(dir: &std::path::Path, visit: &mut dyn FnMut(&std::path::Path, &str)) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk_rs(&path, visit);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                visit(&path, &without_comments(&text));
            }
        }
    }

    let crate_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    walk_rs(&crate_src.join("db/ops/code"), &mut |path, text| {
        for needle in ["ChatId", "MessageId", "AgentEvent"] {
            assert!(
                !text.contains(needle),
                "{} references chat type {needle}",
                path.display()
            );
        }
        // `TurnId` is a suffix of `CodeTurnId`; require a word-ish boundary.
        // `code_turn_attachment.turn_id` is a code-mode FK, not chat `TurnId`.
        for line in text.lines() {
            if line.contains("TurnId")
                && !line.contains("CodeTurnId")
                && !line.contains("code_turn_attachment")
            {
                panic!("{} references chat TurnId: {line}", path.display());
            }
        }
    });
    walk_rs(&crate_src.join("code"), &mut |path, text| {
        if path.file_name().and_then(|name| name.to_str()) == Some("permission.rs") {
            return;
        }
        for needle in ["ChatId", "MessageId", "AgentEvent"] {
            assert!(
                !text.contains(needle),
                "{} references chat type {needle}",
                path.display()
            );
        }
    });
}

/// Decision 47's validation for code mode: on a shared store, a second user
/// sees none of another owner's `code_*` rows. Every table is exercised, not
/// a sample, because the wrong implementation scopes the surfaces someone
/// remembered and leaves the rest reachable.
#[tokio::test]
async fn owner_scoped_code_queries_partition_every_table() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("alice").unwrap();
    let bob = OwnerId::new("bob").unwrap();
    let (alice_session, alice_turn) = seed_owner(&store, &alice, "alice").await;
    let (bob_session, _bob_turn) = seed_owner(&store, &bob, "bob").await;

    // Repositories and workspaces: each owner sees exactly one of each.
    let alice_repos = list_repos(&store, &alice).await.unwrap();
    assert_eq!(alice_repos.len(), 1);
    assert_eq!(alice_repos[0].display_name, "alice");
    let bob_repos = list_repos(&store, &bob).await.unwrap();
    assert_eq!(bob_repos.len(), 1);
    assert_eq!(bob_repos[0].display_name, "bob");
    assert!(get_repo(&store, &bob, alice_repos[0].id)
        .await
        .unwrap()
        .is_none());

    let alice_workspace = get_session(&store, &alice, alice_session)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    assert!(get_workspace(&store, &bob, alice_workspace)
        .await
        .unwrap()
        .is_none());

    // Sessions: listed per owner, and another owner's id resolves to nothing.
    assert_eq!(list_sessions(&store, &alice).await.unwrap().len(), 1);
    assert_eq!(list_sessions(&store, &bob).await.unwrap().len(), 1);
    assert!(get_session(&store, &bob, alice_session)
        .await
        .unwrap()
        .is_none());
    assert!(get_session(&store, &alice, bob_session)
        .await
        .unwrap()
        .is_none());

    // Turns.
    assert!(get_turn(&store, &bob, alice_turn).await.unwrap().is_none());
    assert!(list_turns(&store, &bob, alice_session)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(list_turn_metrics(&store, &alice).await.unwrap().len(), 1);
    assert_eq!(list_turn_metrics(&store, &bob).await.unwrap().len(), 1);

    // Journal events: the second user replays nothing from the first user's
    // session, whatever cursor they ask from.
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
            native_call_id: Some("toolu_alice".into()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(0),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: now(),
            decided_at: None,
        },
    )
    .await
    .unwrap();
    assert!(get_approval(&store, &bob, approval_id)
        .await
        .unwrap()
        .is_none());
    assert!(list_approvals(&store, &bob, None, None)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        list_approvals(&store, &alice, None, None)
            .await
            .unwrap()
            .len(),
        1
    );

    // Writes do not cross either: a save carrying another owner's key changes
    // no row, so a forged record cannot overwrite one it does not own.
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

/// Two owners may register the same repository path. The uniqueness that
/// keeps one owner from registering a path twice is per owner, not per
/// install, or the second user on a shared machine could not use code mode
/// on a repository the first user had already registered.
#[tokio::test]
async fn the_same_repository_path_can_belong_to_two_owners() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("alice").unwrap();
    let bob = OwnerId::new("bob").unwrap();
    for owner in [&alice, &bob] {
        insert_repo(
            &store,
            &CodeRepo {
                id: RepoId::new(),
                owner: owner.clone(),
                root_path: "/srv/shared-checkout".into(),
                display_name: "shared".into(),
                default_base_ref: "main".into(),
                branch_prefix: "tidebreak/".into(),
                setup_script: None,
                archive_script: None,
                quick_actions: Vec::new(),
                created_at: now(),
                removed_at: None,
                cloned_from: None,
                origin_host: None,
                origin_owner: None,
                origin_name: None,
            },
        )
        .await
        .unwrap();
    }
    let found = crate::db::code::get_repo_by_root_path(&store, &bob, "/srv/shared-checkout")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.owner, bob);
}

/// Decision 48 step 1 is structural: the owner column is on *every* code
/// table, so an owner-scoped query exists for each one. A new `code_*` table
/// added without an owner fails here rather than in review.
#[test]
fn every_code_table_carries_an_owner_column() {
    let mut checked = Vec::new();
    for entry in crate::db::migration::tables_for_test() {
        let Some(name) = entry.table.get_table_name().map(|name| format!("{name:?}")) else {
            continue;
        };
        // `TableRef` has no Display; its debug form names the table.
        let name = name
            .rsplit(['(', ')', '"', ' '])
            .find(|part| part.starts_with("code_") || part.contains('_'))
            .unwrap_or(&name)
            .to_owned();
        if !name.starts_with("code_") {
            continue;
        }
        let has_owner = entry
            .table
            .get_columns()
            .iter()
            .any(|column| column.get_column_name() == "owner");
        assert!(
            has_owner,
            "{name} has no owner column: every code_* table is owner-scoped \
             (decision 48 step 1), so add one rather than reaching this row \
             through a join"
        );
        checked.push(name);
    }
    checked.sort();
    assert_eq!(
        checked,
        [
            "code_approval",
            "code_event",
            "code_repo",
            "code_session",
            "code_session_image",
            "code_trigger",
            "code_trigger_fire",
            "code_turn",
            "code_turn_attachment",
            "code_watch",
            "code_workspace",
        ],
        "the set of code tables changed; confirm the new one is owner-scoped"
    );
}

/// Every code-mode store function takes an owner, so there is no unscoped
/// query for a caller to reach for by accident. The exceptions are named
/// `_all_owners` and are documented as system paths — boot recovery, the
/// stall sweep, and a worker re-reading the session it was spawned against.
#[test]
fn code_store_queries_are_owner_scoped_or_say_they_are_not() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/db/ops/code");
    let mut unscoped = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("code ops directory") {
        let path = entry.expect("code ops entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("code ops file");
        let file = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            let Some(rest) = line.trim_start().strip_prefix("pub async fn ") else {
                continue;
            };
            let name = rest.split('(').next().unwrap_or_default().to_owned();
            // `insert_*` and `save_*` carry the owner on the record itself;
            // `bump_spawn_epoch` and the lock helpers act on an id a caller
            // already authorized.
            if name.starts_with("insert_")
                || name.starts_with("save_")
                || name == "bump_spawn_epoch"
            {
                continue;
            }
            if name.ends_with("_all_owners") {
                continue;
            }
            // The owner parameter is on the signature, which may wrap.
            let mut signature = line.to_owned();
            for _ in 0..12 {
                if signature.contains(") ->") {
                    break;
                }
                match lines.peek() {
                    Some(next) => {
                        signature.push_str(next);
                        lines.next();
                    }
                    None => break,
                }
            }
            if !signature.contains("owner: &OwnerId") {
                unscoped.push(format!("{file}::{name}"));
            }
        }
    }
    assert!(
        unscoped.is_empty(),
        "these code store queries take no owner: {unscoped:?}. Add an \
         `owner: &OwnerId` parameter and filter on it, or, if this really is a \
         system path rather than a request path, name it `*_all_owners` and \
         say why in its documentation."
    );
}

/// The digest builder labels a watch child row from the watch found by its
/// session id. A workspace-based match would hand one session the other
/// watch's state the moment a workspace has watched twice.
#[tokio::test]
async fn latest_watch_for_session_matches_on_the_session_not_the_workspace() {
    use crate::code::{CodeWatch, CodeWatchId, CodeWatchState};
    use crate::db::code::{insert_watch, latest_watch_for_session};

    let (_dir, store, session_id, _turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    let watch_session_id = CodeSessionId::new();
    insert_session(
        &store,
        &CodeSession {
            id: watch_session_id,
            owner: owner.clone(),
            workspace_id,
            kind: CodeSessionKind::Watch,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: PermissionMode::Auto,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: CodeSessionLifecycle::Running,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: now(),
        },
    )
    .await
    .unwrap();

    let older = CodeWatch {
        id: CodeWatchId::new(),
        owner: owner.clone(),
        workspace_id,
        session_id,
        pr_number: 7,
        state: CodeWatchState::Stopped,
        detail: None,
        last_fix_head: None,
        cycles: 1,
        created_at: now() - chrono::Duration::minutes(5),
        updated_at: now() - chrono::Duration::minutes(5),
    };
    let newer = CodeWatch {
        id: CodeWatchId::new(),
        owner: owner.clone(),
        workspace_id,
        session_id: watch_session_id,
        pr_number: 9,
        state: CodeWatchState::Fixing,
        detail: Some("fixing failing checks".to_owned()),
        last_fix_head: None,
        cycles: 3,
        created_at: now(),
        updated_at: now(),
    };
    insert_watch(&store, &older).await.unwrap();
    insert_watch(&store, &newer).await.unwrap();

    let found = latest_watch_for_session(&store, &owner, session_id)
        .await
        .unwrap()
        .expect("older watch");
    assert_eq!(found.id, older.id);
    assert_eq!(found.state, CodeWatchState::Stopped);

    let found = latest_watch_for_session(&store, &owner, watch_session_id)
        .await
        .unwrap()
        .expect("newer watch");
    assert_eq!(found.cycles, 3);
    assert_eq!(found.detail.as_deref(), Some("fixing failing checks"));

    assert!(
        latest_watch_for_session(&store, &owner, CodeSessionId::new())
            .await
            .unwrap()
            .is_none()
    );
}

/// A failed detached submit releases only its own reservation. The retry can
/// reserve the same head, while a duplicate sweep cannot reserve it twice.
#[tokio::test]
async fn watch_submission_failure_retries_without_consuming_the_head() {
    use crate::code::{CodeWatch, CodeWatchId, CodeWatchState};
    use crate::db::code::{
        accept_watch_submission, insert_watch, latest_watch_for_workspace,
        release_watch_submission, reserve_watch_submission,
    };

    let (_dir, store, session_id, _turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    let created_at = now() - chrono::Duration::minutes(1);
    let watch = CodeWatch {
        id: CodeWatchId::new(),
        owner: owner.clone(),
        workspace_id,
        session_id,
        pr_number: 42,
        state: CodeWatchState::Watching,
        detail: None,
        last_fix_head: None,
        cycles: 0,
        created_at,
        updated_at: created_at,
    };
    insert_watch(&store, &watch).await.unwrap();

    let first_detail = "submitting failing checks for head abc123";
    let first_reserved_at = created_at + chrono::Duration::seconds(1);
    let first_claim =
        reserve_watch_submission(&store, &owner, &watch, first_detail, first_reserved_at)
            .await
            .unwrap()
            .expect("first reservation");
    assert!(
        reserve_watch_submission(&store, &owner, &watch, first_detail, first_reserved_at)
            .await
            .unwrap()
            .is_none(),
        "a duplicate sweep must not reserve the same transition"
    );
    let reserved = latest_watch_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reserved.state, CodeWatchState::Fixing);
    assert_eq!(reserved.detail.as_deref(), Some(first_detail));
    assert_eq!(reserved.last_fix_head, None);
    assert_eq!(reserved.cycles, 0);

    let released_at = first_reserved_at + chrono::Duration::seconds(1);
    assert!(
        release_watch_submission(&store, &owner, &first_claim, released_at)
            .await
            .unwrap()
    );
    let retry = latest_watch_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.state, CodeWatchState::Watching);
    assert_eq!(retry.detail, None);
    assert_eq!(retry.last_fix_head, None);
    assert_eq!(retry.cycles, 0);

    let second_detail = "submitting failing checks for head abc123";
    let second_reserved_at = released_at + chrono::Duration::seconds(1);
    let second_claim =
        reserve_watch_submission(&store, &owner, &retry, second_detail, second_reserved_at)
            .await
            .unwrap()
            .expect("retry reservation");
    let accepted_at = second_reserved_at + chrono::Duration::seconds(1);
    assert!(accept_watch_submission(
        &store,
        &owner,
        &second_claim,
        Some("abc123"),
        "fixing failing checks",
        accepted_at,
    )
    .await
    .unwrap());
    assert!(
        !release_watch_submission(
            &store,
            &owner,
            &second_claim,
            accepted_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap(),
        "a late failure must not release an accepted retry"
    );
    let accepted = latest_watch_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.state, CodeWatchState::Fixing);
    assert_eq!(accepted.detail.as_deref(), Some("fixing failing checks"));
    assert_eq!(accepted.last_fix_head.as_deref(), Some("abc123"));
    assert_eq!(accepted.cycles, 1);
}

/// Removing a repository must not take its history with it.
///
/// A hard delete cannot do this. SQLite does not enforce the workspace
/// foreign key, so it would leave the workspace, session, turn, and event
/// rows behind with no reachable repository to find them through; PostgreSQL
/// does enforce it, so the same delete fails against exactly the archived
/// workspaces this path requires. Soft removal is what makes the two backends
/// agree and keeps the transcript searchable afterwards.
#[tokio::test]
async fn a_removed_repo_keeps_its_workspaces_and_transcript() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let (session_id, turn_id) = seed_owner(&store, &owner, "example").await;

    let repo = list_repos(&store, &owner).await.unwrap().remove(0);
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;

    assert!(mark_repo_removed(&store, &owner, repo.id, now())
        .await
        .unwrap());

    // Gone from the list the user picks a repository from.
    assert!(list_repos(&store, &owner).await.unwrap().is_empty());

    // Still resolvable, because the history hangs off it.
    let stored = get_repo(&store, &owner, repo.id).await.unwrap().unwrap();
    assert!(stored.removed_at.is_some());
    assert!(get_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .is_some());
    assert!(get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .is_some());
    assert!(get_turn(&store, &owner, turn_id).await.unwrap().is_some());
}

/// Removing a registration releases its path for a fresh registration while
/// the old row remains addressable for archived history.
#[tokio::test]
async fn a_removed_repo_path_can_be_registered_again() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    seed_owner(&store, &owner, "example").await;
    let removed = list_repos(&store, &owner).await.unwrap().remove(0);

    assert!(mark_repo_removed(&store, &owner, removed.id, now())
        .await
        .unwrap());

    let replacement = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: removed.root_path.clone(),
        display_name: "example again".into(),
        default_base_ref: "main".into(),
        branch_prefix: "tidebreak/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: now(),
        removed_at: None,
        cloned_from: None,
        origin_host: None,
        origin_owner: None,
        origin_name: None,
    };
    insert_repo(&store, &replacement).await.unwrap();

    let live = get_repo_by_root_path(&store, &owner, &removed.root_path)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.id, replacement.id);
    assert_eq!(list_repos(&store, &owner).await.unwrap(), vec![replacement]);
    assert!(get_repo(&store, &owner, removed.id)
        .await
        .unwrap()
        .unwrap()
        .removed_at
        .is_some());
}

/// Removal is once. A second call reports that nothing changed rather than
/// silently restamping the timestamp a reader may be showing.
#[tokio::test]
async fn removing_an_already_removed_repo_reports_no_change() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    seed_owner(&store, &owner, "example").await;
    let repo = list_repos(&store, &owner).await.unwrap().remove(0);

    assert!(mark_repo_removed(&store, &owner, repo.id, now())
        .await
        .unwrap());
    assert!(!mark_repo_removed(&store, &owner, repo.id, now())
        .await
        .unwrap());
}

/// Another owner's repository is not removable, the way it is not readable.
#[tokio::test]
async fn a_repo_is_not_removable_by_another_owner() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::local();
    let bob = OwnerId::new("bob").unwrap();
    seed_owner(&store, &alice, "alice").await;
    let repo = list_repos(&store, &alice).await.unwrap().remove(0);

    assert!(!mark_repo_removed(&store, &bob, repo.id, now())
        .await
        .unwrap());
    assert_eq!(list_repos(&store, &alice).await.unwrap().len(), 1);
}

/// Provenance is what makes a checkout reclaimable, so it must survive the
/// round trip rather than being inferred later from a path.
#[tokio::test]
async fn a_cloned_repo_records_where_it_came_from() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let cloned = RepoId::new();
    insert_repo(
        &store,
        &CodeRepo {
            id: cloned,
            owner: owner.clone(),
            root_path: "/tmp/cloned-repo".into(),
            display_name: "cloned".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: now(),
            removed_at: None,
            cloned_from: Some("https://example.invalid/team/repo.git".into()),
            origin_host: None,
            origin_owner: None,
            origin_name: None,
        },
    )
    .await
    .unwrap();

    let stored = get_repo(&store, &owner, cloned).await.unwrap().unwrap();
    assert_eq!(
        stored.cloned_from.as_deref(),
        Some("https://example.invalid/team/repo.git")
    );

    // A registered repository records nothing, which is what keeps reclaim
    // from ever pointing at a directory the user brought.
    let (session_id, _) = seed_owner(&store, &owner, "registered").await;
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    let registered = get_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .unwrap()
        .repo_id;
    assert!(get_repo(&store, &owner, registered)
        .await
        .unwrap()
        .unwrap()
        .cloned_from
        .is_none());
}

/// Arming the same condition twice is an edit, not a second rule. Without the
/// upsert the unique index would turn a re-arm into a store error the user
/// would see as "already exists" on a switch they just flipped.
#[tokio::test]
async fn arming_a_condition_twice_edits_the_rule() {
    use crate::code::{CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerId};
    use crate::db::code::{arm_trigger, list_repos, list_triggers_for_repo};

    let (_dir, store, _session_id, _turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let repo_id = list_repos(&store, &owner).await.unwrap()[0].id;

    let armed = CodeTrigger {
        id: CodeTriggerId::new(),
        owner: owner.clone(),
        repo_id,
        condition: CodeTriggerCondition::ChecksFailed,
        action: CodeTriggerAction::Notify,
        enabled: true,
        created_at: now(),
        updated_at: now(),
    };
    arm_trigger(&store, &owner, &armed).await.unwrap();

    let rearmed = CodeTrigger {
        id: CodeTriggerId::new(),
        action: CodeTriggerAction::Deliver,
        updated_at: now() + chrono::Duration::minutes(1),
        ..armed.clone()
    };
    arm_trigger(&store, &owner, &rearmed).await.unwrap();

    let stored = list_triggers_for_repo(&store, &owner, repo_id)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "re-arming created a second rule");
    assert_eq!(stored[0].action, CodeTriggerAction::Deliver);
    assert_eq!(stored[0].id, armed.id, "the identity moved on an edit");
}

/// Trigger writes must carry the owner through every persistence boundary.
/// A system sweep may discover another owner's delivery id, but that id alone
/// must not authorize a lease, retry, acknowledgment, or repository binding.
#[tokio::test]
async fn trigger_persistence_rejects_another_owner() {
    use crate::code::{
        CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFireIdentity,
        CodeTriggerId,
    };
    use crate::db::code::{
        acknowledge_trigger_fire_delivery, arm_trigger, insert_or_load_trigger_fire,
        lease_trigger_fire_delivery, list_fires_for_workspace, list_repos, list_triggers_for_repo,
        reschedule_trigger_fire_delivery_failure,
    };

    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("alice").unwrap();
    let bob = OwnerId::new("bob").unwrap();
    let (alice_session_id, _) = seed_owner(&store, &alice, "trigger-alice").await;
    seed_owner(&store, &bob, "trigger-bob").await;
    let alice_repo_id = list_repos(&store, &alice).await.unwrap()[0].id;
    let bob_repo_id = list_repos(&store, &bob).await.unwrap()[0].id;
    let alice_workspace_id = get_session(&store, &alice, alice_session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    let at = now();

    let foreign_repo_trigger = CodeTrigger {
        id: CodeTriggerId::new(),
        owner: alice.clone(),
        repo_id: bob_repo_id,
        condition: CodeTriggerCondition::Conflicts,
        action: CodeTriggerAction::Notify,
        enabled: true,
        created_at: at,
        updated_at: at,
    };
    assert!(arm_trigger(&store, &alice, &foreign_repo_trigger)
        .await
        .is_err());
    assert!(list_triggers_for_repo(&store, &bob, bob_repo_id)
        .await
        .unwrap()
        .is_empty());

    let trigger = CodeTrigger {
        id: CodeTriggerId::new(),
        owner: alice.clone(),
        repo_id: alice_repo_id,
        condition: CodeTriggerCondition::ChecksFailed,
        action: CodeTriggerAction::Deliver,
        enabled: true,
        created_at: at,
        updated_at: at,
    };
    arm_trigger(&store, &alice, &trigger).await.unwrap();
    let fire = insert_or_load_trigger_fire(
        &store,
        &CodeTriggerFireIdentity {
            trigger_id: trigger.id,
            owner: alice.clone(),
            workspace_id: alice_workspace_id,
            pr_number: 42,
            head_sha: "owner-boundary".to_owned(),
        },
        &trigger_payload(trigger.action, trigger.condition),
        at,
    )
    .await
    .unwrap()
    .unwrap();

    assert!(lease_trigger_fire_delivery(
        &store,
        &bob,
        fire.delivery_id,
        uuid::Uuid::new_v4(),
        at,
        at + chrono::Duration::minutes(1),
    )
    .await
    .unwrap()
    .is_none());

    let lease_token = uuid::Uuid::new_v4();
    let lease_expires_at = at + chrono::Duration::minutes(1);
    lease_trigger_fire_delivery(
        &store,
        &alice,
        fire.delivery_id,
        lease_token,
        at,
        lease_expires_at,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !acknowledge_trigger_fire_delivery(&store, &bob, fire.delivery_id, lease_token, at,)
            .await
            .unwrap()
    );
    assert!(reschedule_trigger_fire_delivery_failure(
        &store,
        &bob,
        fire.delivery_id,
        lease_token,
        at,
        "wrong owner",
    )
    .await
    .unwrap()
    .is_none());
    let still_leased = list_fires_for_workspace(&store, &alice, alice_workspace_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(still_leased.lease_token, Some(lease_token));

    let retry_at = reschedule_trigger_fire_delivery_failure(
        &store,
        &alice,
        fire.delivery_id,
        lease_token,
        at,
        "retry",
    )
    .await
    .unwrap()
    .unwrap();
    let next_token = uuid::Uuid::new_v4();
    lease_trigger_fire_delivery(
        &store,
        &alice,
        fire.delivery_id,
        next_token,
        retry_at,
        retry_at + chrono::Duration::minutes(1),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(acknowledge_trigger_fire_delivery(
        &store,
        &alice,
        fire.delivery_id,
        next_token,
        retry_at,
    )
    .await
    .unwrap());
}

/// Pull request number is part of the fire identity. A host may reuse a head
/// SHA across pull requests, while an exact retry must keep one delivery id.
#[tokio::test]
async fn trigger_fire_identity_includes_pull_request_number() {
    use crate::code::{
        CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFireIdentity,
        CodeTriggerFireState, CodeTriggerId,
    };
    use crate::db::code::{
        arm_trigger, insert_or_load_trigger_fire, list_fires_for_workspace, list_repos,
    };

    let (_dir, store, session_id, _turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let repo_id = list_repos(&store, &owner).await.unwrap()[0].id;
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;

    let trigger = CodeTrigger {
        id: CodeTriggerId::new(),
        owner: owner.clone(),
        repo_id,
        condition: CodeTriggerCondition::ChecksFailed,
        action: CodeTriggerAction::Deliver,
        enabled: true,
        created_at: now(),
        updated_at: now(),
    };
    arm_trigger(&store, &owner, &trigger).await.unwrap();

    let fired_at = now();
    let pull_40 = CodeTriggerFireIdentity {
        trigger_id: trigger.id,
        owner: owner.clone(),
        workspace_id,
        pr_number: 40,
        head_sha: "aaaa1111".to_owned(),
    };
    let payload = trigger_payload(trigger.action, trigger.condition);
    let first = insert_or_load_trigger_fire(&store, &pull_40, &payload, fired_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.identity, pull_40);
    assert_eq!(first.state, CodeTriggerFireState::Pending);
    assert_eq!(first.attempt_count, 0);
    assert_eq!(first.next_attempt_at, Some(fired_at));

    let duplicate = insert_or_load_trigger_fire(
        &store,
        &pull_40,
        &payload,
        fired_at + chrono::Duration::minutes(1),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(duplicate.delivery_id, first.delivery_id);
    assert_eq!(duplicate.fired_at, fired_at);

    let pull_41 = CodeTriggerFireIdentity {
        pr_number: 41,
        ..pull_40.clone()
    };
    let second = insert_or_load_trigger_fire(&store, &pull_41, &payload, fired_at)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(second.delivery_id, first.delivery_id);

    let fires = list_fires_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap();
    assert_eq!(fires.len(), 2);
}

/// A pending delivery keeps one id across explicit failure and lease expiry.
/// Only the current lease may change its state.
#[tokio::test]
async fn trigger_fire_delivery_retries_are_fenced_and_durable() {
    use crate::code::{
        CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFire,
        CodeTriggerFireIdentity, CodeTriggerFireState, CodeTriggerId,
    };
    use crate::db::code::{
        acknowledge_trigger_fire_delivery, arm_trigger, insert_or_load_trigger_fire,
        lease_trigger_fire_delivery, list_due_trigger_fire_deliveries_all_owners,
        list_fires_for_workspace, list_repos, reschedule_trigger_fire_delivery_failure,
    };

    let (_dir, store, session_id, _turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let repo_id = list_repos(&store, &owner).await.unwrap()[0].id;
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    let trigger = CodeTrigger {
        id: CodeTriggerId::new(),
        owner: owner.clone(),
        repo_id,
        condition: CodeTriggerCondition::ChangesRequested,
        action: CodeTriggerAction::Deliver,
        enabled: true,
        created_at: now(),
        updated_at: now(),
    };
    arm_trigger(&store, &owner, &trigger).await.unwrap();

    let fired_at = now();
    let fire = insert_or_load_trigger_fire(
        &store,
        &CodeTriggerFireIdentity {
            trigger_id: trigger.id,
            owner: owner.clone(),
            workspace_id,
            pr_number: 42,
            head_sha: "aaaa1111".to_owned(),
        },
        &trigger_payload(trigger.action, trigger.condition),
        fired_at,
    )
    .await
    .unwrap()
    .unwrap();
    let first_token = uuid::Uuid::new_v4();
    let first_expiry = fired_at + chrono::Duration::seconds(30);
    let first_lease = lease_trigger_fire_delivery(
        &store,
        &owner,
        fire.delivery_id,
        first_token,
        fired_at,
        first_expiry,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(first_lease.delivery_id, fire.delivery_id);
    assert_eq!(first_lease.attempt_count, 1);
    assert_eq!(first_lease.lease_token, Some(first_token));
    assert!(lease_trigger_fire_delivery(
        &store,
        &owner,
        fire.delivery_id,
        uuid::Uuid::new_v4(),
        fired_at,
        first_expiry,
    )
    .await
    .unwrap()
    .is_none());

    let failed_at = fired_at + chrono::Duration::seconds(1);
    let retry_at = reschedule_trigger_fire_delivery_failure(
        &store,
        &owner,
        fire.delivery_id,
        first_token,
        failed_at,
        "",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        retry_at,
        failed_at + CodeTriggerFire::retry_delay(first_lease.attempt_count)
    );
    let failed = list_fires_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        failed.last_error.as_deref(),
        Some("trigger delivery failed")
    );
    assert_eq!(failed.next_attempt_at, Some(retry_at));
    assert_eq!(failed.lease_token, None);
    assert_eq!(failed.lease_expires_at, None);
    let due = list_due_trigger_fire_deliveries_all_owners(&store, retry_at, 10)
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].delivery_id, fire.delivery_id);
    assert_eq!(
        due[0].payload.as_ref().unwrap().message,
        trigger_payload(trigger.action, trigger.condition).message
    );
    assert!(lease_trigger_fire_delivery(
        &store,
        &owner,
        fire.delivery_id,
        uuid::Uuid::new_v4(),
        retry_at - chrono::Duration::milliseconds(1),
        retry_at + chrono::Duration::seconds(30),
    )
    .await
    .unwrap()
    .is_none());

    let second_token = uuid::Uuid::new_v4();
    let second_expiry = retry_at + chrono::Duration::seconds(30);
    let second_lease = lease_trigger_fire_delivery(
        &store,
        &owner,
        fire.delivery_id,
        second_token,
        retry_at,
        second_expiry,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(second_lease.delivery_id, fire.delivery_id);
    assert_eq!(second_lease.attempt_count, 2);

    let second_failed_at = retry_at + chrono::Duration::seconds(1);
    let long_error = "x".repeat(CodeTriggerFire::MAX_LAST_ERROR_CHARS + 16);
    let second_retry_at = reschedule_trigger_fire_delivery_failure(
        &store,
        &owner,
        fire.delivery_id,
        second_token,
        second_failed_at,
        &long_error,
    )
    .await
    .unwrap()
    .unwrap();
    let second_failure = list_fires_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        second_failure.last_error.as_ref().unwrap().chars().count(),
        CodeTriggerFire::MAX_LAST_ERROR_CHARS
    );

    let third_token = uuid::Uuid::new_v4();
    let third_expiry = second_retry_at + chrono::Duration::seconds(30);
    let third_lease = lease_trigger_fire_delivery(
        &store,
        &owner,
        fire.delivery_id,
        third_token,
        second_retry_at,
        third_expiry,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(third_lease.delivery_id, fire.delivery_id);
    assert_eq!(third_lease.attempt_count, 3);

    let fourth_token = uuid::Uuid::new_v4();
    let fourth_expiry = third_expiry + chrono::Duration::seconds(30);
    let fourth_lease = lease_trigger_fire_delivery(
        &store,
        &owner,
        fire.delivery_id,
        fourth_token,
        third_expiry,
        fourth_expiry,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fourth_lease.delivery_id, fire.delivery_id);
    assert_eq!(fourth_lease.attempt_count, 4);
    assert!(!acknowledge_trigger_fire_delivery(
        &store,
        &owner,
        fire.delivery_id,
        third_token,
        third_expiry,
    )
    .await
    .unwrap());

    let delivered_at = third_expiry + chrono::Duration::seconds(1);
    assert!(acknowledge_trigger_fire_delivery(
        &store,
        &owner,
        fire.delivery_id,
        fourth_token,
        delivered_at,
    )
    .await
    .unwrap());
    let stored = list_fires_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(stored.delivery_id, fire.delivery_id);
    assert_eq!(stored.state, CodeTriggerFireState::Delivered);
    assert_eq!(stored.attempt_count, 4);
    assert_eq!(stored.delivered_at, Some(delivered_at));
    assert_eq!(stored.lease_token, None);
    assert_eq!(stored.lease_expires_at, None);
    assert_eq!(stored.next_attempt_at, None);
    assert_eq!(stored.last_error, None);
}

/// Deleting a trigger takes its fire rows with it. Leaving them would let a
/// re-armed trigger inherit the old one's suppression and stay silent on a
/// head it never actually fired for.
#[tokio::test]
async fn deleting_a_trigger_clears_its_fires() {
    use crate::code::{
        CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFireIdentity,
        CodeTriggerId,
    };
    use crate::db::code::{
        arm_trigger, delete_trigger, insert_or_load_trigger_fire, list_fires_for_workspace,
        list_repos,
    };

    let (_dir, store, session_id, _turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let repo_id = list_repos(&store, &owner).await.unwrap()[0].id;
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;

    let trigger = CodeTrigger {
        id: CodeTriggerId::new(),
        owner: owner.clone(),
        repo_id,
        condition: CodeTriggerCondition::Conflicts,
        action: CodeTriggerAction::Deliver,
        enabled: true,
        created_at: now(),
        updated_at: now(),
    };
    arm_trigger(&store, &owner, &trigger).await.unwrap();
    insert_or_load_trigger_fire(
        &store,
        &CodeTriggerFireIdentity {
            trigger_id: trigger.id,
            owner: owner.clone(),
            workspace_id,
            pr_number: 42,
            head_sha: "aaaa1111".to_owned(),
        },
        &trigger_payload(trigger.action, trigger.condition),
        now(),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(delete_trigger(&store, &owner, repo_id, trigger.id)
        .await
        .unwrap());
    assert!(list_fires_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .is_empty());
}

/// Trigger mutations share only the enabled bit. A toggle must preserve the
/// action, a later re-arm deliberately enables the rule, and a stale toggle
/// after deletion must not recreate it.
#[tokio::test]
async fn trigger_mutations_have_field_specific_serialization() {
    use crate::code::{
        CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFireIdentity,
        CodeTriggerFireState, CodeTriggerId,
    };
    use crate::db::code::{
        arm_trigger, delete_trigger, insert_or_load_trigger_fire,
        list_due_trigger_fire_deliveries_all_owners, list_fires_for_workspace, list_repos,
        list_triggers_for_repo, update_trigger_enabled,
    };

    let (_dir, store, session_id, _turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let repo_id = list_repos(&store, &owner).await.unwrap()[0].id;
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;
    let created_at = now();
    let trigger = CodeTrigger {
        id: CodeTriggerId::new(),
        owner: owner.clone(),
        repo_id,
        condition: CodeTriggerCondition::ChecksFailed,
        action: CodeTriggerAction::Notify,
        enabled: true,
        created_at,
        updated_at: created_at,
    };
    arm_trigger(&store, &owner, &trigger).await.unwrap();
    let fire_identity = CodeTriggerFireIdentity {
        trigger_id: trigger.id,
        owner: owner.clone(),
        workspace_id,
        pr_number: 42,
        head_sha: "toggle-head".to_owned(),
    };
    let fire = insert_or_load_trigger_fire(
        &store,
        &fire_identity,
        &trigger_payload(trigger.action, trigger.condition),
        created_at,
    )
    .await
    .unwrap()
    .unwrap();

    assert!(update_trigger_enabled(
        &store,
        &owner,
        repo_id,
        trigger.id,
        false,
        created_at + chrono::Duration::minutes(1),
    )
    .await
    .unwrap());
    let disabled = list_triggers_for_repo(&store, &owner, repo_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.action, CodeTriggerAction::Notify);
    let cancelled = list_fires_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(cancelled.delivery_id, fire.delivery_id);
    assert_eq!(cancelled.state, CodeTriggerFireState::Cancelled);
    assert!(cancelled.cancelled_at.is_some());
    assert!(
        list_due_trigger_fire_deliveries_all_owners(&store, now(), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(insert_or_load_trigger_fire(
        &store,
        &CodeTriggerFireIdentity {
            head_sha: "disabled-head".to_owned(),
            ..fire_identity.clone()
        },
        &trigger_payload(trigger.action, trigger.condition),
        created_at + chrono::Duration::minutes(1),
    )
    .await
    .unwrap()
    .is_none());

    let rearmed = CodeTrigger {
        id: CodeTriggerId::new(),
        action: CodeTriggerAction::Deliver,
        enabled: true,
        updated_at: created_at + chrono::Duration::minutes(2),
        ..trigger.clone()
    };
    arm_trigger(&store, &owner, &rearmed).await.unwrap();
    let stored = list_triggers_for_repo(&store, &owner, repo_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(stored.id, trigger.id);
    assert!(stored.enabled, "a later arm must enable the rule");
    assert_eq!(stored.action, CodeTriggerAction::Deliver);
    let suppressed = insert_or_load_trigger_fire(
        &store,
        &fire_identity,
        &trigger_payload(CodeTriggerAction::Deliver, trigger.condition),
        created_at + chrono::Duration::minutes(2),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(suppressed.state, CodeTriggerFireState::Cancelled);

    assert!(delete_trigger(&store, &owner, repo_id, trigger.id)
        .await
        .unwrap());
    assert!(!update_trigger_enabled(
        &store,
        &owner,
        repo_id,
        trigger.id,
        false,
        created_at + chrono::Duration::minutes(3),
    )
    .await
    .unwrap());
    assert!(list_triggers_for_repo(&store, &owner, repo_id)
        .await
        .unwrap()
        .is_empty());
}

/// A trigger turn receipt and its running turn share one transaction. If the
/// turn insert fails, the receipt must roll back so a later attempt can retry.
/// Once accepted, the same delivery id cannot create a turn in another session.
#[tokio::test]
async fn trigger_turn_acceptance_is_atomic_and_global() {
    use crate::code::{CodeTriggerAction, CodeTriggerCondition, CodeTriggerDeliverySink};
    use crate::db::code::{
        accept_trigger_delivery, accept_trigger_turn_delivery, trigger_delivery_accepted,
    };

    let (_dir, store, session_id, existing_turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let existing = get_turn(&store, &owner, existing_turn_id)
        .await
        .unwrap()
        .unwrap();
    let (delivery_id, lease_token) = claimed_trigger_delivery(
        &store,
        &owner,
        session_id,
        CodeTriggerCondition::ChangesRequested,
        CodeTriggerAction::Deliver,
    )
    .await;
    let mut duplicate = existing.clone();
    duplicate.ordinal = 2;
    assert!(accept_trigger_turn_delivery(
        &store,
        &owner,
        delivery_id,
        lease_token,
        &duplicate,
        now(),
    )
    .await
    .is_err());
    assert!(!trigger_delivery_accepted(&store, &owner, delivery_id)
        .await
        .unwrap());

    let mut accepted_turn = duplicate;
    accepted_turn.id = CodeTurnId::new();
    assert!(accept_trigger_turn_delivery(
        &store,
        &owner,
        delivery_id,
        lease_token,
        &accepted_turn,
        now(),
    )
    .await
    .unwrap());
    assert!(get_turn(&store, &owner, accepted_turn.id)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        CodeSessionLifecycle::Running
    );

    let (other_session_id, other_turn_id) = seed_owner(&store, &owner, "trigger-retry").await;
    assert!(!accept_trigger_delivery(
        &store,
        &owner,
        delivery_id,
        lease_token,
        CodeTriggerDeliverySink::Steer,
        other_session_id,
        Some(other_turn_id),
        now(),
    )
    .await
    .unwrap());
    assert_eq!(accepted_turn.session_id, session_id);
}

/// Attention acceptance locks the current session state and commits the
/// receipt with the state change. Missing sessions roll back the receipt, and
/// a retry cannot move the accepted id to another sink or session.
#[tokio::test]
async fn trigger_attention_acceptance_is_atomic_and_global() {
    use crate::code::{CodeTriggerAction, CodeTriggerCondition, CodeTriggerDeliverySink};
    use crate::db::code::{
        accept_trigger_attention_delivery, accept_trigger_delivery, trigger_delivery_accepted,
    };

    let (_dir, store, session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();
    let (missing_delivery, missing_lease) = claimed_trigger_delivery(
        &store,
        &owner,
        session_id,
        CodeTriggerCondition::Conflicts,
        CodeTriggerAction::Notify,
    )
    .await;
    let next = Attention::needs_you("checks failed", AttentionSource::Structured);
    assert!(accept_trigger_attention_delivery(
        &store,
        &owner,
        missing_delivery,
        missing_lease,
        CodeSessionId::new(),
        &next,
        now(),
    )
    .await
    .is_err());
    assert!(!trigger_delivery_accepted(&store, &owner, missing_delivery)
        .await
        .unwrap());

    let other_owner = OwnerId::new("attention-other").unwrap();
    let (other_owner_session_id, _) = seed_owner(&store, &other_owner, "attention-other").await;
    let (foreign_delivery, foreign_lease) = claimed_trigger_delivery(
        &store,
        &owner,
        session_id,
        CodeTriggerCondition::ChangesRequested,
        CodeTriggerAction::Notify,
    )
    .await;
    assert!(accept_trigger_attention_delivery(
        &store,
        &owner,
        foreign_delivery,
        foreign_lease,
        other_owner_session_id,
        &next,
        now(),
    )
    .await
    .is_err());
    assert!(!trigger_delivery_accepted(&store, &owner, foreign_delivery)
        .await
        .unwrap());

    let (policy_session_id, _) = seed_owner(&store, &owner, "attention-policy").await;
    let manual = Attention::manual("keep this state");
    replace_session_attention(&store, &owner, policy_session_id, &manual, true)
        .await
        .unwrap();
    let (policy_delivery, policy_lease) = claimed_trigger_delivery(
        &store,
        &owner,
        policy_session_id,
        CodeTriggerCondition::Conflicts,
        CodeTriggerAction::Notify,
    )
    .await;
    assert!(!accept_trigger_attention_delivery(
        &store,
        &owner,
        policy_delivery,
        policy_lease,
        policy_session_id,
        &next,
        now(),
    )
    .await
    .unwrap());
    assert!(trigger_delivery_accepted(&store, &owner, policy_delivery)
        .await
        .unwrap());
    assert_eq!(
        get_session(&store, &owner, policy_session_id)
            .await
            .unwrap()
            .unwrap()
            .attention,
        manual
    );

    let (delivery_id, lease_token) = claimed_trigger_delivery(
        &store,
        &owner,
        session_id,
        CodeTriggerCondition::ChecksFailed,
        CodeTriggerAction::Notify,
    )
    .await;
    assert!(accept_trigger_attention_delivery(
        &store,
        &owner,
        delivery_id,
        lease_token,
        session_id,
        &next,
        now(),
    )
    .await
    .unwrap());
    assert_eq!(
        get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap()
            .attention,
        next
    );

    let (other_session_id, _) = seed_owner(&store, &owner, "attention-retry").await;
    assert!(!accept_trigger_delivery(
        &store,
        &owner,
        delivery_id,
        lease_token,
        CodeTriggerDeliverySink::Steer,
        other_session_id,
        Some(turn_id),
        now(),
    )
    .await
    .unwrap());
    assert_eq!(
        get_session(&store, &owner, other_session_id)
            .await
            .unwrap()
            .unwrap()
            .attention,
        Attention::working(AttentionSource::Lifecycle)
    );
}

/// Facts and attribution (decision 77): identity upsert, claim-once, and the
/// contributed-to-authored promotion.
#[tokio::test]
async fn pull_request_facts_upsert_claim_and_promote() {
    use crate::code::{
        CodePullRequestAttribution, CodePullRequestDiscovery, CodePullRequestFact,
        CodePullRequestId, CodePullRequestLiveState, CodePullRequestRelation, CodePullRequestState,
        PullRequestCheck, PullRequestCheckBucket,
    };
    use crate::db::code::{
        count_attributed_prs_for_workspace, get_pull_request_fact, get_pull_request_fetch_state,
        insert_pull_request_attribution, list_attributed_facts_for_workspace,
        list_fact_repo_identities, promote_attribution_to_authored, save_pull_request_fact,
        set_pull_request_fetch_state, set_pull_request_live_state, PullRequestFetchCondition,
    };

    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let (session_id, _turn_id) = seed_owner(&store, &owner, "facts").await;
    let workspace_id = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap()
        .workspace_id;

    let first_seen = now();
    let fact = CodePullRequestFact {
        id: CodePullRequestId::new(),
        owner: owner.clone(),
        host: "github.com".into(),
        repo_owner: "acme".into(),
        repo_name: "tools".into(),
        number: 412,
        url: "https://github.com/acme/tools/pull/412".into(),
        title: "First".into(),
        state: CodePullRequestState::Open,
        draft: true,
        author: Some("octocat".into()),
        head_branch: "feat/x".into(),
        base_branch: "main".into(),
        head_sha: Some("aaa111".into()),
        created_at: first_seen,
        updated_at: first_seen,
        merged_at: None,
        closed_at: None,
        first_seen_at: first_seen,
        last_seen_at: first_seen,
        live: None,
    };
    let id = save_pull_request_fact(&store, &fact).await.unwrap();

    // Same identity, fresh snapshot: the id and first_seen_at hold, the
    // snapshot and last_seen_at move.
    let later = now();
    let refreshed = CodePullRequestFact {
        id: CodePullRequestId::new(),
        title: "First, retitled".into(),
        state: CodePullRequestState::Merged,
        draft: false,
        head_sha: Some("bbb222".into()),
        merged_at: Some(later),
        first_seen_at: later,
        last_seen_at: later,
        ..fact.clone()
    };
    let same_id = save_pull_request_fact(&store, &refreshed).await.unwrap();
    assert_eq!(id, same_id);
    let stored = get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.id, id);
    assert_eq!(stored.title, "First, retitled");
    assert_eq!(stored.state, CodePullRequestState::Merged);
    assert_eq!(stored.head_sha.as_deref(), Some("bbb222"));
    assert_eq!(stored.first_seen_at, first_seen);
    assert_eq!(stored.last_seen_at, later);

    assert!(set_pull_request_fetch_state(
        &store,
        &owner,
        "github.com",
        "acme",
        "tools",
        412,
        Some(&refreshed),
        PullRequestFetchCondition::Unconditional,
        Some("W/\"pull-2\""),
        Some("W/\"checks-2\""),
        Some("W/\"reviews-2\""),
    )
    .await
    .unwrap());
    let fetch_state =
        get_pull_request_fetch_state(&store, &owner, "github.com", "acme", "tools", 412)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(fetch_state.fact.title, "First, retitled");
    assert_eq!(fetch_state.fact.head_sha.as_deref(), Some("bbb222"));
    assert_eq!(fetch_state.pull_etag.as_deref(), Some("W/\"pull-2\""));
    assert_eq!(fetch_state.checks_etag.as_deref(), Some("W/\"checks-2\""));
    assert_eq!(fetch_state.reviews_etag.as_deref(), Some("W/\"reviews-2\""));

    // The live tier (decision 66): the first write reports change, an
    // identical write does not, and a snapshot upsert never blanks it.
    let live = CodePullRequestLiveState {
        checks_summary: Some("8 passing, 1 pending, 0 failing".into()),
        checks: Some(vec![PullRequestCheck {
            name: "ci".into(),
            bucket: PullRequestCheckBucket::Pending,
            detail: None,
            url: None,
        }]),
        review_decision: Some("review_required".into()),
        mergeable: Some("mergeable".into()),
        merge_state_status: Some("blocked".into()),
        auto_merge_enabled: Some(true),
        in_merge_queue: Some(false),
        observed_at: later,
    };
    let (live_id, changed) =
        set_pull_request_live_state(&store, &owner, "github.com", "acme", "tools", 412, &live)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(live_id, id);
    assert!(changed);
    let (_, changed_again) =
        set_pull_request_live_state(&store, &owner, "github.com", "acme", "tools", 412, &live)
            .await
            .unwrap()
            .unwrap();
    assert!(!changed_again, "observed_at alone is not change");
    let stored = get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .unwrap();
    let stored_live = stored.live.as_ref().unwrap();
    assert_eq!(stored_live.merge_state_status.as_deref(), Some("blocked"));
    assert_eq!(stored_live.checks.as_ref().unwrap().len(), 1);
    assert_eq!(stored_live.auto_merge_enabled, Some(true));
    save_pull_request_fact(&store, &refreshed).await.unwrap();
    let stored = get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .unwrap();
    assert!(
        stored.live.is_some(),
        "a snapshot upsert must not blank the live tier"
    );
    // The tier decorates observations; it never mints a row.
    assert!(
        set_pull_request_live_state(&store, &owner, "github.com", "acme", "tools", 999, &live)
            .await
            .unwrap()
            .is_none()
    );

    // Claim once: the second claim reports the row already exists and the
    // stored relation is untouched.
    let attribution = CodePullRequestAttribution {
        owner: owner.clone(),
        pull_request_id: id,
        workspace_id,
        relation: CodePullRequestRelation::Contributed,
        discovered_via: CodePullRequestDiscovery::Command,
        session_id: Some(session_id),
        parent_call_id: Some("task-1".into()),
        created_at: now(),
    };
    assert!(insert_pull_request_attribution(&store, &attribution)
        .await
        .unwrap());
    assert!(!insert_pull_request_attribution(
        &store,
        &CodePullRequestAttribution {
            relation: CodePullRequestRelation::Authored,
            ..attribution.clone()
        }
    )
    .await
    .unwrap());
    let listed = list_attributed_facts_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].1, CodePullRequestRelation::Contributed);

    // The promotion is the only way the losing claim's stronger relation
    // lands.
    promote_attribution_to_authored(&store, &owner, id, workspace_id)
        .await
        .unwrap();
    let listed = list_attributed_facts_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap();
    assert_eq!(listed[0].1, CodePullRequestRelation::Authored);

    assert_eq!(
        count_attributed_prs_for_workspace(&store, &owner, workspace_id)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        list_fact_repo_identities(&store, &owner).await.unwrap(),
        vec![(
            "github.com".to_owned(),
            "acme".to_owned(),
            "tools".to_owned()
        )]
    );

    // Another owner sees none of it.
    let stranger = OwnerId::new("stranger").unwrap();
    assert!(
        get_pull_request_fact(&store, &stranger, "github.com", "acme", "tools", 412)
            .await
            .unwrap()
            .is_none()
    );
    assert!(list_fact_repo_identities(&store, &stranger)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn snapshot_upsert_invalidates_a_concurrent_fetch_validator() {
    use crate::code::{CodePullRequestFact, CodePullRequestId, CodePullRequestState};
    use crate::db::code::{
        get_pull_request_fetch_state, save_pull_request_fact, set_pull_request_fetch_state,
        PullRequestFetchCondition,
    };

    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let observed = now();
    let stale = CodePullRequestFact {
        id: CodePullRequestId::new(),
        owner: owner.clone(),
        host: "github.com".into(),
        repo_owner: "acme".into(),
        repo_name: "tools".into(),
        number: 99,
        url: "https://github.com/acme/tools/pull/99".into(),
        title: "Stale".into(),
        state: CodePullRequestState::Open,
        draft: false,
        author: None,
        head_branch: "feature".into(),
        base_branch: "main".into(),
        head_sha: Some("old".into()),
        created_at: observed,
        updated_at: observed,
        merged_at: None,
        closed_at: None,
        first_seen_at: observed,
        last_seen_at: observed,
        live: None,
    };
    save_pull_request_fact(&store, &stale).await.unwrap();

    let fresh = CodePullRequestFact {
        title: "Fresh".into(),
        head_sha: Some("new".into()),
        ..stale.clone()
    };
    assert!(set_pull_request_fetch_state(
        &store,
        &owner,
        "github.com",
        "acme",
        "tools",
        99,
        Some(&fresh),
        PullRequestFetchCondition::Unconditional,
        Some("W/\"fresh\""),
        None,
        None,
    )
    .await
    .unwrap());

    save_pull_request_fact(&store, &stale).await.unwrap();

    let stored = get_pull_request_fetch_state(&store, &owner, "github.com", "acme", "tools", 99)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.fact.title, "Stale");
    assert_eq!(stored.fact.head_sha.as_deref(), Some("old"));
    assert_eq!(stored.pull_etag, None);
}

#[tokio::test]
async fn late_304_cannot_restore_an_invalidated_pull_validator() {
    use crate::code::{CodePullRequestFact, CodePullRequestId, CodePullRequestState};
    use crate::db::code::{
        get_pull_request_fetch_state, save_pull_request_fact, set_pull_request_fetch_state,
        PullRequestFetchCondition,
    };

    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let observed = now();
    let snapshot_a = CodePullRequestFact {
        id: CodePullRequestId::new(),
        owner: owner.clone(),
        host: "github.com".into(),
        repo_owner: "acme".into(),
        repo_name: "tools".into(),
        number: 100,
        url: "https://github.com/acme/tools/pull/100".into(),
        title: "Snapshot A".into(),
        state: CodePullRequestState::Open,
        draft: false,
        author: None,
        head_branch: "feature".into(),
        base_branch: "main".into(),
        head_sha: Some("aaa".into()),
        created_at: observed,
        updated_at: observed,
        merged_at: None,
        closed_at: None,
        first_seen_at: observed,
        last_seen_at: observed,
        live: None,
    };
    save_pull_request_fact(&store, &snapshot_a).await.unwrap();
    assert!(set_pull_request_fetch_state(
        &store,
        &owner,
        "github.com",
        "acme",
        "tools",
        100,
        Some(&snapshot_a),
        PullRequestFetchCondition::Unconditional,
        Some("W/\"snapshot-a\""),
        Some("W/\"checks-a\""),
        Some("W/\"reviews-a\""),
    )
    .await
    .unwrap());

    let refresh = get_pull_request_fetch_state(&store, &owner, "github.com", "acme", "tools", 100)
        .await
        .unwrap()
        .unwrap();
    let snapshot_c = CodePullRequestFact {
        title: "Snapshot C".into(),
        head_sha: Some("ccc".into()),
        ..snapshot_a.clone()
    };
    save_pull_request_fact(&store, &snapshot_c).await.unwrap();

    assert!(!set_pull_request_fetch_state(
        &store,
        &owner,
        "github.com",
        "acme",
        "tools",
        100,
        None,
        PullRequestFetchCondition::PullEtag(refresh.pull_etag.as_deref()),
        refresh.pull_etag.as_deref(),
        Some("W/\"late-checks\""),
        Some("W/\"late-reviews\""),
    )
    .await
    .unwrap());

    let stored = get_pull_request_fetch_state(&store, &owner, "github.com", "acme", "tools", 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.fact.title, "Snapshot C");
    assert_eq!(stored.fact.head_sha.as_deref(), Some("ccc"));
    assert_eq!(stored.pull_etag, None);
    assert_eq!(stored.checks_etag.as_deref(), Some("W/\"checks-a\""));
    assert_eq!(stored.reviews_etag.as_deref(), Some("W/\"reviews-a\""));
}

/// A whole-row turn save cannot blank a recap that landed while it was held.
///
/// The two writers genuinely overlap. A recap is derived after the turn ends
/// and takes seconds; `checkpoint::after_turn_ended` holds a `CodeTurn` read
/// before the turn was even terminal and saves it once the git work finishes.
/// While `save_turn` still wrote `narrative`, whichever landed second won, so
/// the recap survived or vanished depending on how long a checkpoint took.
#[tokio::test]
async fn saving_a_turn_does_not_blank_its_narrative() {
    let (_dir, store, _session_id, turn_id) = seeded_session().await;
    let owner = OwnerId::local();

    set_turn_narrative(
        &store,
        &owner,
        turn_id,
        "Tests pass. Next: the refresh path.",
    )
    .await
    .unwrap();

    // The snapshot a checkpoint writer holds: read before the recap existed.
    let mut stale = get_turn(&store, &owner, turn_id).await.unwrap().unwrap();
    stale.narrative = None;
    stale.checkpoint_ref = Some("refs/tidebreak/checkpoints/ws/1".into());
    assert!(save_turn(&store, &owner, &stale).await.unwrap());

    let stored = get_turn(&store, &owner, turn_id).await.unwrap().unwrap();
    assert_eq!(
        stored.narrative.as_deref(),
        Some("Tests pass. Next: the refresh path."),
        "the checkpoint save must not carry a stale narrative over the stored one"
    );
    assert_eq!(
        stored.checkpoint_ref.as_deref(),
        Some("refs/tidebreak/checkpoints/ws/1"),
        "and it still writes what it owns"
    );
}

fn queued_message(session_id: CodeSessionId, message: &str) -> CodeQueuedTurn {
    CodeQueuedTurn {
        id: CodeTurnId::new(),
        session_id,
        message: message.to_owned(),
        attachments: Vec::new(),
        position: 0,
        created_at: now(),
        updated_at: now(),
    }
}

fn turn_for(row: &CodeQueuedTurn, ordinal: i64) -> CodeTurn {
    CodeTurn {
        id: row.id,
        session_id: row.session_id,
        ordinal,
        status: CodeTurnStatus::Running,
        model: None,
        fast_mode: false,
        user_input: row.message.clone(),
        user_input_blob_id: None,
        attachments: row.attachments.clone(),
        checkpoint_ref: None,
        diffstat: None,
        usage: None,
        narrative: None,
        started_at: now(),
        ended_at: None,
    }
}

#[tokio::test]
async fn code_queued_turns_are_fifo_and_promote_into_their_own_turn_id() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();

    let first = enqueue_queued_turn(&store, &owner, &queued_message(session_id, "first"))
        .await
        .unwrap();
    let second = enqueue_queued_turn(&store, &owner, &queued_message(session_id, "second"))
        .await
        .unwrap();
    assert_eq!(first.position, 0);
    assert_eq!(second.position, 1);

    let head = queued_turn_head(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.id, first.id);

    // Promotion deletes the row and inserts the turn together, under the
    // row's own id.
    assert!(
        promote_queued_turn(&store, &owner, &head, &turn_for(&head, 2))
            .await
            .unwrap()
    );
    let promoted = get_turn(&store, &owner, first.id).await.unwrap().unwrap();
    assert_eq!(promoted.user_input, "first");
    let remaining = list_queued_turns(&store, &owner, session_id).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, second.id);

    // A second promotion of the same snapshot must refuse: the row is gone.
    assert!(
        !promote_queued_turn(&store, &owner, &head, &turn_for(&head, 3))
            .await
            .unwrap(),
        "a spent snapshot must not promote twice"
    );
}

#[tokio::test]
async fn an_edited_or_retracted_row_refuses_a_stale_promotion() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();

    let row = enqueue_queued_turn(&store, &owner, &queued_message(session_id, "original"))
        .await
        .unwrap();
    let snapshot = queued_turn_head(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();

    // An edit after the snapshot bumps updated_at, so the stale snapshot must
    // not run the old content — and must write no turn at all.
    let edited = update_queued_turn(&store, &owner, session_id, row.id, Some("edited"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(edited.message, "edited");
    assert!(
        !promote_queued_turn(&store, &owner, &snapshot, &turn_for(&snapshot, 2))
            .await
            .unwrap()
    );
    assert!(get_turn(&store, &owner, row.id).await.unwrap().is_none());

    // The fresh head promotes; a retracted row refuses the same way.
    let fresh = queued_turn_head(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(delete_queued_turn(&store, &owner, session_id, fresh.id)
        .await
        .unwrap());
    assert!(
        !promote_queued_turn(&store, &owner, &fresh, &turn_for(&fresh, 2))
            .await
            .unwrap()
    );
    assert!(list_queued_turns(&store, &owner, session_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn code_queue_reorders_stay_dense_and_the_cap_holds() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();

    let a = enqueue_queued_turn(&store, &owner, &queued_message(session_id, "a"))
        .await
        .unwrap();
    let _b = enqueue_queued_turn(&store, &owner, &queued_message(session_id, "b"))
        .await
        .unwrap();
    let c = enqueue_queued_turn(&store, &owner, &queued_message(session_id, "c"))
        .await
        .unwrap();

    // Move the tail first; every position rewrites densely.
    update_queued_turn(&store, &owner, session_id, c.id, None, Some(0))
        .await
        .unwrap()
        .unwrap();
    let rows = list_queued_turns(&store, &owner, session_id).await.unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (row.message.as_str(), row.position))
            .collect::<Vec<_>>(),
        vec![("c", 0), ("a", 1), ("b", 2)]
    );

    // Deleting a middle row keeps order; the cap refuses the 33rd message.
    assert!(delete_queued_turn(&store, &owner, session_id, a.id)
        .await
        .unwrap());
    for index in 0..(CodeQueuedTurn::MAX_PER_SESSION - 2) {
        enqueue_queued_turn(
            &store,
            &owner,
            &queued_message(session_id, &format!("fill {index}")),
        )
        .await
        .unwrap();
    }
    let overflow =
        enqueue_queued_turn(&store, &owner, &queued_message(session_id, "too many")).await;
    assert!(overflow.is_err(), "the per-session cap must hold");
}

#[tokio::test]
async fn code_queue_pause_round_trips_and_an_ended_session_clears_its_rows() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();

    assert!(!queue_paused(&store, &owner, session_id).await.unwrap());
    set_queue_paused(&store, &owner, session_id, true)
        .await
        .unwrap();
    assert!(queue_paused(&store, &owner, session_id).await.unwrap());

    // The pause anchors on the owner's session row: a foreign owner reads
    // the default and cannot flip it.
    let other = OwnerId::new("intruder").unwrap();
    assert!(!queue_paused(&store, &other, session_id).await.unwrap());
    assert!(set_queue_paused(&store, &other, session_id, false)
        .await
        .is_err());
    assert!(queue_paused(&store, &owner, session_id).await.unwrap());

    set_queue_paused(&store, &owner, session_id, false)
        .await
        .unwrap();
    assert!(!queue_paused(&store, &owner, session_id).await.unwrap());

    enqueue_queued_turn(&store, &owner, &queued_message(session_id, "one"))
        .await
        .unwrap();
    enqueue_queued_turn(&store, &owner, &queued_message(session_id, "two"))
        .await
        .unwrap();
    assert_eq!(
        delete_session_queued_turns(&store, &owner, session_id)
            .await
            .unwrap(),
        2
    );
    assert!(list_queued_turns(&store, &owner, session_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn another_owner_cannot_see_or_touch_a_code_queue() {
    let (_dir, store, session_id, _turn) = seeded_session().await;
    let owner = OwnerId::local();
    let other = OwnerId::new("intruder").unwrap();

    let row = enqueue_queued_turn(&store, &owner, &queued_message(session_id, "mine"))
        .await
        .unwrap();

    assert!(list_queued_turns(&store, &other, session_id)
        .await
        .unwrap()
        .is_empty());
    assert!(queued_turn_head(&store, &other, session_id)
        .await
        .unwrap()
        .is_none());
    assert!(
        update_queued_turn(&store, &other, session_id, row.id, Some("stolen"), None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!delete_queued_turn(&store, &other, session_id, row.id)
        .await
        .unwrap());
    assert!(
        !promote_queued_turn(&store, &other, &row, &turn_for(&row, 2))
            .await
            .unwrap(),
        "a foreign owner's promotion must refuse"
    );
    assert_eq!(
        list_queued_turns(&store, &owner, session_id)
            .await
            .unwrap()
            .len(),
        1
    );
}
