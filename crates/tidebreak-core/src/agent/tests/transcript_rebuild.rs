use super::*;

#[test]
fn a_resumed_transcript_is_bounded_like_a_live_one() {
    // The record may now hold more than a turn can afford to re-read, so
    // rebuilding has to apply the feedback bound too — otherwise resuming
    // would feed the model something the original step never did.
    let oversized = "y".repeat(DEFAULT_MAX_TOOL_RESULT_BYTES * 2);
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: SessionId::new(),
        turn_id: TurnId::new(),
        provider_id: "call-1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Completed,
        result: Some(oversized.clone()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: Utc::now(),
        resolved_at: Some(Utc::now()),
    };
    let rebuilt = rebuild_transcript(&[], &[call], &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    let found = rebuilt.iter().find_map(|message| {
        message.content.iter().find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
    });
    let content = found.expect("the resumed transcript replays the result");
    assert!(content.len() < oversized.len());
    assert!(content.contains("[truncated:"));
}

#[test]
fn rebuild_replays_message_images_in_their_recorded_order() {
    use crate::image::ImageMediaType;

    let turn = TurnId::new();
    let chat = SessionId::new();
    let t0 = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let with_images = MessageId::new();
    let text_only = MessageId::new();
    let mut messages = vec![
        Message {
            id: with_images,
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "compare these".into(),
            llm_content: None,
            created_at: t0,
        },
        Message {
            id: text_only,
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "and this one?".into(),
            llm_content: None,
            created_at: t1,
        },
    ];
    let image = |seed: u128, media_type| ImageRef {
        blob_id: uuid::Uuid::from_u128(seed),
        media_type,
        width: 800,
        height: 600,
        byte_len: 4_096,
    };
    let first = image(1, ImageMediaType::Png);
    let second = image(2, ImageMediaType::Jpeg);
    messages[0].llm_content =
        crate::model::user_message_llm_content("compare these", &[first, second], &[], &[], false);
    // Deliberately out of row order: the ordinal decides, not arrival.
    let attachments = vec![
        MessageAttachment {
            message_id: with_images,
            chat_id: chat,
            ordinal: 1,
            image: second,
            created_at: t0,
        },
        MessageAttachment {
            message_id: with_images,
            chat_id: chat,
            ordinal: 0,
            image: first,
            created_at: t0,
        },
    ];

    let rebuilt = rebuild_transcript(&messages, &[], &attachments, DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[0].role, Role::User);
    assert_eq!(
            rebuilt[0].content,
            vec![
                ContentBlock::Image { image: first },
                ContentBlock::Image { image: second },
                ContentBlock::Text {
                    text: format!(
                        "# Important context\n\n<attachments>\n\
                         image_1: id={}; media_type=image/png; byte_size=4096; this is image content block 1\n\
                         image_2: id={}; media_type=image/jpeg; byte_size=4096; this is image content block 2\n\
                         </attachments>\n\n# User message\n\ncompare these",
                        first.blob_id, second.blob_id
                    )
                },
            ]
        );
    // A message with no attachments rebuilds exactly as it did before.
    assert_eq!(
        rebuilt[1].content,
        vec![ContentBlock::Text {
            text: "and this one?".into()
        }]
    );
    // Reloading the same rows reproduces the identical block sequence.
    assert_eq!(
        rebuild_transcript(&messages, &[], &attachments, DEFAULT_MAX_TOOL_RESULT_BYTES),
        rebuilt
    );
}

#[test]
fn rebuild_announces_file_routes_and_bounds_attachment_context() {
    let turn = TurnId::new();
    let chat = SessionId::new();
    let message_id = MessageId::new();
    let created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let mut message = Message {
        id: message_id,
        chat_id: chat,
        turn_id: turn,
        role: Role::User,
        reasoning: Default::default(),
        content: "summarize this file".into(),
        llm_content: None,
        created_at,
    };
    let text_id = crate::id::DocumentId::new();
    let text_blob = crate::model::DocumentBlob::from_bytes(b"decoded notes");
    let text = crate::model::MessageDocumentAttachment {
        message_id,
        chat_id: chat,
        ordinal: 0,
        document_id: text_id,
        title: Some("notes.txt".into()),
        media_type: "text/plain".into(),
        source_blob: Some(text_blob),
        readable: true,
        created_at,
    };
    let pdf_id = crate::id::DocumentId::new();
    let pdf_blob = crate::model::DocumentBlob::from_bytes(b"%PDF opaque");
    let pdf = crate::model::MessageDocumentAttachment {
        message_id,
        chat_id: chat,
        ordinal: 1,
        document_id: pdf_id,
        title: Some("brief.pdf".into()),
        media_type: "application/pdf".into(),
        source_blob: Some(pdf_blob),
        readable: false,
        created_at,
    };
    let mut documents = vec![text, pdf];
    let oversized_id = crate::id::DocumentId::new();
    documents.push(crate::model::MessageDocumentAttachment {
        message_id,
        chat_id: chat,
        ordinal: 2,
        document_id: oversized_id,
        title: Some("large.xlsx".into()),
        media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
        source_blob: Some(crate::model::DocumentBlob::from_digest(
            [9; 32],
            crate::model::MAX_EXEC_WORKSPACE_FILE_BYTES as u64 + 1,
        )),
        readable: false,
        created_at,
    });
    for ordinal in 3..=8 {
        documents.push(crate::model::MessageDocumentAttachment {
            message_id,
            chat_id: chat,
            ordinal,
            document_id: crate::id::DocumentId::new(),
            title: Some(format!("extra-{ordinal}.bin")),
            media_type: "application/octet-stream".into(),
            source_blob: Some(crate::model::DocumentBlob::from_bytes(
                format!("extra-{ordinal}").as_bytes(),
            )),
            readable: false,
            created_at,
        });
    }

    message.llm_content =
        crate::model::user_message_llm_content(&message.content, &[], &documents, &[], false);
    let rebuilt = rebuild_transcript_with_boundary(
        &[message],
        &[],
        &[],
        DEFAULT_MAX_TOOL_RESULT_BYTES,
        false,
        None,
    )
    .0;
    let ContentBlock::Text { text } = &rebuilt[0].content[0] else {
        panic!("file attachment should annotate the user text");
    };
    assert!(text.starts_with("# Important context\n\n<attachments>"));
    assert!(text.contains(&text_id.to_string()));
    assert!(text.contains("\"title\":\"notes.txt\""));
    let text_path = format!(
        "documents/{}",
        crate::model::exec_attachment_file_name(Some("notes.txt"), text_id)
    );
    assert!(text.contains(&format!(
        "route: readable via read_document(document_id=\"{text_id}\"); raw bytes at \
         {text_path} in the exec workspace"
    )));
    let pdf_path = format!(
        "documents/{}",
        crate::model::exec_attachment_file_name(Some("brief.pdf"), pdf_id)
    );
    assert!(text.contains(&pdf_id.to_string()));
    assert!(text.contains("\"title\":\"brief.pdf\""));
    assert!(text.contains("\"media_type\":\"application/pdf\""));
    assert!(text.contains(&format!(
        "route: raw bytes at {pdf_path} in the exec workspace; helper: python3 \
             .tidebreak/exec-scripts/render_pdf.py {pdf_path}"
    )));
    assert!(text.contains(&oversized_id.to_string()));
    assert!(text.contains(&format!(
        "route: raw bytes not materialized because the file exceeds the \
             {}-byte exec workspace limit",
        crate::model::MAX_EXEC_WORKSPACE_FILE_BYTES
    )));
    assert!(text.contains("1 more attachment(s) omitted."));
    assert!(text.ends_with("</attachments>\n\n# User message\n\nsummarize this file"));
}

#[test]
fn rebuild_attaches_tools_to_assistant_text() {
    let turn = TurnId::new();
    let chat = SessionId::new();
    let t0 = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
    let messages = vec![
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "read it".into(),
            llm_content: None,
            created_at: t0,
        },
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "looking…".into(),
            llm_content: None,
            created_at: t1,
        },
    ];
    let calls = vec![ToolCallRecord {
        id: CallId::new(),
        chat_id: chat,
        turn_id: turn,
        provider_id: "tu_1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path": "a"}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Completed,
        result: Some("ok".into()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: t2,
        resolved_at: Some(DateTime::<Utc>::from_timestamp(1_003, 0).unwrap()),
    }];
    let rebuilt = rebuild_transcript(&messages, &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 3);
    assert_eq!(rebuilt[0].role, Role::User);
    assert!(matches!(
        &rebuilt[1].content[..],
        [
            ContentBlock::Text { text },
            ContentBlock::ToolUse { id, name, .. }
        ] if text == "looking…" && id == "tu_1" && name == "read_file"
    ));
    assert!(matches!(
        &rebuilt[2].content[..],
        [ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error: false
        }] if tool_use_id == "tu_1" && content == "ok"
    ));
}

#[test]
fn orchestration_forces_a_model_step_boundary_despite_overlapping_timestamps() {
    let turn = TurnId::new();
    let chat = SessionId::new();
    let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
    let t3 = DateTime::<Utc>::from_timestamp(1_003, 0).unwrap();
    let call = |provider_id: &str,
                execution: ToolCallExecution,
                created_at: DateTime<Utc>,
                resolved_at: DateTime<Utc>| ToolCallRecord {
        id: CallId::new(),
        chat_id: chat,
        turn_id: turn,
        provider_id: provider_id.into(),
        name: if execution == ToolCallExecution::Orchestration {
            crate::SPAWN_SANDBOX_AGENT_TOOL.into()
        } else {
            "read_file".into()
        },
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution,
        status: ToolCallStatus::Completed,
        result: Some("ok".into()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: Some(resolved_at),
    };
    let calls = vec![
        call("ordinary-before", ToolCallExecution::Server, t1, t3),
        call("spawn", ToolCallExecution::Orchestration, t2, t2),
        call("ordinary-after", ToolCallExecution::Server, t2, t3),
    ];
    let batches = batch_tool_calls(&calls);
    assert_eq!(batches.len(), 3);
    assert_eq!(
        batches
            .iter()
            .map(|batch| batch
                .iter()
                .map(|call| call.provider_id.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![
            vec!["ordinary-before"],
            vec!["spawn"],
            vec!["ordinary-after"],
        ]
    );
}

#[test]
fn answered_user_questions_rebuild_as_a_model_facing_tool_result() {
    let turn = TurnId::new();
    let chat = SessionId::new();
    let created_at = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let answer = crate::AnswerUserQuestions {
        answers: vec![crate::UserQuestionAnswer {
            question_id: "target".into(),
            selected_option_ids: vec!["staging".into()],
            custom_answer: None,
        }],
        additional_user_context: None,
    };
    let calls = vec![ToolCallRecord {
        id: CallId::new(),
        chat_id: chat,
        turn_id: turn,
        provider_id: "question_1".into(),
        name: crate::ASK_USER_QUESTIONS_TOOL.into(),
        arguments: serde_json::json!({
            "questions": [{
                "id": "target",
                "header": "Target",
                "question": "Where should I deploy?",
                "options": [{
                    "id": "staging",
                    "label": "Staging",
                    "description": "Deploy for verification."
                }]
            }]
        }),
        raw_arguments: None,
        execution: ToolCallExecution::Orchestration,
        status: ToolCallStatus::Completed,
        result: Some(serde_json::to_string(&answer).unwrap()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: Some(created_at),
    }];

    let rebuilt = rebuild_transcript(&[], &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 2);
    assert!(matches!(
        &rebuilt[0],
        ChatMessage {
            role: Role::Assistant,
            content: assistant,
            ..
        } if matches!(
            &assistant[..],
            [ContentBlock::ToolUse { id, name, .. }]
                if id == "question_1" && name == crate::ASK_USER_QUESTIONS_TOOL
        )
    ));
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error,
    } = &rebuilt[1].content[0]
    else {
        panic!("answer must rebuild as a tool result");
    };
    assert_eq!(rebuilt[1].role, Role::User);
    assert_eq!(tool_use_id, "question_1");
    assert!(!is_error);
    assert_eq!(
        serde_json::from_str::<crate::AnswerUserQuestions>(content).unwrap(),
        answer
    );
}

#[test]
fn rebuild_emits_tool_only_step_before_final_text() {
    let turn = TurnId::new();
    let chat = SessionId::new();
    let t0 = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
    let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
    let messages = vec![
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "go".into(),
            llm_content: None,
            created_at: t0,
        },
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "done".into(),
            llm_content: None,
            created_at: t2,
        },
    ];
    let calls = vec![ToolCallRecord {
        id: CallId::new(),
        chat_id: chat,
        turn_id: turn,
        provider_id: "tu_1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Completed,
        result: Some("data".into()),
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: t1,
        resolved_at: Some(t1),
    }];
    let rebuilt = rebuild_transcript(&messages, &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 4);
    assert_eq!(rebuilt[0].role, Role::User);
    assert!(matches!(
        &rebuilt[1].content[..],
        [ContentBlock::ToolUse { .. }]
    ));
    assert!(matches!(
        &rebuilt[2].content[..],
        [ContentBlock::ToolResult { .. }]
    ));
    assert_eq!(rebuilt[3].role, Role::Assistant);
}

#[test]
fn rebuild_skips_legacy_tool_role_rows() {
    let turn = TurnId::new();
    let chat = SessionId::new();
    let messages = vec![
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::User,
            reasoning: Default::default(),
            content: "hi".into(),
            llm_content: None,
            created_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        },
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::Tool,
            reasoning: Default::default(),
            content: "legacy".into(),
            llm_content: None,
            created_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
        },
        Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: turn,
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "bye".into(),
            llm_content: None,
            created_at: DateTime::<Utc>::from_timestamp(3, 0).unwrap(),
        },
    ];
    let rebuilt = rebuild_transcript(&messages, &[], &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[0].role, Role::User);
    assert_eq!(rebuilt[1].role, Role::Assistant);
}

#[tokio::test]
async fn second_turn_rebuilds_prior_tool_calls_into_transcript() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "hello from disk").unwrap();
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    // Turn 1: tool call then finish (FakeProvider).
    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;

    // Turn 2: provider that records the request so we can assert ToolUse/Result
    // blocks were rebuilt from the store.
    let seen: Arc<Mutex<Vec<ChatMessage>>> = Arc::new(Mutex::new(Vec::new()));
    struct CaptureProvider {
        seen: Arc<Mutex<Vec<ChatMessage>>>,
    }
    #[async_trait]
    impl ModelProvider for CaptureProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("capture")
        }
        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            *self.seen.lock().unwrap() = req.messages;
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }
    let agent = Agent::new(
        Arc::new(CaptureProvider { seen: seen.clone() }),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "what did you find?", &tx)
        .await
        .unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;

    let messages = seen.lock().unwrap().clone();
    assert!(
        messages.iter().any(|m| {
            m.role == Role::Assistant
                && m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { name, .. } if name == "read_file"))
        }),
        "expected rebuilt ToolUse in cross-turn transcript: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| {
            m.role == Role::User
                && m.content.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::ToolResult { content, .. } if content == "hello from disk"
                    )
                })
        }),
        "expected rebuilt ToolResult in cross-turn transcript: {messages:?}"
    );
}
