use super::*;

fn date_agent(store: Arc<dyn Store>) -> Agent {
    Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            image_input: true,
            ..Default::default()
        },
    )
}

fn text(message: &ChatMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn submission_dates_advance_at_utc_midnight_without_rewriting_prior_requests() {
    let (store, chat, _dir) = cancel_test_chat().await;
    let agent = date_agent(store.clone());
    let first = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        content: "Search news from the past seven days.".into(),
        llm_content: None,
        reasoning: Default::default(),
        created_at: DateTime::parse_from_rfc3339("2026-09-11T23:59:59Z")
            .unwrap()
            .with_timezone(&Utc),
    };
    store.append_message(&first).await.unwrap();
    let initial = agent.load_transcript(chat.id, None).await.unwrap();
    assert!(text(&initial.messages[0]).contains("Submission date: 2026-09-11 (UTC)"));
    assert_eq!(initial.user_texts, vec![(first.id, first.content.clone())]);

    let mut second = first.clone();
    second.id = MessageId::new();
    second.turn_id = TurnId::new();
    second.content = "What changed today?".into();
    // This local timestamp is already the next calendar date in UTC.
    second.created_at = DateTime::parse_from_rfc3339("2026-09-11T20:00:01-04:00")
        .unwrap()
        .with_timezone(&Utc);
    store.append_message(&second).await.unwrap();
    let later = agent.load_transcript(chat.id, None).await.unwrap();
    assert_eq!(later.messages[0], initial.messages[0]);
    assert!(text(&later.messages[1]).contains("Submission date: 2026-09-12 (UTC)"));
    assert!(!text(&later.messages[1]).contains("2026-09-11"));

    let replay = date_agent(store.clone())
        .load_transcript(chat.id, None)
        .await
        .unwrap();
    assert_eq!(
        replay.messages, later.messages,
        "resume must reuse the saved dates"
    );
    assert_eq!(
        store.list_messages(chat.id).await.unwrap(),
        vec![first, second]
    );
}

#[tokio::test]
async fn submission_date_preserves_model_context_and_image_attachments() {
    use crate::image::ImageMediaType;

    let (store, chat, _dir) = cancel_test_chat().await;
    let image = ImageRef {
        blob_id: uuid::Uuid::new_v4(),
        media_type: ImageMediaType::Png,
        width: 800,
        height: 600,
        byte_len: 4096,
    };
    assert!(store.publish_chat_image(chat.id, &image).await.unwrap());
    store
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "fake",
            "Compare this chart with today's news.",
            &[image],
            &[],
            &[],
        )
        .await
        .unwrap();
    let original = store.list_messages(chat.id).await.unwrap();
    let original_text = original[0]
        .llm_content
        .as_ref()
        .expect("attachment model context");
    let agent = date_agent(store.clone());
    let loaded = agent.load_transcript(chat.id, None).await.unwrap();
    assert!(text(&loaded.messages[0]).starts_with(original_text));
    assert!(text(&loaded.messages[0]).contains(&format!(
        "Submission date: {} (UTC)",
        original[0].created_at.date_naive()
    )));
    assert!(loaded.messages[0]
        .content
        .contains(&ContentBlock::Image { image }));
    assert_eq!(store.list_messages(chat.id).await.unwrap(), original);
}

struct DateRecordingProvider(Mutex<Vec<ChatRequest>>);

#[async_trait]
impl ModelProvider for DateRecordingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("date-recorder")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.0.lock().unwrap().push(request);
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "A dated answer.".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

#[tokio::test]
async fn submission_date_reaches_tool_and_chat_only_requests_without_changing_system_prompt() {
    for tools_supported in [true, false] {
        let (store, chat, _dir) = cancel_test_chat().await;
        let provider = Arc::new(DateRecordingProvider(Mutex::new(Vec::new())));
        let system = "A stable operating prompt.";
        let agent = Agent::new(
            provider.clone(),
            Arc::new(ToolRegistry::new().with(Box::new(CheckpointNoopTool))),
            store.clone(),
            AgentConfig {
                system_prompt: Some(system.into()),
                tools_supported,
                ..Default::default()
            },
        );
        let (tx, _rx) = unbounded();
        agent
            .run_turn(&chat, "What happened this week?", &tx)
            .await
            .unwrap();
        let rows = store.list_messages(chat.id).await.unwrap();
        let requests = provider.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].system.as_deref(), Some(system));
        assert_eq!(requests[0].tools.is_empty(), !tools_supported);
        assert!(text(&requests[0].messages[0]).contains(&format!(
            "Submission date: {} (UTC)",
            rows[0].created_at.date_naive()
        )));
        assert_eq!(rows[0].content, "What happened this week?");
        assert!(rows[0].llm_content.is_none());
    }
}

#[tokio::test]
async fn replay_keeps_live_steering_text_unchanged_within_the_dated_turn() {
    let (store, chat, _dir) = cancel_test_chat().await;
    let inbox = SteerInbox::new();
    let agent = date_agent(store.clone()).with_steer(inbox.clone());
    let turn_id = TurnId::new();
    agent
        .persist(chat.id, turn_id, Role::User, "Review this week's news.")
        .await
        .unwrap();
    let mut live = agent.load_transcript(chat.id, None).await.unwrap().messages;
    assert!(inbox.push("Focus on product releases.", false));
    let (tx, _rx) = unbounded();
    agent
        .apply_steers(
            &chat,
            turn_id,
            &mut live,
            None,
            &super::super::events::EventSink::Legacy(&tx),
        )
        .await
        .unwrap();
    assert_eq!(text(&live[1]), "Focus on product releases.");
    let replay = agent.load_transcript(chat.id, None).await.unwrap();
    assert_eq!(replay.messages, live);
}
