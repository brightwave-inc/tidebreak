use super::*;

#[tokio::test]
async fn set_chat_model_updates_then_clears() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().model, None);

    store
        .set_chat_model(chat.id, Some("claude-x".into()))
        .await
        .unwrap();
    assert_eq!(
        store
            .get_chat(chat.id)
            .await
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("claude-x")
    );

    store.set_chat_model(chat.id, None).await.unwrap();
    assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().model, None);
}

#[tokio::test]
async fn chats_stored_before_the_effort_scale_widened_still_load() {
    let (_dir, store) = temp_store().await;
    // Written the way a release before `none`/`xhigh`/`max` existed wrote them:
    // straight into the column, with no chance to migrate the token.
    for (stored, expected) in [
        ("low", Some(ReasoningEffort::Low)),
        ("medium", Some(ReasoningEffort::Medium)),
        ("high", Some(ReasoningEffort::High)),
        // A token this build does not recognize is dropped, not fatal — the
        // chat still opens on the provider default.
        ("aggressive", None),
    ] {
        let chat = sample_chat();
        entities::chat::ActiveModel {
            id: Set(chat.id.0),
            project_id: Set(None),
            title: Set(chat.title.clone()),
            model: Set(None),
            reasoning_effort: Set(Some(stored.to_owned())),
            permission_mode: Set(None),
            network_policy: Set(r#"{"mode":"off"}"#.into()),
            attachment_revision: Set(0),
            created_at: Set(chat.created_at),
            owner: sea_orm::ActiveValue::NotSet,
            engine_private: Set(false),
        }
        .insert(&store.conn)
        .await
        .unwrap();
        assert_eq!(
            store
                .get_chat(chat.id)
                .await
                .unwrap()
                .unwrap()
                .reasoning_effort,
            expected,
            "stored effort {stored} no longer loads"
        );
    }

    // Every level this build can write reads back as itself.
    for effort in ReasoningEffort::ALL {
        let mut chat = sample_chat();
        chat.reasoning_effort = Some(*effort);
        store.create_chat(&chat).await.unwrap();
        assert_eq!(
            store
                .get_chat(chat.id)
                .await
                .unwrap()
                .unwrap()
                .reasoning_effort,
            Some(*effort)
        );
    }
}

#[tokio::test]
async fn chats_and_messages_roundtrip() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    assert_eq!(store.get_chat(chat.id).await.unwrap().as_ref(), Some(&chat));
    assert_eq!(store.list_chats().await.unwrap(), vec![chat.clone()]);
    assert_eq!(store.get_chat(ChatId::new()).await.unwrap(), None);

    let msg = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        reasoning: Default::default(),
        content: "hi there".into(),
        llm_content: None,
        created_at: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
    };
    store.append_message(&msg).await.unwrap();
    assert_eq!(store.list_messages(chat.id).await.unwrap(), vec![msg]);
}

#[tokio::test]
async fn list_chats_is_newest_first_and_messages_follow_commit_sequence() {
    let (_dir, store) = temp_store().await;
    let mut older = sample_chat();
    older.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let mut newer = sample_chat();
    newer.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
    store.create_chat(&older).await.unwrap();
    store.create_chat(&newer).await.unwrap();
    // list_chats is newest-first.
    assert_eq!(
        store.list_chats().await.unwrap(),
        vec![newer.clone(), older.clone()]
    );

    // Transcript order follows the durable per-chat commit sequence, even when
    // caller-provided timestamps move backwards.
    let msg = |ts: i64| Message {
        id: MessageId::new(),
        chat_id: newer.id,
        turn_id: TurnId::new(),
        role: Role::User,
        reasoning: Default::default(),
        content: format!("t{ts}"),
        llm_content: None,
        created_at: DateTime::<Utc>::from_timestamp(ts, 0).unwrap(),
    };
    let (m1, m2) = (msg(20), msg(10));
    store.append_message(&m1).await.unwrap();
    store.append_message(&m2).await.unwrap();
    let listed = store.list_messages(newer.id).await.unwrap();
    assert_eq!(listed, vec![m1, m2]);
}

#[tokio::test]
async fn delete_chat_erases_quiesced_history_and_fails_closed_for_live_work_or_roots() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let message = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        reasoning: Default::default(),
        content: "delete this history".into(),
        llm_content: None,
        created_at: Utc::now(),
    };
    store.append_message(&message).await.unwrap();
    store
        .append_event(
            chat.id,
            &AgentEvent::TextDelta {
                text: "live".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert_eq!(store.get_chat(chat.id).await.unwrap(), None);
    assert!(store.list_messages(chat.id).await.unwrap().is_empty());
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::NotFound
    );

    let active = sample_chat();
    store.create_chat(&active).await.unwrap();
    let active_turn_id = TurnId::new();
    store
        .accept_turn(active_turn_id, active.id, "test", "still working")
        .await
        .unwrap();
    assert_eq!(
        store.delete_chat(active.id).await.unwrap(),
        DeleteChatOutcome::ActiveWork
    );
    assert!(store.get_chat(active.id).await.unwrap().is_some());
    let active_turn = store.get_turn_run(active_turn_id).await.unwrap().unwrap();
    store
        .request_turn_cancellation_and_append_event(
            active_turn_id,
            active_turn.updated_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    // A recorded task plan restricts against the chat, its turn, and the call
    // that wrote it, so deletion has to erase it explicitly. Before it did,
    // any chat that ever made a plan became undeletable.
    let planning_call = ToolCallRecord {
        id: CallId::new(),
        chat_id: active.id,
        turn_id: active_turn_id,
        provider_id: "tu_plan".into(),
        name: crate::UPDATE_TASK_PLAN_TOOL.into(),
        arguments: serde_json::json!({"steps": []}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&planning_call).await.unwrap();
    store
        .update_task_plan(
            active.id,
            planning_call.id,
            &[crate::TaskPlanStep {
                content: "ship it".into(),
                status: crate::TaskPlanStepStatus::InProgress,
            }],
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("an unclaimed turn has no lease to fence against");
    assert!(matches!(
        store.delete_chat(active.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert_eq!(store.get_task_plan(active.id).await.unwrap(), None);

    // A plan request restricts against the chat, its turn, the proposing call,
    // and the journal row holding its renderer hint. Before deletion erased it,
    // any chat that ever left plan mode was undeletable.
    let planned = Chat {
        permission_mode: Some(crate::PermissionMode::Plan),
        ..sample_chat()
    };
    store.create_chat(&planned).await.unwrap();
    let (planned_turn_id, plan_call, parked_at) = park_test_plan(&store, planned.id).await;
    let decided_at = parked_at + chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .decide_plan(
                &crate::DecidePlanRequest {
                    chat_id: planned.id,
                    call_id: plan_call.id,
                    decision: crate::PlanDecision {
                        decision: crate::PlanDecisionChoice::Accept,
                        feedback: None,
                        permission_mode: None,
                    },
                },
                decided_at,
            )
            .await
            .unwrap(),
        crate::storage::DecidePlanOutcome::Decided { .. }
    ));
    let planned_turn = store.get_turn_run(planned_turn_id).await.unwrap().unwrap();
    store
        .request_turn_cancellation_and_append_event(
            planned_turn_id,
            planned_turn.updated_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    assert!(matches!(
        store.delete_chat(planned.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert!(store
        .list_pending_plan_approvals(planned.id)
        .await
        .unwrap()
        .is_empty());

    let root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut rooted = sample_chat();
    rooted.attachment_revision = 1;
    rooted.root_attachments = vec![ChatRootAttachment {
        root_id: root,
        origin: RootAttachmentOrigin::Conversation,
    }];
    store.create_chat(&rooted).await.unwrap();
    assert_eq!(
        store.delete_chat(rooted.id).await.unwrap(),
        DeleteChatOutcome::RootsAttached
    );
    assert!(store.get_chat(rooted.id).await.unwrap().is_some());

    // The store still refuses an unknown broker observation, even though the
    // desktop no longer records one: a rejected mutation now settles on the
    // state the broker reports. This keeps the gate honest for a row written by
    // an older build, which nothing will ever re-drive.
    let ambiguous = sample_chat();
    store.create_chat(&ambiguous).await.unwrap();
    let change = BeginRootAttachmentChange {
        id: RootAttachmentChangeId::new(),
        chat_id: ambiguous.id,
        executor_id: uuid::Uuid::new_v4(),
        root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        action: RootAttachmentChangeAction::Attach,
        expected_attachment_revision: 0,
        created_at: Utc::now(),
    };
    assert!(matches!(
        store.begin_root_attachment_change(&change).await.unwrap(),
        BeginRootAttachmentChangeOutcome::Begun(_)
    ));
    assert!(matches!(
        store
            .finish_root_attachment_change(
                change.id,
                change.executor_id,
                &RootAttachmentChangeTerminal::Failed {
                    broker_changed: None,
                    broker_currently_attached: None,
                    failure: RootAttachmentChangeFailure {
                        code: "broker_unavailable".into(),
                        message: "could not verify the folder attachment".into(),
                        retryable: true,
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::Finished(_)
    ));
    assert_eq!(
        store.delete_chat(ambiguous.id).await.unwrap(),
        DeleteChatOutcome::RootAttachmentStateUnresolved
    );
    assert!(store.get_chat(ambiguous.id).await.unwrap().is_some());
}

#[tokio::test]
async fn delete_chat_atomically_retires_only_its_owned_sources() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let mut owned = sample_document(None);
    owned.chat_id = Some(chat.id);
    owned.source_blob = Some(DocumentBlob::from_bytes(b"owned source bytes"));
    let owned_id = owned.id;
    let blob_id = owned.source_blob.as_ref().unwrap().id;
    store.create_document(&owned).await.unwrap();

    let legacy = sample_document(None);
    let legacy_id = legacy.id;
    store.create_document(&legacy).await.unwrap();

    assert_eq!(
        store
            .list_document_ids(DocumentScope::Chat(chat.id))
            .await
            .unwrap(),
        vec![owned_id]
    );
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert_eq!(store.get_chat(chat.id).await.unwrap(), None);
    assert_eq!(store.get_document(owned_id).await.unwrap(), None);
    assert_eq!(
        store
            .get_blob_retirement(blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );
    assert!(store.get_document(legacy_id).await.unwrap().is_some());
}

/// The #853 scoping contract: two principals' root aggregates are disjoint
/// under the owner-scoped surface — reads, lists, mutations, and creation
/// against another owner's parent all behave as if the other owner's rows do
/// not exist — while the unscoped surface still sees everything and keeps
/// attributing new rows to the local owner.
#[tokio::test]
async fn owner_scoped_queries_partition_root_aggregates() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("user:alice").unwrap();
    let bob = OwnerId::new("user:bob").unwrap();
    let local = OwnerId::local();

    let project = sample_project();
    store.create_project_scoped(&alice, &project).await.unwrap();
    let mut chat = sample_chat();
    chat.project_id = Some(project.id);
    store.create_chat_scoped(&alice, &chat).await.unwrap();

    // Chats: partitioned reads, lists, and mutations.
    assert_eq!(
        store
            .get_chat_scoped(&alice, chat.id)
            .await
            .unwrap()
            .as_ref(),
        Some(&chat)
    );
    assert_eq!(store.get_chat_scoped(&bob, chat.id).await.unwrap(), None);
    assert_eq!(
        store.list_chats_scoped(&alice).await.unwrap(),
        vec![chat.clone()]
    );
    assert_eq!(store.list_chats_scoped(&bob).await.unwrap(), Vec::new());
    assert_eq!(
        store.delete_chat_scoped(&bob, chat.id).await.unwrap(),
        DeleteChatOutcome::NotFound
    );
    assert!(!store
        .update_chat_metadata_scoped(
            &bob,
            chat.id,
            Some(Some("stolen".into())),
            None,
            None,
            None,
            None
        )
        .await
        .unwrap());
    assert!(store
        .get_chat_transcript_scoped(&bob, chat.id)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .get_chat_transcript_scoped(&alice, chat.id)
        .await
        .unwrap()
        .is_some());
    // The failed cross-owner mutation left the row untouched.
    assert_eq!(
        store
            .get_chat_scoped(&alice, chat.id)
            .await
            .unwrap()
            .as_ref(),
        Some(&chat)
    );

    // Projects: partitioned, and unusable as another owner's chat parent.
    assert_eq!(
        store.get_project_scoped(&bob, project.id).await.unwrap(),
        None
    );
    assert_eq!(store.list_projects_scoped(&bob).await.unwrap(), Vec::new());
    assert!(!store
        .update_project_title_scoped(&bob, project.id, Some("stolen".into()))
        .await
        .unwrap());
    assert_eq!(
        store.delete_project_scoped(&bob, project.id).await.unwrap(),
        DeleteProjectOutcome::NotFound
    );
    let mut cross_owner_chat = sample_chat();
    cross_owner_chat.project_id = Some(project.id);
    assert!(store
        .create_chat_scoped(&bob, &cross_owner_chat)
        .await
        .is_err());
    assert!(store
        .create_chat_with_project_defaults_scoped(&bob, &cross_owner_chat)
        .await
        .is_err());

    // Documents: inherit the parent's owner and partition with it.
    let source = DocumentSourceUpsert {
        id: DocumentId::new(),
        chat_id: Some(chat.id),
        project_id: None,
        origin_uri: None,
        media_type: "text/plain".into(),
        title: None,
        source_blob: DocumentBlob::from_bytes(b"alice's notes"),
        canonical_text: "alice's notes".into(),
        updated_at: Utc::now(),
    };
    let document = store
        .accept_document_source_scoped(&alice, &source)
        .await
        .unwrap();
    assert!(store
        .get_document_scoped(&alice, document.id)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        store.get_document_scoped(&bob, document.id).await.unwrap(),
        None
    );
    assert_eq!(
        store
            .list_document_summaries_scoped(&bob, DocumentScope::Chat(chat.id), None, 10)
            .await
            .unwrap(),
        Vec::new()
    );
    assert_eq!(
        store
            .list_document_summaries_scoped(&alice, DocumentScope::Chat(chat.id), None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    // A cross-owner accept against alice's chat is indistinguishable from a
    // missing parent; a cross-owner delete leaves the row in place.
    assert!(store
        .accept_document_source_scoped(&bob, &source)
        .await
        .is_err());
    store
        .delete_document_scoped(&bob, document.id)
        .await
        .unwrap();
    assert!(store
        .get_document_scoped(&alice, document.id)
        .await
        .unwrap()
        .is_some());

    // The unscoped surface still sees everything, and unscoped creation
    // attributes to the local owner.
    assert_eq!(store.list_chats().await.unwrap().len(), 1);
    let loose = sample_chat();
    store.create_chat(&loose).await.unwrap();
    assert_eq!(
        store
            .get_chat_scoped(&local, loose.id)
            .await
            .unwrap()
            .as_ref(),
        Some(&loose)
    );
    assert_eq!(store.get_chat_scoped(&alice, loose.id).await.unwrap(), None);
}
