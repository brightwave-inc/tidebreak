use super::*;

fn mem_root_id() -> crate::id::HostRootId {
    crate::id::HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap()
}

fn mem_attachment_chat(
    project_id: Option<ProjectId>,
    roots: Vec<ChatRootAttachment>,
    attachment_revision: i64,
) -> Chat {
    Chat {
        id: ChatId::new(),
        project_id,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        citation_format: None,
        attachment_revision,
        root_attachments: roots,
        created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(10, 0).unwrap(),
    }
}

fn mem_attachment_request(
    chat: &Chat,
    executor_id: uuid::Uuid,
    root_id: crate::id::HostRootId,
    action: RootAttachmentChangeAction,
    expected_attachment_revision: i64,
    created_at: chrono::DateTime<chrono::Utc>,
) -> BeginRootAttachmentChange {
    BeginRootAttachmentChange {
        id: RootAttachmentChangeId::new(),
        chat_id: chat.id,
        executor_id,
        root_id,
        action,
        expected_attachment_revision,
        created_at,
    }
}

#[test]
fn mem_root_attachment_begin_derives_authority_and_enforces_atomic_guards() {
    let store = MemStore::default();
    let executor_id = uuid::Uuid::new_v4();
    let project_root = mem_root_id();
    let project = Project {
        id: ProjectId::new(),
        title: None,
        attachment_revision: 1,
        root_attachments: vec![project_root],
        created_at: chrono::Utc::now(),
    };
    block_on(store.create_project(&project)).unwrap();
    let base = mem_attachment_chat(Some(project.id), Vec::new(), 0);
    let chat = block_on(store.create_chat_with_project_defaults(&base)).unwrap();
    let root_id = mem_root_id();
    let now = chrono::DateTime::<chrono::Utc>::from_timestamp(20, 0).unwrap();
    let request = mem_attachment_request(
        &chat,
        executor_id,
        root_id,
        RootAttachmentChangeAction::Attach,
        1,
        now,
    );

    let begun = match block_on(store.begin_root_attachment_change(&request)).unwrap() {
        BeginRootAttachmentChangeOutcome::Begun(change) => change,
        outcome => panic!("unexpected begin outcome: {outcome:?}"),
    };
    assert_eq!(begun.subject_kind, RootAttachmentSubjectKind::Project);
    assert_eq!(begun.subject_id, *project.id.as_uuid());
    assert_eq!(begun.before_revision, 1);
    assert_eq!(begun.intent_revision, 2);
    assert_eq!(begun.origin, Some(RootAttachmentOrigin::Conversation));
    assert_eq!(begun.projection_position, Some(1));
    assert!(!begun.projection_existed_before);
    assert_eq!(
        block_on(store.get_chat(chat.id))
            .unwrap()
            .unwrap()
            .root_attachments,
        vec![
            ChatRootAttachment {
                root_id: project_root,
                origin: RootAttachmentOrigin::ProjectDefault,
            },
            ChatRootAttachment {
                root_id,
                origin: RootAttachmentOrigin::Conversation,
            },
        ]
    );
    assert_eq!(
        block_on(store.begin_root_attachment_change(&request)).unwrap(),
        BeginRootAttachmentChangeOutcome::Existing(begun.clone())
    );
    assert_eq!(
        block_on(
            store.begin_root_attachment_change(&BeginRootAttachmentChange {
                created_at: request.created_at + chrono::Duration::nanoseconds(999),
                ..request
            })
        )
        .unwrap(),
        BeginRootAttachmentChangeOutcome::Existing(begun.clone())
    );
    assert_eq!(
        block_on(
            store.begin_root_attachment_change(&BeginRootAttachmentChange {
                root_id: mem_root_id(),
                ..request
            })
        )
        .unwrap(),
        BeginRootAttachmentChangeOutcome::IdentityConflict
    );

    let busy = mem_attachment_request(
        &chat,
        executor_id,
        mem_root_id(),
        RootAttachmentChangeAction::Attach,
        2,
        now + chrono::Duration::seconds(1),
    );
    assert_eq!(
        block_on(store.begin_root_attachment_change(&busy)).unwrap(),
        BeginRootAttachmentChangeOutcome::ChatBusy
    );

    let standalone = mem_attachment_chat(None, Vec::new(), 4);
    block_on(store.create_chat(&standalone)).unwrap();
    let stale = mem_attachment_request(
        &standalone,
        executor_id,
        mem_root_id(),
        RootAttachmentChangeAction::Attach,
        3,
        now,
    );
    assert_eq!(
        block_on(store.begin_root_attachment_change(&stale)).unwrap(),
        BeginRootAttachmentChangeOutcome::RevisionConflict {
            current_attachment_revision: 4
        }
    );

    let full_roots: Vec<_> = (0..MAX_ROOT_ATTACHMENTS)
        .map(|_| ChatRootAttachment {
            root_id: mem_root_id(),
            origin: RootAttachmentOrigin::Conversation,
        })
        .collect();
    let full = mem_attachment_chat(None, full_roots, 1);
    block_on(store.create_chat(&full)).unwrap();
    let at_capacity = mem_attachment_request(
        &full,
        executor_id,
        mem_root_id(),
        RootAttachmentChangeAction::Attach,
        1,
        now,
    );
    assert_eq!(
        block_on(store.begin_root_attachment_change(&at_capacity)).unwrap(),
        BeginRootAttachmentChangeOutcome::CapacityExceeded
    );

    let exhausted = mem_attachment_chat(None, Vec::new(), MAX_ATTACHMENT_REVISION - 1);
    block_on(store.create_chat(&exhausted)).unwrap();
    let cannot_reserve_rollback = mem_attachment_request(
        &exhausted,
        executor_id,
        mem_root_id(),
        RootAttachmentChangeAction::Attach,
        MAX_ATTACHMENT_REVISION - 1,
        now,
    );
    assert_eq!(
        block_on(store.begin_root_attachment_change(&cannot_reserve_rollback)).unwrap(),
        BeginRootAttachmentChangeOutcome::RevisionExhausted
    );

    let existing_root = mem_root_id();
    let detach_exhausted = mem_attachment_chat(
        None,
        vec![ChatRootAttachment {
            root_id: existing_root,
            origin: RootAttachmentOrigin::Conversation,
        }],
        MAX_ATTACHMENT_REVISION,
    );
    block_on(store.create_chat(&detach_exhausted)).unwrap();
    let cannot_reserve_removal = mem_attachment_request(
        &detach_exhausted,
        executor_id,
        existing_root,
        RootAttachmentChangeAction::Detach,
        MAX_ATTACHMENT_REVISION,
        now,
    );
    assert_eq!(
        block_on(store.begin_root_attachment_change(&cannot_reserve_removal)).unwrap(),
        BeginRootAttachmentChangeOutcome::RevisionExhausted
    );

    let invalid_project_chat =
        mem_attachment_chat(Some(ProjectId(uuid::Uuid::nil())), Vec::new(), 0);
    store
        .chats
        .lock()
        .unwrap()
        .insert(invalid_project_chat.id, invalid_project_chat.clone());
    let invalid_request = mem_attachment_request(
        &invalid_project_chat,
        executor_id,
        mem_root_id(),
        RootAttachmentChangeAction::Attach,
        0,
        now,
    );
    assert!(block_on(store.begin_root_attachment_change(&invalid_request)).is_err());
    assert_eq!(
        block_on(store.get_chat(invalid_project_chat.id)).unwrap(),
        Some(invalid_project_chat)
    );
    assert!(
        block_on(store.get_root_attachment_change(invalid_request.id))
            .unwrap()
            .is_none()
    );
}

#[test]
fn mem_root_attachment_attach_projects_intent_and_rolls_back_failure() {
    let store = MemStore::default();
    let executor_id = uuid::Uuid::new_v4();
    let chat = mem_attachment_chat(None, Vec::new(), 0);
    block_on(store.create_chat(&chat)).unwrap();
    let root_id = mem_root_id();
    let now = chrono::DateTime::<chrono::Utc>::from_timestamp(30, 0).unwrap();
    let request = mem_attachment_request(
        &chat,
        executor_id,
        root_id,
        RootAttachmentChangeAction::Attach,
        0,
        now,
    );
    let awaiting = match block_on(store.begin_root_attachment_change(&request)).unwrap() {
        BeginRootAttachmentChangeOutcome::Begun(change) => change,
        outcome => panic!("unexpected begin outcome: {outcome:?}"),
    };
    assert_eq!(
        awaiting.subject_kind,
        RootAttachmentSubjectKind::Conversation
    );
    assert_eq!(awaiting.subject_id, *chat.id.as_uuid());
    let intent_chat = block_on(store.get_chat(chat.id)).unwrap().unwrap();
    assert_eq!(intent_chat.attachment_revision, 1);
    assert_eq!(intent_chat.root_attachments.len(), 1);

    let failure = RootAttachmentChangeTerminal::Failed {
        broker_changed: Some(false),
        broker_currently_attached: Some(false),
        failure: RootAttachmentChangeFailure {
            code: "denied".into(),
            message: "host access was denied".into(),
            retryable: false,
        },
    };
    let finished_at = now - chrono::Duration::seconds(1);
    assert_eq!(
        block_on(store.finish_root_attachment_change(
            request.id,
            executor_id,
            &RootAttachmentChangeTerminal::Failed {
                broker_changed: Some(true),
                broker_currently_attached: Some(true),
                failure: RootAttachmentChangeFailure {
                    code: "ambiguous".into(),
                    message: "broker reports the requested state".into(),
                    retryable: true,
                },
            },
            finished_at,
        ))
        .unwrap(),
        FinishRootAttachmentChangeOutcome::BrokerStateMismatch
    );
    assert_eq!(
        block_on(store.get_root_attachment_change(request.id)).unwrap(),
        Some(awaiting)
    );
    let failed = match block_on(store.finish_root_attachment_change(
        request.id,
        executor_id,
        &failure,
        finished_at,
    ))
    .unwrap()
    {
        FinishRootAttachmentChangeOutcome::Finished(change) => change,
        outcome => panic!("unexpected finish outcome: {outcome:?}"),
    };
    assert_eq!(failed.phase, RootAttachmentChangePhase::Failed);
    assert_eq!(failed.finished_at, Some(now));
    assert_eq!(failed.result_revision, Some(2));
    assert_eq!(failed.projection_changed, Some(false));
    let rolled_back = block_on(store.get_chat(chat.id)).unwrap().unwrap();
    assert_eq!(rolled_back.attachment_revision, 2);
    assert!(rolled_back.root_attachments.is_empty());
    assert_eq!(
        block_on(store.finish_root_attachment_change(
            request.id,
            executor_id,
            &failure,
            finished_at,
        ))
        .unwrap(),
        FinishRootAttachmentChangeOutcome::Existing(failed.clone())
    );
    assert_eq!(
        block_on(store.finish_root_attachment_change(
            request.id,
            executor_id,
            &failure,
            finished_at + chrono::Duration::seconds(1),
        ))
        .unwrap(),
        FinishRootAttachmentChangeOutcome::Existing(failed)
    );

    let success_request = mem_attachment_request(
        &rolled_back,
        executor_id,
        root_id,
        RootAttachmentChangeAction::Attach,
        2,
        now + chrono::Duration::seconds(2),
    );
    block_on(store.begin_root_attachment_change(&success_request)).unwrap();
    let success = RootAttachmentChangeTerminal::Completed {
        broker_changed: true,
        broker_currently_attached: true,
    };
    let completed = match block_on(store.finish_root_attachment_change(
        success_request.id,
        executor_id,
        &success,
        now + chrono::Duration::seconds(3),
    ))
    .unwrap()
    {
        FinishRootAttachmentChangeOutcome::Finished(change) => change,
        outcome => panic!("unexpected finish outcome: {outcome:?}"),
    };
    assert_eq!(completed.phase, RootAttachmentChangePhase::Completed);
    assert_eq!(completed.result_revision, Some(3));
    assert_eq!(completed.projection_changed, Some(true));
    assert_eq!(
        block_on(store.get_root_attachment_change(success_request.id)).unwrap(),
        Some(completed)
    );
}

#[test]
fn mem_root_attachment_detach_waits_for_broker_and_preserves_position() {
    let store = MemStore::default();
    let executor_id = uuid::Uuid::new_v4();
    let roots = [mem_root_id(), mem_root_id(), mem_root_id()];
    let chat = mem_attachment_chat(
        None,
        roots
            .into_iter()
            .map(|root_id| ChatRootAttachment {
                root_id,
                origin: RootAttachmentOrigin::Conversation,
            })
            .collect(),
        1,
    );
    block_on(store.create_chat(&chat)).unwrap();
    let now = chrono::DateTime::<chrono::Utc>::from_timestamp(40, 0).unwrap();
    let request = mem_attachment_request(
        &chat,
        executor_id,
        roots[1],
        RootAttachmentChangeAction::Detach,
        1,
        now,
    );
    let awaiting = match block_on(store.begin_root_attachment_change(&request)).unwrap() {
        BeginRootAttachmentChangeOutcome::Begun(change) => change,
        outcome => panic!("unexpected begin outcome: {outcome:?}"),
    };
    assert_eq!(awaiting.intent_revision, 1);
    assert_eq!(awaiting.projection_position, Some(1));
    assert_eq!(
        block_on(store.get_chat(chat.id))
            .unwrap()
            .unwrap()
            .root_attachments,
        chat.root_attachments
    );
    let completed = RootAttachmentChangeTerminal::Completed {
        broker_changed: true,
        broker_currently_attached: false,
    };
    assert_eq!(
        block_on(store.finish_root_attachment_change(
            request.id,
            uuid::Uuid::new_v4(),
            &completed,
            now + chrono::Duration::seconds(1),
        ))
        .unwrap(),
        FinishRootAttachmentChangeOutcome::ExecutorMismatch
    );
    assert_eq!(
        block_on(store.finish_root_attachment_change(
            request.id,
            executor_id,
            &RootAttachmentChangeTerminal::Completed {
                broker_changed: false,
                broker_currently_attached: true,
            },
            now + chrono::Duration::seconds(1),
        ))
        .unwrap(),
        FinishRootAttachmentChangeOutcome::BrokerStateMismatch
    );
    let finished = match block_on(store.finish_root_attachment_change(
        request.id,
        executor_id,
        &completed,
        now + chrono::Duration::seconds(1),
    ))
    .unwrap()
    {
        FinishRootAttachmentChangeOutcome::Finished(change) => change,
        outcome => panic!("unexpected finish outcome: {outcome:?}"),
    };
    assert_eq!(finished.result_revision, Some(2));
    assert_eq!(finished.projection_changed, Some(true));
    let detached = block_on(store.get_chat(chat.id)).unwrap().unwrap();
    assert_eq!(detached.attachment_revision, 2);
    assert_eq!(
        detached
            .root_attachments
            .iter()
            .map(|attachment| attachment.root_id)
            .collect::<Vec<_>>(),
        vec![roots[0], roots[2]]
    );

    let absent = mem_attachment_request(
        &detached,
        executor_id,
        roots[1],
        RootAttachmentChangeAction::Detach,
        2,
        now + chrono::Duration::seconds(2),
    );
    let absent_change = match block_on(store.begin_root_attachment_change(&absent)).unwrap() {
        BeginRootAttachmentChangeOutcome::Begun(change) => change,
        outcome => panic!("unexpected begin outcome: {outcome:?}"),
    };
    assert!(!absent_change.projection_existed_before);
    assert_eq!(absent_change.origin, None);
    let absent_finished = match block_on(store.finish_root_attachment_change(
        absent.id,
        executor_id,
        &RootAttachmentChangeTerminal::Completed {
            broker_changed: false,
            broker_currently_attached: false,
        },
        now + chrono::Duration::seconds(3),
    ))
    .unwrap()
    {
        FinishRootAttachmentChangeOutcome::Finished(change) => change,
        outcome => panic!("unexpected finish outcome: {outcome:?}"),
    };
    assert_eq!(absent_finished.result_revision, Some(2));
    assert_eq!(absent_finished.projection_changed, Some(false));

    let failure_chat = mem_attachment_chat(
        None,
        vec![ChatRootAttachment {
            root_id: roots[1],
            origin: RootAttachmentOrigin::Conversation,
        }],
        5,
    );
    block_on(store.create_chat(&failure_chat)).unwrap();
    let failed_detach = mem_attachment_request(
        &failure_chat,
        executor_id,
        roots[1],
        RootAttachmentChangeAction::Detach,
        5,
        now + chrono::Duration::seconds(4),
    );
    block_on(store.begin_root_attachment_change(&failed_detach)).unwrap();
    let failed = match block_on(store.finish_root_attachment_change(
        failed_detach.id,
        executor_id,
        &RootAttachmentChangeTerminal::Failed {
            broker_changed: None,
            broker_currently_attached: None,
            failure: RootAttachmentChangeFailure {
                code: "unavailable".into(),
                message: "broker was unavailable".into(),
                retryable: true,
            },
        },
        now + chrono::Duration::seconds(5),
    ))
    .unwrap()
    {
        FinishRootAttachmentChangeOutcome::Finished(change) => change,
        outcome => panic!("unexpected finish outcome: {outcome:?}"),
    };
    assert_eq!(failed.result_revision, Some(5));
    assert_eq!(failed.projection_changed, Some(false));
    assert_eq!(
        block_on(store.get_chat(failure_chat.id)).unwrap(),
        Some(failure_chat)
    );
}

#[test]
fn mem_root_attachment_pending_scan_is_bounded_filtered_and_ordered() {
    let store = MemStore::default();
    let executor_id = uuid::Uuid::new_v4();
    let other_executor_id = uuid::Uuid::new_v4();
    let time = chrono::DateTime::<chrono::Utc>::from_timestamp(50, 0).unwrap();
    let mut expected = Vec::new();
    for (id, executor, created_at) in [
        (1_u128, executor_id, time),
        (3, executor_id, time + chrono::Duration::seconds(1)),
        (2, executor_id, time),
        (4, other_executor_id, time),
    ] {
        let chat = mem_attachment_chat(None, Vec::new(), 0);
        block_on(store.create_chat(&chat)).unwrap();
        let mut request = mem_attachment_request(
            &chat,
            executor,
            mem_root_id(),
            RootAttachmentChangeAction::Detach,
            0,
            created_at,
        );
        request.id = RootAttachmentChangeId::from_uuid(uuid::Uuid::from_u128(id)).unwrap();
        let change = match block_on(store.begin_root_attachment_change(&request)).unwrap() {
            BeginRootAttachmentChangeOutcome::Begun(change) => change,
            outcome => panic!("unexpected begin outcome: {outcome:?}"),
        };
        if executor == executor_id {
            expected.push(change);
        }
    }
    expected.sort_by_key(|change| (change.created_at, *change.id.as_uuid()));

    assert_eq!(
        block_on(store.list_pending_root_attachment_changes(executor_id, 2)).unwrap(),
        expected[..2]
    );
    assert_eq!(
        block_on(store.list_pending_root_attachment_changes(executor_id, 3)).unwrap(),
        expected
    );
    assert!(block_on(store.list_pending_root_attachment_changes(executor_id, 0)).is_err());
    assert!(block_on(store.list_pending_root_attachment_changes(
        executor_id,
        MAX_PENDING_ROOT_ATTACHMENT_CHANGES + 1,
    ))
    .is_err());
    assert!(block_on(store.list_pending_root_attachment_changes(uuid::Uuid::nil(), 1)).is_err());
}
