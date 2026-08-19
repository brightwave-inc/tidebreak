use super::temp_store;
use crate::code::{
    Attention, AttentionSource, CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState,
    CodeEvent, CodePermissionMode, CodeRepo, CodeSession, CodeSessionId, CodeSessionLifecycle,
    CodeTurn, CodeTurnId, CodeTurnStatus, CodeWorkspace, CodeWorkspaceStatus, HarnessKind, RepoId,
    WorkspaceId,
};
use crate::db::code::{
    append_event, bump_spawn_epoch, get_approval, get_repo, get_session, get_turn, get_workspace,
    insert_approval, insert_repo, insert_session, insert_turn, insert_workspace, list_approvals,
    list_events, list_repos, list_sessions, list_turns, save_session, set_workspace_title_if,
    CodeJournalError,
};
use crate::OwnerId;
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
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: Some("2.1.233".into()),
            harness_resume_ref: None,
            permission_mode: CodePermissionMode::Ask,
            model: None,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
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
    let events = list_events(&store, &OwnerId::local(), session_id, 0)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[1].seq, 2);
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
    let events = list_events(&store, &OwnerId::local(), session_id, 0)
        .await
        .unwrap();
    assert_eq!(events.len(), n as usize);
    assert!(events.windows(2).all(|pair| pair[0].seq + 1 == pair[1].seq));
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
        list_events(&store, &alice, alice_session, 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(list_events(&store, &bob, alice_session, 0)
        .await
        .unwrap()
        .is_empty());

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
            "code_turn",
            "code_turn_attachment",
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
