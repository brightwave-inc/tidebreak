use super::*;

async fn output_writeback_fixture() -> (tempfile::TempDir, Arc<dyn Store>, Chat, TurnId, uuid::Uuid)
{
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("writeback.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "publish the report")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    (db, store, chat, turn_id, lease_token)
}

async fn create_named_output(
    store: &Arc<dyn Store>,
    chat_id: ChatId,
    filename: &str,
    created_at: chrono::DateTime<Utc>,
) -> crate::OutputId {
    let id = crate::OutputId::new();
    store
        .create_output(&crate::CreateOutput {
            id,
            chat_id,
            filename: filename.to_owned(),
            kind: crate::DeliverableKind::Text,
            revision: crate::NewOutputRevision {
                id: crate::OutputRevisionId::new(),
                byte_len: 5,
                sha256: [7; 32],
                turn_id: None,
                producing_run_id: None,
                created_at,
            },
        })
        .await
        .unwrap();
    id
}

fn output_writeback_agent(
    store: Arc<dyn Store>,
    arguments: String,
    lease_token: uuid::Uuid,
) -> Agent {
    let mut registry = ToolRegistry::new();
    registry.register_validated_client(
        crate::write_output_to_connected_folder_tool_spec(),
        ApprovalClass::Workspace,
        crate::validate_write_output_to_connected_folder_arguments,
    );
    Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
            arguments: Box::leak(arguments.into_boxed_str()),
        }),
        Arc::new(registry),
        store,
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
}

/// The model names an output by filename; the checkpoint carries the
/// resolved opaque id of the newest live output with that name — the same
/// record the output scan would version — so everything downstream keeps
/// working from a stable identity the model never saw.
#[tokio::test]
async fn output_writeback_filename_resolves_to_the_newest_live_output() {
    let (_db, store, chat, turn_id, lease_token) = output_writeback_fixture().await;
    // A retracted `report.md` frees the name for a later one. Only one live
    // output can hold it, and that is the one the checkpoint must resolve.
    let older = Utc::now() - chrono::Duration::minutes(10);
    let retracted = create_named_output(&store, chat.id, "report.md", older).await;
    store.delete_output(retracted, older).await.unwrap();
    let newest = create_named_output(&store, chat.id, "report.md", Utc::now()).await;

    let root_id = uuid::Uuid::new_v4();
    let agent = output_writeback_agent(
        store.clone(),
        format!(
            r#"{{"filename":"report.md","root_id":"{root_id}","path":"reports/report.md","mode":"create"}}"#
        ),
        lease_token,
    );
    let (tx, _rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    let AgentTurnOutcome::ClientToolCall { request, .. } = outcome else {
        panic!("a resolvable filename must reach a client checkpoint: {outcome:?}");
    };
    assert_eq!(request.name, crate::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL);
    assert_eq!(
        request.arguments,
        serde_json::json!({
            "output_id": newest.as_uuid(),
            "root_id": root_id,
            "path": "reports/report.md",
            "mode": "create"
        })
    );
}

/// A filename with no live output — never published, or deleted — is
/// answered in place with an error naming the file, instead of parking a
/// checkpoint no executor could satisfy.
#[tokio::test]
async fn output_writeback_without_a_live_match_is_refused_naming_the_filename() {
    let (_db, store, chat, turn_id, lease_token) = output_writeback_fixture().await;
    let deleted = create_named_output(&store, chat.id, "report.md", Utc::now()).await;
    store.delete_output(deleted, Utc::now()).await.unwrap();

    let agent = output_writeback_agent(
        store.clone(),
        format!(
            r#"{{"filename":"report.md","root_id":"{}","path":"report.md","mode":"create"}}"#,
            uuid::Uuid::new_v4()
        ),
        lease_token,
    );
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    assert!(
        !matches!(outcome, AgentTurnOutcome::ClientToolCall { .. }),
        "an unresolvable filename must not reach a checkpoint: {outcome:?}"
    );
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    let events = emitted_events(rx.collect().await);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.is_error && output.content.contains("report.md")
        )),
        "the refusal must name the filename"
    );
}
