use super::*;
use crate::id::RootAttachmentChangeId;
use crate::model::{
    BeginRootAttachmentChange, RootAttachmentChange, RootAttachmentChangeAction,
    RootAttachmentChangeFailure, RootAttachmentChangePhase, RootAttachmentChangeTerminal,
    RootAttachmentSubjectKind,
};
use crate::storage::{
    BeginRootAttachmentChangeOutcome, FinishRootAttachmentChangeOutcome,
    MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};

fn at(second: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_710_000_000 + second, 0).unwrap()
}

fn root_id() -> HostRootId {
    HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap()
}

fn begin_request(
    chat: &Chat,
    executor_id: uuid::Uuid,
    root_id: HostRootId,
    action: RootAttachmentChangeAction,
    expected_attachment_revision: i64,
    second: i64,
) -> BeginRootAttachmentChange {
    BeginRootAttachmentChange {
        id: RootAttachmentChangeId::new(),
        chat_id: chat.id,
        executor_id,
        root_id,
        action,
        expected_attachment_revision,
        created_at: at(second),
    }
}

fn begun(outcome: BeginRootAttachmentChangeOutcome) -> RootAttachmentChange {
    match outcome {
        BeginRootAttachmentChangeOutcome::Begun(change) => change,
        other => panic!("expected begun attachment change, got {other:?}"),
    }
}

fn finished(outcome: FinishRootAttachmentChangeOutcome) -> RootAttachmentChange {
    match outcome {
        FinishRootAttachmentChangeOutcome::Finished(change) => change,
        other => panic!("expected finished attachment change, got {other:?}"),
    }
}

fn completed(attached: bool, changed: bool) -> RootAttachmentChangeTerminal {
    RootAttachmentChangeTerminal::Completed {
        broker_changed: changed,
        broker_currently_attached: attached,
    }
}

fn failed(code: &str) -> RootAttachmentChangeTerminal {
    RootAttachmentChangeTerminal::Failed {
        broker_changed: None,
        broker_currently_attached: None,
        failure: RootAttachmentChangeFailure {
            code: code.into(),
            message: format!("{code} failure"),
            retryable: true,
        },
    }
}

#[tokio::test]
async fn attach_intent_success_and_failure_rollback_are_atomic() {
    let (_dir, store) = temp_store().await;
    let executor = uuid::Uuid::new_v4();

    let success_chat = sample_chat();
    store.create_chat(&success_chat).await.unwrap();
    let success_root = root_id();
    let success_request = begin_request(
        &success_chat,
        executor,
        success_root,
        RootAttachmentChangeAction::Attach,
        0,
        0,
    );
    let pending = begun(
        store
            .begin_root_attachment_change(&success_request)
            .await
            .unwrap(),
    );
    assert_eq!(pending.phase, RootAttachmentChangePhase::AwaitingBroker);
    assert_eq!(pending.before_revision, 0);
    assert_eq!(pending.intent_revision, 1);
    assert!(!pending.projection_existed_before);
    assert_eq!(pending.origin, Some(RootAttachmentOrigin::Conversation));
    assert_eq!(pending.projection_position, Some(0));
    let intent_chat = store.get_chat(success_chat.id).await.unwrap().unwrap();
    assert_eq!(intent_chat.attachment_revision, 1);
    assert_eq!(
        intent_chat.root_attachments,
        vec![ChatRootAttachment {
            root_id: success_root,
            origin: RootAttachmentOrigin::Conversation,
        }]
    );

    let completed_change = finished(
        store
            .finish_root_attachment_change(
                success_request.id,
                executor,
                &completed(true, true),
                at(1),
            )
            .await
            .unwrap(),
    );
    assert_eq!(completed_change.phase, RootAttachmentChangePhase::Completed);
    assert_eq!(completed_change.result_revision, Some(1));
    assert_eq!(completed_change.projection_changed, Some(true));
    assert_eq!(
        store
            .get_chat(success_chat.id)
            .await
            .unwrap()
            .unwrap()
            .root_attachments,
        intent_chat.root_attachments
    );

    let failure_chat = sample_chat();
    store.create_chat(&failure_chat).await.unwrap();
    let failure_root = root_id();
    let failure_request = begin_request(
        &failure_chat,
        executor,
        failure_root,
        RootAttachmentChangeAction::Attach,
        0,
        2,
    );
    begun(
        store
            .begin_root_attachment_change(&failure_request)
            .await
            .unwrap(),
    );
    let failed_change = finished(
        store
            .finish_root_attachment_change(failure_request.id, executor, &failed("denied"), at(3))
            .await
            .unwrap(),
    );
    assert_eq!(failed_change.phase, RootAttachmentChangePhase::Failed);
    assert_eq!(failed_change.result_revision, Some(2));
    assert_eq!(failed_change.projection_changed, Some(false));
    let rolled_back = store.get_chat(failure_chat.id).await.unwrap().unwrap();
    assert_eq!(rolled_back.attachment_revision, 2);
    assert!(rolled_back.root_attachments.is_empty());
}

#[tokio::test]
async fn detach_waits_for_broker_then_removes_or_preserves_and_compacts_positions() {
    let (_dir, store) = temp_store().await;
    let executor = uuid::Uuid::new_v4();
    let roots = [root_id(), root_id(), root_id()];

    let mut success_chat = sample_chat();
    success_chat.attachment_revision = 7;
    success_chat.root_attachments = roots
        .iter()
        .copied()
        .map(|root_id| ChatRootAttachment {
            root_id,
            origin: RootAttachmentOrigin::Conversation,
        })
        .collect();
    store.create_chat(&success_chat).await.unwrap();
    let detach = begin_request(
        &success_chat,
        executor,
        roots[1],
        RootAttachmentChangeAction::Detach,
        7,
        0,
    );
    let pending = begun(store.begin_root_attachment_change(&detach).await.unwrap());
    assert!(pending.projection_existed_before);
    assert_eq!(pending.projection_position, Some(1));
    assert_eq!(pending.intent_revision, 7);
    assert_eq!(
        store.get_chat(success_chat.id).await.unwrap().unwrap(),
        success_chat
    );

    let completed_change = finished(
        store
            .finish_root_attachment_change(detach.id, executor, &completed(false, true), at(1))
            .await
            .unwrap(),
    );
    assert_eq!(completed_change.result_revision, Some(8));
    assert_eq!(completed_change.projection_changed, Some(true));
    let detached = store.get_chat(success_chat.id).await.unwrap().unwrap();
    assert_eq!(detached.attachment_revision, 8);
    assert_eq!(
        detached
            .root_attachments
            .iter()
            .map(|attachment| attachment.root_id)
            .collect::<Vec<_>>(),
        vec![roots[0], roots[2]]
    );

    let mut failure_chat = sample_chat();
    failure_chat.attachment_revision = 4;
    failure_chat.root_attachments = vec![ChatRootAttachment {
        root_id: roots[1],
        origin: RootAttachmentOrigin::Conversation,
    }];
    store.create_chat(&failure_chat).await.unwrap();
    let failed_detach = begin_request(
        &failure_chat,
        executor,
        roots[1],
        RootAttachmentChangeAction::Detach,
        4,
        2,
    );
    begun(
        store
            .begin_root_attachment_change(&failed_detach)
            .await
            .unwrap(),
    );
    let failed_change = finished(
        store
            .finish_root_attachment_change(
                failed_detach.id,
                executor,
                &failed("broker_unavailable"),
                at(3),
            )
            .await
            .unwrap(),
    );
    assert_eq!(failed_change.phase, RootAttachmentChangePhase::Failed);
    assert_eq!(failed_change.result_revision, Some(4));
    assert_eq!(failed_change.projection_changed, Some(false));
    assert_eq!(
        store.get_chat(failure_chat.id).await.unwrap().unwrap(),
        failure_chat
    );
}

#[tokio::test]
async fn begin_and_finish_retries_are_exact_and_conflicts_fail_closed() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let executor = uuid::Uuid::new_v4();
    let request = begin_request(
        &chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        0,
    );
    let begun_change = begun(store.begin_root_attachment_change(&request).await.unwrap());
    assert_eq!(
        store.begin_root_attachment_change(&request).await.unwrap(),
        BeginRootAttachmentChangeOutcome::Existing(begun_change.clone())
    );

    let mut conflicting = request;
    conflicting.root_id = root_id();
    assert_eq!(
        store
            .begin_root_attachment_change(&conflicting)
            .await
            .unwrap(),
        BeginRootAttachmentChangeOutcome::IdentityConflict
    );

    assert_eq!(
        store
            .finish_root_attachment_change(
                request.id,
                uuid::Uuid::new_v4(),
                &completed(true, true),
                at(1),
            )
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::ExecutorMismatch
    );
    assert_eq!(
        store.get_root_attachment_change(request.id).await.unwrap(),
        Some(begun_change)
    );

    let terminal = completed(true, true);
    let completed_change = finished(
        store
            .finish_root_attachment_change(request.id, executor, &terminal, at(1))
            .await
            .unwrap(),
    );
    assert_eq!(
        store
            .finish_root_attachment_change(request.id, executor, &terminal, at(2))
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::Existing(completed_change.clone())
    );
    assert_eq!(completed_change.finished_at, Some(at(1)));
    assert_eq!(
        store
            .finish_root_attachment_change(request.id, executor, &completed(true, false), at(1))
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::AlreadyTerminal(completed_change)
    );
    assert_eq!(
        store
            .finish_root_attachment_change(
                RootAttachmentChangeId::new(),
                executor,
                &terminal,
                at(1),
            )
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::NotFound
    );
}

#[tokio::test]
async fn one_pending_change_owns_each_chat_and_stale_revisions_are_rejected() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let executor = uuid::Uuid::new_v4();

    let stale = begin_request(
        &chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        1,
        0,
    );
    assert_eq!(
        store.begin_root_attachment_change(&stale).await.unwrap(),
        BeginRootAttachmentChangeOutcome::RevisionConflict {
            current_attachment_revision: 0,
        }
    );

    let first = begin_request(
        &chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        1,
    );
    let first_change = begun(store.begin_root_attachment_change(&first).await.unwrap());
    let second = begin_request(
        &chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        1,
        2,
    );
    assert_eq!(
        store.begin_root_attachment_change(&second).await.unwrap(),
        BeginRootAttachmentChangeOutcome::ChatBusy
    );
    assert_eq!(
        store.get_root_attachment_change(first.id).await.unwrap(),
        Some(first_change)
    );
}

#[tokio::test]
async fn concurrent_begins_serialize_to_one_pending_change() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let executor = uuid::Uuid::new_v4();
    let requests = [
        begin_request(
            &chat,
            executor,
            root_id(),
            RootAttachmentChangeAction::Attach,
            0,
            0,
        ),
        begin_request(
            &chat,
            executor,
            root_id(),
            RootAttachmentChangeAction::Attach,
            0,
            1,
        ),
    ];
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(requests.len()));
    let tasks = requests.into_iter().map(|request| {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store.begin_root_attachment_change(&request).await
        })
    });
    let outcomes = futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|task| task.expect("concurrent begin task panicked"))
        .collect::<Vec<_>>();

    let outcomes = outcomes
        .into_iter()
        .map(|outcome| outcome.expect("concurrent begin leaked a database error"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BeginRootAttachmentChangeOutcome::Begun(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BeginRootAttachmentChangeOutcome::ChatBusy))
            .count(),
        1
    );
    assert_eq!(
        store
            .list_pending_root_attachment_changes(executor, 2)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn concurrent_exact_finishes_converge_on_one_terminal_change() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let executor = uuid::Uuid::new_v4();
    let request = begin_request(
        &chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        0,
    );
    begun(store.begin_root_attachment_change(&request).await.unwrap());

    let terminal = completed(true, true);
    let finished_at = [at(1), at(2)];
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(finished_at.len()));
    let tasks = finished_at.into_iter().map(|finished_at| {
        let store = store.clone();
        let barrier = barrier.clone();
        let terminal = terminal.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .finish_root_attachment_change(request.id, executor, &terminal, finished_at)
                .await
        })
    });
    let outcomes = futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|task| task.expect("concurrent finish task panicked"))
        .collect::<Vec<_>>();

    let outcomes = outcomes
        .into_iter()
        .map(|outcome| outcome.expect("concurrent finish leaked a database error"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, FinishRootAttachmentChangeOutcome::Finished(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, FinishRootAttachmentChangeOutcome::Existing(_)))
            .count(),
        1
    );
    let committed = outcomes.iter().find_map(|outcome| match outcome {
        FinishRootAttachmentChangeOutcome::Finished(change)
        | FinishRootAttachmentChangeOutcome::Existing(change) => Some(change),
        _ => None,
    });
    assert!(outcomes.iter().all(|outcome| match outcome {
        FinishRootAttachmentChangeOutcome::Finished(change)
        | FinishRootAttachmentChangeOutcome::Existing(change) => Some(change) == committed,
        _ => false,
    }));
}

#[tokio::test]
async fn subjects_are_derived_from_authoritative_chat_ownership() {
    let (_dir, store) = temp_store().await;
    let executor = uuid::Uuid::new_v4();

    let standalone = sample_chat();
    store.create_chat(&standalone).await.unwrap();
    let standalone_request = begin_request(
        &standalone,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        0,
    );
    let standalone_change = begun(
        store
            .begin_root_attachment_change(&standalone_request)
            .await
            .unwrap(),
    );
    assert_eq!(
        standalone_change.subject_kind,
        RootAttachmentSubjectKind::Conversation
    );
    assert_eq!(standalone_change.subject_id, *standalone.id.as_uuid());

    let project = sample_project();
    store.create_project(&project).await.unwrap();
    let mut project_chat = sample_chat();
    project_chat.project_id = Some(project.id);
    store.create_chat(&project_chat).await.unwrap();
    let project_request = begin_request(
        &project_chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        1,
    );
    let project_change = begun(
        store
            .begin_root_attachment_change(&project_request)
            .await
            .unwrap(),
    );
    assert_eq!(
        project_change.subject_kind,
        RootAttachmentSubjectKind::Project
    );
    assert_eq!(project_change.subject_id, *project.id.as_uuid());
}

#[tokio::test]
async fn contradictory_broker_success_leaves_the_change_awaiting() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let executor = uuid::Uuid::new_v4();
    let request = begin_request(
        &chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        0,
    );
    let pending = begun(store.begin_root_attachment_change(&request).await.unwrap());

    assert_eq!(
        store
            .finish_root_attachment_change(request.id, executor, &completed(false, true), at(1),)
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::BrokerStateMismatch
    );
    assert_eq!(
        store.get_root_attachment_change(request.id).await.unwrap(),
        Some(pending)
    );
    assert_eq!(
        store
            .list_pending_root_attachment_changes(executor, 1)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn contradictory_failed_broker_observation_leaves_the_change_awaiting() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let executor = uuid::Uuid::new_v4();
    let request = begin_request(
        &chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        0,
    );
    let pending = begun(store.begin_root_attachment_change(&request).await.unwrap());
    let contradictory_failure = RootAttachmentChangeTerminal::Failed {
        broker_changed: Some(true),
        broker_currently_attached: Some(true),
        failure: RootAttachmentChangeFailure {
            code: "ambiguous_attach".into(),
            message: "broker reports the root remains attached".into(),
            retryable: true,
        },
    };

    assert_eq!(
        store
            .finish_root_attachment_change(request.id, executor, &contradictory_failure, at(1),)
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::BrokerStateMismatch
    );
    assert_eq!(
        store.get_root_attachment_change(request.id).await.unwrap(),
        Some(pending)
    );
}

#[tokio::test]
async fn pending_scan_is_bounded_filtered_and_oldest_first() {
    let (_dir, store) = temp_store().await;
    let executor = uuid::Uuid::new_v4();
    let other_executor = uuid::Uuid::new_v4();
    let mut expected = Vec::new();

    for second in [30, 10, 20] {
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();
        let request = begin_request(
            &chat,
            executor,
            root_id(),
            RootAttachmentChangeAction::Attach,
            0,
            second,
        );
        expected.push(begun(
            store.begin_root_attachment_change(&request).await.unwrap(),
        ));
    }
    let other_chat = sample_chat();
    store.create_chat(&other_chat).await.unwrap();
    let other = begin_request(
        &other_chat,
        other_executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        0,
    );
    begun(store.begin_root_attachment_change(&other).await.unwrap());

    expected.sort_by_key(|change| change.created_at);
    assert_eq!(
        store
            .list_pending_root_attachment_changes(executor, 2)
            .await
            .unwrap(),
        expected[..2]
    );
    assert!(store
        .list_pending_root_attachment_changes(executor, 0)
        .await
        .is_err());
    assert!(store
        .list_pending_root_attachment_changes(executor, MAX_PENDING_ROOT_ATTACHMENT_CHANGES + 1)
        .await
        .is_err());
    assert!(store
        .list_pending_root_attachment_changes(uuid::Uuid::nil(), 1)
        .await
        .is_err());
}

#[tokio::test]
async fn capacity_and_revision_headroom_are_reserved_before_begin() {
    let (_dir, store) = temp_store().await;
    let executor = uuid::Uuid::new_v4();

    let mut full = sample_chat();
    full.attachment_revision = 1;
    full.root_attachments = (0..MAX_ROOT_ATTACHMENTS)
        .map(|_| ChatRootAttachment {
            root_id: root_id(),
            origin: RootAttachmentOrigin::Conversation,
        })
        .collect();
    store.create_chat(&full).await.unwrap();
    let over_capacity = begin_request(
        &full,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        1,
        0,
    );
    assert_eq!(
        store
            .begin_root_attachment_change(&over_capacity)
            .await
            .unwrap(),
        BeginRootAttachmentChangeOutcome::CapacityExceeded
    );
    assert_eq!(store.get_chat(full.id).await.unwrap(), Some(full));

    let mut no_rollback_headroom = sample_chat();
    no_rollback_headroom.attachment_revision = MAX_ATTACHMENT_REVISION - 1;
    store.create_chat(&no_rollback_headroom).await.unwrap();
    let attach = begin_request(
        &no_rollback_headroom,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        MAX_ATTACHMENT_REVISION - 1,
        1,
    );
    assert_eq!(
        store.begin_root_attachment_change(&attach).await.unwrap(),
        BeginRootAttachmentChangeOutcome::RevisionExhausted
    );

    let existing_root = root_id();
    let mut no_detach_headroom = sample_chat();
    no_detach_headroom.attachment_revision = MAX_ATTACHMENT_REVISION;
    no_detach_headroom.root_attachments = vec![ChatRootAttachment {
        root_id: existing_root,
        origin: RootAttachmentOrigin::Conversation,
    }];
    store.create_chat(&no_detach_headroom).await.unwrap();
    let detach = begin_request(
        &no_detach_headroom,
        executor,
        existing_root,
        RootAttachmentChangeAction::Detach,
        MAX_ATTACHMENT_REVISION,
        2,
    );
    assert_eq!(
        store.begin_root_attachment_change(&detach).await.unwrap(),
        BeginRootAttachmentChangeOutcome::RevisionExhausted
    );
}

#[tokio::test]
async fn awaiting_and_terminal_changes_survive_file_backed_restart() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("restart.db").display()
    );
    let executor = uuid::Uuid::new_v4();
    let (pending_request, terminal_request, terminal_change) = {
        let store = DbStore::connect(&url).await.unwrap();
        let pending_chat = sample_chat();
        store.create_chat(&pending_chat).await.unwrap();
        let pending_request = begin_request(
            &pending_chat,
            executor,
            root_id(),
            RootAttachmentChangeAction::Attach,
            0,
            0,
        );
        begun(
            store
                .begin_root_attachment_change(&pending_request)
                .await
                .unwrap(),
        );

        let terminal_chat = sample_chat();
        store.create_chat(&terminal_chat).await.unwrap();
        let terminal_request = begin_request(
            &terminal_chat,
            executor,
            root_id(),
            RootAttachmentChangeAction::Attach,
            0,
            1,
        );
        begun(
            store
                .begin_root_attachment_change(&terminal_request)
                .await
                .unwrap(),
        );
        let terminal_change = finished(
            store
                .finish_root_attachment_change(
                    terminal_request.id,
                    executor,
                    &completed(true, true),
                    at(2),
                )
                .await
                .unwrap(),
        );
        (pending_request, terminal_request, terminal_change)
    };

    let reopened = DbStore::connect(&url).await.unwrap();
    assert_eq!(
        reopened
            .get_root_attachment_change(terminal_request.id)
            .await
            .unwrap(),
        Some(terminal_change)
    );
    assert_eq!(
        reopened
            .list_pending_root_attachment_changes(executor, 10)
            .await
            .unwrap()
            .iter()
            .map(|change| change.id)
            .collect::<Vec<_>>(),
        vec![pending_request.id]
    );
}

#[tokio::test]
async fn persisted_subject_must_match_the_authoritative_chat() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let executor = uuid::Uuid::new_v4();
    let request = begin_request(
        &chat,
        executor,
        root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        0,
    );
    begun(store.begin_root_attachment_change(&request).await.unwrap());

    let updated = entities::root_attachment_change::Entity::update_many()
        .col_expr(
            entities::root_attachment_change::Column::SubjectKind,
            sea_orm::sea_query::Expr::value("project"),
        )
        .col_expr(
            entities::root_attachment_change::Column::SubjectId,
            sea_orm::sea_query::Expr::value(uuid::Uuid::new_v4()),
        )
        .filter(entities::root_attachment_change::Column::Id.eq(*request.id.as_uuid()))
        .exec(&store.conn)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected, 1);

    assert!(store.get_root_attachment_change(request.id).await.is_err());
    assert!(store
        .list_pending_root_attachment_changes(executor, 10)
        .await
        .is_err());
    assert!(store
        .finish_root_attachment_change(request.id, executor, &completed(true, true), at(1))
        .await
        .is_err());
}
