use super::*;
use tidebreak_core::code::{CodeIncarnationId, IncarnationAdmission};
use tidebreak_core::db::code::*;
use tidebreak_core::{ExecutionLocation, FenceReason, Turn, TurnId};

async fn fixture() -> (tempfile::TempDir, DbStore, Session, CodeIncarnationId, Turn) {
    let dir = tempfile::tempdir().unwrap();
    let db = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("startup.db").display()
    ))
    .await
    .unwrap();
    let mut session = super::tests::session_with(Attention::working(AttentionSource::Lifecycle));
    session.workspace_id = None;
    session.execution_location = ExecutionLocation::Sandbox;
    session.lifecycle = SessionLifecycle::Running;
    session.created_at = Utc::now() - chrono::Duration::minutes(2);
    insert_session(&db, &session).await.unwrap();
    let incarnation = activate(&db, &session, 1).await;
    let turn = deliver(&db, &session, incarnation, 1, None).await;
    (dir, db, session, incarnation, turn)
}

async fn activate(db: &DbStore, session: &Session, ordinal: i32) -> CodeIncarnationId {
    let IncarnationAdmission::Admitted(row) =
        create_incarnation_intent(db, &session.owner, session.id, ordinal, 10)
            .await
            .unwrap()
    else {
        panic!("incarnation admitted")
    };
    activate_incarnation(db, &session.owner, row.id, &format!("sandbox-{ordinal}"))
        .await
        .unwrap();
    row.id
}

async fn deliver(
    db: &DbStore,
    session: &Session,
    incarnation: CodeIncarnationId,
    ordinal: i64,
    message_seq: Option<i64>,
) -> Turn {
    let turn = Turn {
        id: TurnId::new(),
        session_id: session.id,
        ordinal,
        status: TurnStatus::Running,
        actor: None,
        model: None,
        fast_mode: false,
        user_input: "inspect the repository".into(),
        user_input_blob_id: None,
        attachments: vec![],
        checkpoint_ref: None,
        diffstat: None,
        usage: None,
        narrative: None,
        rewrite: None,
        started_at: session.created_at,
        ended_at: None,
        park_ref: None,
        park_wait: None,
    };
    insert_remote_turn_with_input(db, &session.owner, incarnation, &turn, None, message_seq)
        .await
        .unwrap()
}

async fn runtime(db: &DbStore, incarnation: CodeIncarnationId) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    db.set_setting(
        &format!("code.incarnations.{incarnation}.steering_protocol"),
        &serde_json::json!({
            "turn_identity_protocol": 1,
            "sandbox_id": "sandbox-1",
            "runtime_id": id,
        }),
    )
    .await
    .unwrap();
    id
}

async fn assert_stall(db: &DbStore, session: &Session, stalled: bool) {
    // A fresh bus proves that persisted evidence survives a server restart.
    let bus = CodeEventBus::default();
    let computed = compute_attention(
        db,
        &bus,
        session,
        ComputeOpts {
            now: Utc::now() + chrono::Duration::minutes(2),
            ..ComputeOpts::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        matches!(computed.state, AttentionState::Stalled { .. }),
        stalled
    );
    // Zero exercises the sweep without changing event timestamps or sleeping.
    sweep_stalled(db, &bus, 0).await.unwrap();
    let stored = get_session(db, &session.owner, session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        matches!(stored.attention.state, AttentionState::Stalled { .. }),
        stalled
    );
    if !stalled {
        assert_eq!(computed.state, AttentionState::Working);
        assert_eq!(stored.attention.state, AttentionState::Working);
    }
}

#[tokio::test]
async fn remote_stall_waits_for_each_native_turn_start() {
    let (_dir, db, session, incarnation, mut first) = fixture().await;
    assert_stall(&db, &session, false).await;
    let current = runtime(&db, incarnation).await;
    assert_stall(&db, &session, false).await;

    // An earlier runtime's start cannot prove that the current runtime runs.
    let old = uuid::Uuid::new_v4();
    observe_native_turn(
        &db,
        &session.owner,
        session.id,
        incarnation,
        old,
        1,
        "spawn_task",
        &[],
    )
    .await
    .unwrap();
    project_native_turn(&db, &session.owner, session.id, incarnation, old, first.id)
        .await
        .unwrap();
    assert_stall(&db, &session, false).await;
    observe_native_turn(
        &db,
        &session.owner,
        session.id,
        incarnation,
        current,
        1,
        "spawn_task",
        &[],
    )
    .await
    .unwrap();
    assert_stall(&db, &session, false).await;
    project_native_turn(
        &db,
        &session.owner,
        session.id,
        incarnation,
        current,
        first.id,
    )
    .await
    .unwrap();
    assert_stall(&db, &session, true).await;

    first.status = TurnStatus::Completed;
    save_turn(&db, &session.owner, &first).await.unwrap();
    let follow = deliver(&db, &session, incarnation, 2, Some(17)).await;
    assert_stall(&db, &session, false).await;
    observe_native_turn(
        &db,
        &session.owner,
        session.id,
        incarnation,
        current,
        2,
        "inbox",
        &[17],
    )
    .await
    .unwrap();
    project_native_turn(
        &db,
        &session.owner,
        session.id,
        incarnation,
        current,
        follow.id,
    )
    .await
    .unwrap();
    assert_stall(&db, &session, true).await;
}

#[tokio::test]
async fn remote_stall_legacy_start_survives_a_long_journal_and_reincarnation() {
    let (_dir, db, session, incarnation, mut first) = fixture().await;
    append_event(
        &db,
        &session.owner,
        session.id,
        session.spawn_epoch,
        &Event::TurnStarted {
            turn_id: TurnId::new(),
        },
    )
    .await
    .unwrap();
    assert_stall(&db, &session, false).await;
    append_event(
        &db,
        &session.owner,
        session.id,
        session.spawn_epoch,
        &Event::TurnStarted { turn_id: first.id },
    )
    .await
    .unwrap();
    for _ in 0..=ACTIVITY_EVENT_WINDOW {
        append_event(
            &db,
            &session.owner,
            session.id,
            session.spawn_epoch,
            &Event::AssistantMessage {
                text: "working".into(),
                parent_call_id: None,
            },
        )
        .await
        .unwrap();
    }
    assert_stall(&db, &session, true).await;

    first.status = TurnStatus::Completed;
    save_turn(&db, &session.owner, &first).await.unwrap();
    stop_incarnation(&db, &session.owner, incarnation, Some("completed"))
        .await
        .unwrap();
    let next = activate(&db, &session, 2).await;
    deliver(&db, &session, next, 2, None).await;
    assert_stall(&db, &session, false).await;
}

#[tokio::test]
async fn remote_stall_guard_preserves_local_stalls_and_terminal_startup_failures() {
    let (_dir, db, mut session, incarnation, _turn) = fixture().await;
    let mut local = session.clone();
    local.id = SessionId::new();
    local.execution_location = ExecutionLocation::Machine;
    insert_session(&db, &local).await.unwrap();
    assert_stall(&db, &local, true).await;

    stop_incarnation(
        &db,
        &session.owner,
        incarnation,
        Some("provisioning_failed"),
    )
    .await
    .unwrap();
    save_session(&db, &session).await.unwrap();
    assert_stall(&db, &session, true).await;
    let reason = FenceReason::SandboxLost {
        detail: "provisioning failed".into(),
    };
    session.lifecycle = SessionLifecycle::Fenced;
    session.fence_reason = Some(reason.clone());
    session.attention = Attention::new(
        AttentionState::Fenced { reason },
        AttentionSource::Lifecycle,
    );
    save_session(&db, &session).await.unwrap();
    replace_session_attention(&db, &session.owner, session.id, &session.attention, false)
        .await
        .unwrap();
    assert_eq!(
        compute_attention(
            &db,
            &CodeEventBus::default(),
            &session,
            ComputeOpts::default()
        )
        .await
        .unwrap()
        .state,
        session.attention.state
    );
    sweep_stalled(&db, &CodeEventBus::default(), 0)
        .await
        .unwrap();
    assert_eq!(
        get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap()
            .attention
            .state,
        session.attention.state
    );
}
