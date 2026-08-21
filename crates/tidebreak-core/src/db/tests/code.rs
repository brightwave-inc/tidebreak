use super::temp_store;
use crate::attention::{Attention, AttentionSource};
use crate::code::{
    CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState, CodeEvent, CodeRepo,
    CodeSession, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeSubagentStatus,
    CodeSubagentSummary, CodeTurn, CodeTurnId, CodeTurnStatus, CodeWorkspace, CodeWorkspaceStatus,
    HarnessKind, RepoId, WorkspaceId,
};
use crate::db::code::{
    append_event, bump_spawn_epoch, get_approval, get_repo, get_repo_by_root_path, get_session,
    get_turn, get_workspace, insert_approval, insert_repo, insert_session, insert_turn,
    insert_workspace, list_approvals, list_events, list_repos, list_sessions, list_turns,
    mark_repo_removed, save_session, set_session_subagents, set_workspace_title_if,
    CodeJournalError, MAX_REPLAY_EVENTS,
};
use crate::OwnerId;
use crate::PermissionMode;
use chrono::Utc;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
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
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
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
    let entities = include_str!("../entities.rs");
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
                visit(&path, &text);
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
            lifecycle: CodeSessionLifecycle::Running,
            fence_reason: None,
            child_pid: None,
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
    use crate::db::code::{list_repos, list_triggers_for_repo, save_trigger};

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
    save_trigger(&store, &armed).await.unwrap();

    let rearmed = CodeTrigger {
        id: CodeTriggerId::new(),
        action: CodeTriggerAction::Deliver,
        updated_at: now() + chrono::Duration::minutes(1),
        ..armed.clone()
    };
    save_trigger(&store, &rearmed).await.unwrap();

    let stored = list_triggers_for_repo(&store, &owner, repo_id)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "re-arming created a second rule");
    assert_eq!(stored[0].action, CodeTriggerAction::Deliver);
    assert_eq!(stored[0].id, armed.id, "the identity moved on an edit");
}

/// The fire fingerprint is what makes a trigger fire on an edge. Two sweeps
/// finding the same condition against the same head must produce one fire, and
/// a new head must produce another — a wrong implementation that keys only on
/// the trigger fires once and then never again.
#[tokio::test]
async fn a_trigger_fires_once_per_head_and_again_on_the_next() {
    use crate::code::{
        CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFire, CodeTriggerId,
    };
    use crate::db::code::{
        insert_trigger_fire, list_fires_for_workspace, list_repos, save_trigger,
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
    save_trigger(&store, &trigger).await.unwrap();

    let fire = CodeTriggerFire {
        trigger_id: trigger.id,
        owner: owner.clone(),
        workspace_id,
        pr_number: 42,
        head_sha: "aaaa1111".to_owned(),
        fired_at: now(),
    };
    assert!(insert_trigger_fire(&store, &fire).await.unwrap());
    assert!(
        !insert_trigger_fire(&store, &fire).await.unwrap(),
        "the same head fired twice"
    );

    let next_head = CodeTriggerFire {
        head_sha: "bbbb2222".to_owned(),
        fired_at: now() + chrono::Duration::minutes(1),
        ..fire.clone()
    };
    assert!(
        insert_trigger_fire(&store, &next_head).await.unwrap(),
        "a new head did not fire"
    );

    let fires = list_fires_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap();
    assert_eq!(fires.len(), 2);
}

/// Deleting a trigger takes its fire rows with it. Leaving them would let a
/// re-armed trigger inherit the old one's suppression and stay silent on a
/// head it never actually fired for.
#[tokio::test]
async fn deleting_a_trigger_clears_its_fires() {
    use crate::code::{
        CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFire, CodeTriggerId,
    };
    use crate::db::code::{
        delete_trigger, insert_trigger_fire, list_fires_for_workspace, list_repos, save_trigger,
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
    save_trigger(&store, &trigger).await.unwrap();
    insert_trigger_fire(
        &store,
        &CodeTriggerFire {
            trigger_id: trigger.id,
            owner: owner.clone(),
            workspace_id,
            pr_number: 42,
            head_sha: "aaaa1111".to_owned(),
            fired_at: now(),
        },
    )
    .await
    .unwrap();

    assert!(delete_trigger(&store, &owner, trigger.id).await.unwrap());
    assert!(list_fires_for_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .is_empty());
}
