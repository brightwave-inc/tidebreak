use super::*;
use tidebreak_core::provider::{ChatMessage, MessageReasoning, PromptCacheMode, ReasoningOrigin};
use tidebreak_core::tool::ToolSpec;
use tidebreak_core::ReasoningEffort;

#[test]
fn request_maps_system_messages_and_tools() {
    let req = ChatRequest {
        provider: Some(ProviderId::new("anthropic")),
        model: "claude-opus-4-8".into(),
        reasoning_model: true,
        system: Some("be brief".into()),
        messages: vec![ChatMessage::text(Role::User, "hi")],
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
        }],
        max_tokens: None,
        temperature: Some(0.5),
        reasoning_effort: None,
        images: ImageAttachments::new(),
        ..Default::default()
    };
    let body = build_request_json(&req).unwrap();
    assert_eq!(body["model"], "claude-opus-4-8");
    assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"][0]["text"], "be brief");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["tools"][0]["name"], "read_file");
    assert_eq!(body["temperature"], 0.5);
    assert!(
        body.get("fallbacks").is_none(),
        "fallback models require an explicit registry contract"
    );
}

#[test]
fn cache_breakpoints_sit_on_the_last_tool_system_block_and_transcript_tail() {
    let req = ChatRequest {
        provider: Some(ProviderId::new("anthropic")),
        model: "claude-opus-4-8".into(),
        system: Some("be brief".into()),
        messages: vec![
            ChatMessage::text(Role::User, "hi"),
            ChatMessage::text(Role::Assistant, "hello"),
        ],
        tools: vec![
            ToolSpec {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            },
            ToolSpec {
                name: "write_file".into(),
                description: "write a file".into(),
                input_schema: json!({"type": "object"}),
            },
        ],
        images: ImageAttachments::new(),
        ..Default::default()
    };
    let body = build_request_json(&req).unwrap();

    let ephemeral = json!({ "type": "ephemeral" });
    // Tools render before the system prompt, which renders before the
    // messages, so these three breakpoints are the ends of the three
    // segments — in ascending prefix order.
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(body["tools"][1]["cache_control"], ephemeral);
    assert_eq!(body["system"][0]["cache_control"], ephemeral);
    assert!(body["messages"][0]["content"][0]
        .get("cache_control")
        .is_none());
    assert_eq!(
        body["messages"][1]["content"][0]["cache_control"],
        ephemeral
    );
    // Four is the hard cap; going over rejects the request outright.
    let breakpoints = serde_json::to_string(&body)
        .unwrap()
        .matches("cache_control")
        .count();
    assert_eq!(breakpoints, 3, "{body}");
}

#[test]
fn the_one_hour_retention_extends_every_breakpoint() {
    let req = ChatRequest {
        provider: Some(ProviderId::new("anthropic")),
        model: "claude-opus-4-8".into(),
        system: Some("be brief".into()),
        messages: vec![
            ChatMessage::text(Role::User, "hi"),
            ChatMessage::text(Role::Assistant, "hello"),
        ],
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
        }],
        prompt_cache_retention: PromptCacheRetention::OneHour,
        images: ImageAttachments::new(),
        ..Default::default()
    };
    let body = build_request_json(&req).unwrap();

    // Every breakpoint carries the TTL, none stays on the default:
    // Anthropic rejects a 1-hour breakpoint that appears after a 5-minute
    // one, and tools and system render before the transcript.
    let ephemeral_1h = json!({ "type": "ephemeral", "ttl": "1h" });
    assert_eq!(body["tools"][0]["cache_control"], ephemeral_1h);
    assert_eq!(body["system"][0]["cache_control"], ephemeral_1h);
    assert_eq!(
        body["messages"][1]["content"][0]["cache_control"],
        ephemeral_1h
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert_eq!(
        serialized.matches("cache_control").count(),
        serialized.matches(r#""ttl":"1h""#).count(),
        "{body}"
    );
}

#[test]
fn a_lagging_breakpoint_keeps_a_wide_tool_fan_out_inside_the_lookup_window() {
    // One model step appends an assistant message with 15 tool calls and a
    // user message with their 15 results: 30 blocks at once, beyond the
    // 20-block cache-lookup window. A lone tail breakpoint would then sit
    // further than 20 blocks from everything the previous call cached, and
    // the next call would silently pay a full-price read. This pins the
    // lagging breakpoint that keeps the previous tail inside a window.
    let mut messages = vec![
        ChatMessage::text(Role::User, "hi"),
        ChatMessage::text(Role::Assistant, "on it"),
    ];
    // Flattened block index of the previous call's tail breakpoint.
    let previous_tail = 1usize;
    messages.push(ChatMessage {
        role: Role::Assistant,
        content: (0..15)
            .map(|i| ContentBlock::ToolUse {
                id: format!("call_{i}"),
                name: "read_file".into(),
                input: json!({"path": format!("f{i}.rs")}),
            })
            .collect(),
        reasoning: MessageReasoning::default(),
    });
    messages.push(ChatMessage {
        role: Role::User,
        content: (0..15)
            .map(|i| ContentBlock::ToolResult {
                tool_use_id: format!("call_{i}"),
                content: "ok".into(),
                is_error: false,
            })
            .collect(),
        reasoning: MessageReasoning::default(),
    });
    let req = ChatRequest {
        provider: Some(ProviderId::new("anthropic")),
        model: "claude-opus-4-8".into(),
        system: Some("be brief".into()),
        messages,
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
        }],
        images: ImageAttachments::new(),
        ..Default::default()
    };
    let body = build_request_json(&req).unwrap();

    // Flatten the transcript's content blocks in wire order and locate the
    // breakpoints.
    let mut breakpoints = Vec::new();
    let mut index = 0usize;
    for message in body["messages"].as_array().unwrap() {
        for block in message["content"].as_array().unwrap() {
            if block.get("cache_control").is_some() {
                breakpoints.push(index);
            }
            index += 1;
        }
    }
    assert_eq!(index, 32);
    let [lagging, tail] = breakpoints.as_slice() else {
        panic!("expected exactly two transcript breakpoints: {body}");
    };
    assert_eq!(*tail, index - 1);
    // The lagging breakpoint trails the tail by exactly one window, and
    // the previous call's tail — 30 blocks back from the new tail, out of
    // reach of it — sits inside the lagging breakpoint's window.
    assert_eq!(tail - lagging, 20);
    assert!(*lagging > previous_tail);
    assert!(*lagging - previous_tail <= 20);
    // Four is the hard cap; going over rejects the request outright.
    let all_breakpoints = serde_json::to_string(&body)
        .unwrap()
        .matches("cache_control")
        .count();
    assert_eq!(all_breakpoints, 4, "{body}");
}

/// Flattened wire order of the transcript's content blocks: the type of
/// each, and the indices carrying a breakpoint.
fn transcript_breakpoints(body: &Value) -> (Vec<String>, Vec<usize>) {
    let mut types = Vec::new();
    let mut breakpoints = Vec::new();
    for message in body["messages"].as_array().unwrap() {
        for block in message["content"].as_array().unwrap() {
            if block.get("cache_control").is_some() {
                breakpoints.push(types.len());
            }
            types.push(block["type"].as_str().unwrap().to_owned());
        }
    }
    (types, breakpoints)
}

#[test]
fn breakpoints_are_spaced_on_the_layout_that_includes_replayed_reasoning() {
    // Replayed thinking blocks are wire blocks and count toward the
    // lookup window, so the spacing has to be measured after they are
    // attached. Four steps of a four-way fan-out, each assistant message
    // carrying three thinking blocks: measured before the replay the
    // lagging breakpoint would land 26 blocks from the tail, outside the
    // window it exists to stay inside.
    let mut req = reasoning_request("claude-opus-5", None);
    let reasoning: Vec<Value> = (0..3)
        .map(|i| json!({"type": "thinking", "thinking": format!("step {i}"), "signature": "s"}))
        .collect();
    req.messages = vec![ChatMessage::text(Role::User, "hi")];
    for step in 0..4 {
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            content: (0..4)
                .map(|i| ContentBlock::ToolUse {
                    id: format!("call_{step}_{i}"),
                    name: "read_file".into(),
                    input: json!({"path": format!("f{i}.rs")}),
                })
                .collect(),
            reasoning: MessageReasoning::captured(
                ReasoningOrigin {
                    provider: Some(ProviderId::new("anthropic")),
                    model: "claude-opus-5".into(),
                },
                reasoning.clone(),
            ),
        });
        req.messages.push(ChatMessage {
            role: Role::User,
            content: (0..4)
                .map(|i| ContentBlock::ToolResult {
                    tool_use_id: format!("call_{step}_{i}"),
                    content: "ok".into(),
                    is_error: false,
                })
                .collect(),
            reasoning: MessageReasoning::default(),
        });
    }
    let body = build_request_json(&req).unwrap();

    let (types, breakpoints) = transcript_breakpoints(&body);
    assert_eq!(types.len(), 45, "{body}");
    let [lagging, tail] = breakpoints.as_slice() else {
        panic!("expected exactly two transcript breakpoints: {body}");
    };
    assert_eq!(*tail, types.len() - 1);
    assert!(
        tail - lagging <= 20,
        "the lagging breakpoint must stay inside the lookup window: {breakpoints:?}"
    );
    // One window back lands on a replayed thinking block, which cannot
    // carry `cache_control`; the breakpoint moves toward the tail, never
    // away, so the spacing only shrinks.
    assert_eq!(types[tail - 20], "thinking");
    assert_eq!(types[*lagging], "tool_use");
    assert_eq!(tail - lagging, 18);
}

#[test]
fn a_one_shot_request_writes_no_cache_entries() {
    // A titling or judging call sends a prompt nothing will re-send, so
    // every breakpoint would be billed at the write premium and expire
    // unread.
    let req = ChatRequest {
        provider: Some(ProviderId::new("anthropic")),
        model: "claude-opus-4-8".into(),
        system: Some("be brief".into()),
        messages: vec![
            ChatMessage::text(Role::User, "hi"),
            ChatMessage::text(Role::Assistant, "hello"),
        ],
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
        }],
        prompt_cache: PromptCacheMode::OneShot,
        images: ImageAttachments::new(),
        ..Default::default()
    };
    let body = build_request_json(&req).unwrap();
    assert!(
        !serde_json::to_string(&body)
            .unwrap()
            .contains("cache_control"),
        "{body}"
    );
    // The system prompt keeps its block form; only the marker is gone.
    assert_eq!(body["system"][0]["text"], "be brief");
}

fn reasoning_request(model: &str, effort: Option<ReasoningEffort>) -> ChatRequest {
    ChatRequest {
        provider: Some(ProviderId::new("anthropic")),
        model: model.into(),
        reasoning_model: true,
        system: None,
        messages: vec![ChatMessage::text(Role::User, "hi")],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        reasoning_effort: effort,
        images: ImageAttachments::new(),
        ..Default::default()
    }
}

#[test]
fn a_reasoning_model_is_asked_to_think_out_loud() {
    // Omitting `thinking` means thinking is off on Opus 4.7 and later, and
    // the default `display` streams empty thinking blocks — a silent pause
    // where the transcript should be showing reasoning.
    let body = build_request_json(&reasoning_request("claude-opus-5", None)).unwrap();
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["thinking"]["display"], "summarized");
    // Absent a per-chat override the provider's own effort default holds.
    assert!(body.get("output_config").is_none());
}

#[test]
fn fable_5_1_constrains_output_natively_and_keeps_thinking() {
    let mut req = reasoning_request("claude-fable-5-1", Some(ReasoningEffort::Low));
    req.response_format = Some(ResponseFormat::JsonSchema {
        name: "note".into(),
        schema: json!({
            "type": "object",
            "properties": { "body": { "type": "string" } },
            "required": ["body"],
        }),
    });
    let body = build_request_json(&req).unwrap();
    // No forced call, no synthetic tool: the model rejects the former, and
    // the native constraint makes the latter redundant.
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("tools").is_none());
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(
        body["output_config"]["format"]["schema"]["additionalProperties"],
        false
    );
    // Effort shares `output_config` with the format rather than replacing
    // it, and nothing on the wire is forced, so the request still thinks.
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["thinking"]["type"], "adaptive");
}

#[test]
fn fable_5_1_asks_for_stale_thinking_to_be_dropped() {
    let body = build_request_json(&reasoning_request(
        "claude-fable-5-1",
        Some(ReasoningEffort::High),
    ))
    .unwrap();
    assert_eq!(
        body["thinking"]["block_binding"]["prefix_mismatch_behavior"],
        "drop_block"
    );
    // Opus 5 does not bind its blocks, and the field is a beta the older
    // rows must not be sent.
    let body = build_request_json(&reasoning_request(
        "claude-opus-5",
        Some(ReasoningEffort::High),
    ))
    .unwrap();
    assert!(body["thinking"].get("block_binding").is_none());
}

#[test]
fn fable_5_1_refuses_a_forced_tool_choice_by_name() {
    for choice in [
        ToolChoice::Required,
        ToolChoice::Tool {
            name: "note".into(),
        },
    ] {
        let mut req = reasoning_request("claude-fable-5-1", None);
        req.tool_choice = Some(choice);
        let err = build_request_json(&req).unwrap_err().to_string();
        assert!(err.contains("claude-fable-5-1"), "{err}");
        assert!(err.contains("forced tool choice"), "{err}");
    }
    // `auto` and `none` are unchanged on the model.
    let mut req = reasoning_request("claude-fable-5-1", None);
    req.tool_choice = Some(ToolChoice::None);
    assert_eq!(
        build_request_json(&req).unwrap()["tool_choice"],
        json!({ "type": "none" })
    );
}

#[test]
fn the_fable_5_1_contract_follows_family_and_generation() {
    for id in [
        "claude-fable-5-1",
        "claude-mythos-5-1",
        "claude-fable-6",
        "us.anthropic.claude-fable-5-1",
    ] {
        assert!(fable_5_1_or_later(id), "{id}");
    }
    for id in [
        "claude-fable-5",
        "claude-opus-5",
        "claude-opus-5-1",
        "claude-sonnet-5",
        "claude-opus-4-8",
        "some-gateway-alias",
    ] {
        assert!(!fable_5_1_or_later(id), "{id}");
    }
}

#[test]
fn a_constrained_response_is_a_forced_tool_read_back_as_text() {
    let mut req = reasoning_request("claude-opus-5", Some(ReasoningEffort::Low));
    req.response_format = Some(ResponseFormat::JsonSchema {
        name: "note".into(),
        schema: json!({
            "type": "object",
            "properties": { "body": { "type": "string" } },
            "required": ["body"],
        }),
    });
    let body = build_request_json(&req).unwrap();
    assert_eq!(
        body["tool_choice"],
        json!({ "type": "tool", "name": "note" })
    );
    assert_eq!(body["tools"][0]["name"], "note");
    assert_eq!(
        body["tools"][0]["input_schema"]["additionalProperties"],
        false
    );
    // With extended thinking on, this API accepts only `auto` or `none` for
    // `tool_choice`, so a constrained request cannot also think.
    assert!(body.get("thinking").is_none());

    // The constrained value arrives on the text channel, and the turn ends
    // rather than reading as one waiting on a tool result. Both consumers of
    // this stream reject a tool call they never advertised.
    let mut state = StreamState {
        output_tool: Some("note".into()),
        ..StreamState::default()
    };
    let events: Vec<ProviderEvent> = [
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "note" },
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "{\"body\":\"hi\"}" },
        }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" } }),
    ]
    .iter()
    .flat_map(|frame| normalize(frame, &mut state))
    .collect();
    assert_eq!(
        events,
        vec![
            ProviderEvent::TextDelta {
                text: "{\"body\":\"hi\"}".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]
    );
}

#[test]
fn a_per_chat_effort_override_reaches_the_wire() {
    let body = build_request_json(&reasoning_request(
        "claude-opus-5",
        Some(ReasoningEffort::Low),
    ))
    .unwrap();
    assert_eq!(body["output_config"]["effort"], "low");
}

#[test]
fn claude_4_6_xhigh_is_clamped_before_the_request_wire() {
    for id in [
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-sonnet-4-6-20260101",
    ] {
        let body =
            build_request_json(&reasoning_request(id, Some(ReasoningEffort::XHigh))).unwrap();
        assert_eq!(body["output_config"]["effort"], "high", "{id}");

        let body = build_request_json(&reasoning_request(id, Some(ReasoningEffort::Max))).unwrap();
        assert_eq!(body["output_config"]["effort"], "max", "{id}");
    }

    // The newer rows that do accept xhigh keep it unchanged.
    let body = build_request_json(&reasoning_request(
        "claude-opus-4-7",
        Some(ReasoningEffort::XHigh),
    ))
    .unwrap();
    assert_eq!(body["output_config"]["effort"], "xhigh");
}

#[test]
fn a_non_reasoning_request_asks_for_no_thinking() {
    let req = request_with(
        vec![ContentBlock::Text { text: "hi".into() }],
        ImageAttachments::new(),
    );
    assert!(!req.reasoning_model);
    let body = build_request_json(&req).unwrap();
    assert!(body.get("thinking").is_none());
    assert!(body.get("output_config").is_none());
}

#[test]
fn haiku_4_5_never_gets_an_unsupported_adaptive_request() {
    // Both curated routes mark Haiku 4.5 non-reasoning until classic
    // `budget_tokens` thinking is implemented. A stale direct request that
    // still calls it reasoning-capable must remain safe too: this adapter
    // cannot silently substitute the adaptive 4.6+ shape.
    for id in ["claude-haiku-4-5-20251001", "claude-haiku-4-5"] {
        let body = build_request_json(&reasoning_request(id, Some(ReasoningEffort::High))).unwrap();
        assert!(body.get("thinking").is_none(), "{id}");
        assert!(body.get("output_config").is_none(), "{id}");
    }
}

#[test]
fn the_adaptive_switch_follows_the_generation_in_the_model_id() {
    for id in [
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-opus-6.2",
        "claude-opus-5-20260101",
    ] {
        assert!(takes_adaptive_thinking(id), "{id} should reason adaptively");
    }
    for id in [
        "claude-haiku-4-5-20251001",
        "claude-opus-4-5",
        "claude-opus-4-1-20250805",
        "claude-3-5-sonnet-20241022",
        // No readable generation: keep today's request rather than risk a
        // parameter the endpoint rejects.
        "some-gateway-alias",
        "claude-next",
    ] {
        assert!(!takes_adaptive_thinking(id), "{id} should not");
    }
    assert_eq!(claude_generation("claude-haiku-4-5-20251001"), Some((4, 5)));
    assert_eq!(claude_generation("claude-opus-5"), Some((5, 0)));
    assert_eq!(claude_generation("claude-opus-6.2"), Some((6, 2)));
}

fn run(events: &[Value]) -> Vec<ProviderEvent> {
    run_with_origin(events, None)
}

fn run_with_origin(events: &[Value], replay_origin: Option<ReasoningOrigin>) -> Vec<ProviderEvent> {
    let mut state = StreamState {
        replay_origin,
        ..StreamState::default()
    };
    events
        .iter()
        .flat_map(|e| normalize(e, &mut state))
        .collect()
}

#[test]
fn usage_counts_saturate_instead_of_wrapping() {
    let events = run(&[
        json!({
            "type": "message_start",
            "message": {"usage": {"input_tokens": u64::from(u32::MAX) + 1}}
        }),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": u64::MAX}
        }),
    ]);
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderEvent::Usage(Usage {
            input_tokens: u32::MAX,
            output_tokens: u32::MAX,
            ..
        })
    )));
}

#[test]
fn oversized_structural_index_fails_the_stream() {
    let events = run(&[json!({
        "type": "content_block_start",
        "index": u64::from(u32::MAX) + 1,
        "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}
    })]);
    assert!(matches!(events.as_slice(), [ProviderEvent::Failed { .. }]));
}

#[test]
fn missing_or_non_unsigned_structural_indices_cannot_alias_an_open_block() {
    let invalid_indices = [
        Some(json!(-1)),
        Some(json!(1.5)),
        Some(json!("0")),
        Some(Value::Null),
        None,
    ];
    for invalid in invalid_indices {
        let mut invalid_delta = json!({
            "type": "content_block_delta",
            "delta": {"type": "input_json_delta", "partial_json": "a\"}"}
        });
        if let Some(invalid) = invalid {
            invalid_delta["index"] = invalid;
        }
        let mut state = StreamState::default();
        let events: Vec<ProviderEvent> = [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\""}}),
            invalid_delta,
        ]
        .iter()
        .flat_map(|frame| normalize(frame, &mut state))
        .collect();

        assert!(matches!(
            events.as_slice(),
            [
                ProviderEvent::ToolCallStarted { index: 0, .. },
                ProviderEvent::ToolCallArgsDelta { index: 0, fragment },
                ProviderEvent::Failed { .. },
            ] if fragment == "{\"path\":\""
        ));
        assert!(state.terminal);
    }
}

fn search_origin(model: &str) -> ReasoningOrigin {
    ReasoningOrigin {
        provider: Some(ProviderId::new("anthropic")),
        model: model.into(),
    }
}

#[test]
fn in_band_error_fails_the_stream_instead_of_ending_it_cleanly() {
    let out = run(&[
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "partial"}}),
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        // Anything after the error must not resurrect a clean stop.
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
    ]);
    assert_eq!(
        out,
        vec![
            ProviderEvent::TextDelta {
                text: "partial".into()
            },
            ProviderEvent::Failed {
                error: ProviderErrorInfo {
                    kind: "overloaded".into(),
                    message: "anthropic returned 500 (overloaded_error): Overloaded".into(),
                },
            },
        ]
    );
}

#[test]
fn mid_stream_context_overflow_fails_the_stream_and_strands_no_tool_call() {
    let out = run(&[
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"a"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "model_context_window_exceeded"}, "usage": {"output_tokens": 9}}),
    ]);
    assert_eq!(
        out,
        vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "toolu_1".into(),
                name: "read_file".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: "{\"path\":\"a".into(),
            },
            ProviderEvent::Usage(Usage {
                output_tokens: 9,
                ..Usage::default()
            }),
            ProviderEvent::Failed {
                error: ProviderErrorInfo::from_error(&AgentError::PromptTooLong(
                    "anthropic: the model's context window was exceeded mid-response".into(),
                )),
            },
        ]
    );
}

#[test]
fn a_silent_close_with_an_open_tool_call_fails_the_stream() {
    // A clean TCP close mid-response carries no transport error and no
    // stop_reason. With a tool call's argument JSON still open the stream
    // was truncated, so the adapter must not let the fragment read as a
    // finished turn. This changes behavior on streams that reported
    // success before — the remaining silent-close route after the
    // transport-error and in-band-error paths were closed.
    let mut state = StreamState::default();
    let out: Vec<ProviderEvent> = [
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"a"}}),
    ]
    .iter()
    .flat_map(|frame| normalize(frame, &mut state))
    .collect();
    assert!(matches!(
        out.last(),
        Some(ProviderEvent::ToolCallArgsDelta { .. })
    ));
    let ending = end_of_stream(&state).expect("an open tool call fails the stream");
    assert!(
        matches!(
            &ending,
            ProviderEvent::Failed { error } if error.kind == "provider"
        ),
        "expected a failure, got {ending:?}"
    );

    // Once the block stops and the provider reports a stop_reason, the
    // close is clean and the end-of-stream check stays silent.
    let mut state = StreamState::default();
    for frame in [
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
    ] {
        let _ = normalize(&frame, &mut state);
    }
    assert!(end_of_stream(&state).is_none());

    // Text alone keeps the clean-end reading an exhausted stream already
    // had: no tool-call arguments exist that truncation could corrupt.
    let mut state = StreamState::default();
    let _ = normalize(
        &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}}),
        &mut state,
    );
    assert!(end_of_stream(&state).is_none());
}

#[test]
fn normalizes_text_and_usage_and_stop() {
    let out = run(&[
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "cache_read_input_tokens": 4}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "he"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "llo"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 7}}),
    ]);
    assert_eq!(
        out,
        vec![
            ProviderEvent::TextDelta { text: "he".into() },
            ProviderEvent::TextDelta { text: "llo".into() },
            ProviderEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 7,
                cache_read_input_tokens: 4,
                cache_creation_input_tokens: 0,
            }),
            ProviderEvent::Stop {
                reason: StopReason::EndTurn
            },
        ]
    );
}

#[test]
fn normalizes_tool_call_and_reasoning() {
    let out = run(&[
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}}),
    ]);
    assert_eq!(
        out,
        vec![
            ProviderEvent::ToolCallStarted {
                index: 1,
                id: "toolu_1".into(),
                name: "read_file".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 1,
                fragment: "{\"path\":".into(),
            },
            ProviderEvent::ReasoningDelta { text: "hmm".into() },
        ]
    );
}

#[test]
fn reasoning_blocks_are_captured_whole_and_opaque() {
    let out = run(&[
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "plan: "}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "read first"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-1"}}),
        json!({"type": "content_block_stop", "index": 0}),
        // A redacted block arrives complete and must round-trip untouched:
        // filtering capture on `type == "thinking"` would drop it and
        // split the message's reasoning prefix on replay.
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "redacted_thinking", "data": "opaque-blob"}}),
        json!({"type": "content_block_stop", "index": 1}),
    ]);
    assert_eq!(
        out,
        vec![
            ProviderEvent::ReasoningDelta {
                text: "plan: ".into()
            },
            ProviderEvent::ReasoningDelta {
                text: "read first".into()
            },
            ProviderEvent::ReasoningBlock {
                data: json!({
                    "type": "thinking",
                    "thinking": "plan: read first",
                    "signature": "sig-1",
                }),
            },
            ProviderEvent::ReasoningBlock {
                data: json!({
                    "type": "redacted_thinking",
                    "data": "opaque-blob",
                }),
            },
        ]
    );
}

#[test]
fn split_reasoning_signature_survives_agent_persistence_and_parallel_tool_replay() {
    let events = run(&[
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "check both sources"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "signed-"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "tail"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "redacted_thinking", "data": "opaque"}}),
        json!({"type": "content_block_stop", "index": 1}),
    ]);
    let captured: Vec<Value> = events
        .into_iter()
        .filter_map(|event| match event {
            ProviderEvent::ReasoningBlock { data } => Some(data),
            _ => None,
        })
        .collect();
    let expected = vec![
        json!({
            "type": "thinking",
            "thinking": "check both sources",
            "signature": "signed-tail",
        }),
        json!({"type": "redacted_thinking", "data": "opaque"}),
    ];
    assert_eq!(captured, expected);

    // The agent persists MessageReasoning separately from ChatMessage and
    // reconstructs it before the next provider step. Exercise that same
    // serde boundary before replaying a parallel tool batch.
    let origin = ReasoningOrigin {
        provider: Some(ProviderId::new("anthropic")),
        model: "claude-opus-5".into(),
    };
    let stored = serde_json::to_value(MessageReasoning::captured(origin, captured)).unwrap();
    let restored: MessageReasoning = serde_json::from_value(stored).unwrap();
    let mut req = reasoning_request("claude-opus-5", None);
    req.messages = vec![
        ChatMessage::text(Role::User, "compare these"),
        ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "web_extract".into(),
                    input: json!({"url": "https://example.com/one"}),
                },
                ContentBlock::ToolUse {
                    id: "toolu_2".into(),
                    name: "web_extract".into(),
                    input: json!({"url": "https://example.com/two"}),
                },
            ],
            reasoning: restored,
        },
        ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".into(),
                    content: "first".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "toolu_2".into(),
                    content: "second".into(),
                    is_error: false,
                },
            ],
            reasoning: MessageReasoning::default(),
        },
    ];

    let body = build_request_json(&req).unwrap();
    let replayed = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(replayed[..expected.len()], expected);
    assert_eq!(replayed[2]["id"], "toolu_1");
    assert_eq!(replayed[3]["id"], "toolu_2");
}

#[test]
fn captured_reasoning_is_replayed_verbatim_ahead_of_its_content() {
    let reasoning = vec![
        json!({"type": "thinking", "thinking": "plan: read first", "signature": "sig-1"}),
        json!({"type": "redacted_thinking", "data": "opaque-blob"}),
    ];
    let mut req = reasoning_request("claude-opus-5", None);
    req.messages = vec![
        ChatMessage::text(Role::User, "hi"),
        ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "checking".into(),
                },
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "a"}),
                },
            ],
            reasoning: MessageReasoning::captured(
                ReasoningOrigin {
                    provider: Some(ProviderId::new("anthropic")),
                    model: "claude-opus-5".into(),
                },
                reasoning.clone(),
            ),
        },
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "ok".into(),
                is_error: false,
            }],
            reasoning: MessageReasoning::default(),
        },
    ];
    let body = build_request_json(&req).unwrap();
    let content = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(
        content[..2],
        reasoning[..],
        "the blocks go back byte-identical, redacted included"
    );
    assert_eq!(content[2]["type"], "text");
    assert_eq!(content[3]["type"], "tool_use");
    // The transcript-tail breakpoint still lands on the true tail.
    assert_eq!(
        body["messages"][2]["content"][0]["cache_control"],
        ephemeral_cache_control(PromptCacheRetention::FiveMinutes)
    );

    // The same native protocol served through a gateway has a distinct
    // route identity. Its blocks replay on the gateway, but never on
    // direct Anthropic.
    req.provider = Some(ProviderId::new("model_gateway"));
    req.messages[1].reasoning = MessageReasoning::captured(
        ReasoningOrigin {
            provider: Some(ProviderId::new("model_gateway")),
            model: "claude-opus-5".into(),
        },
        reasoning.clone(),
    );
    let body = build_request_json(&req).unwrap();
    assert_eq!(
        body["messages"][1]["content"].as_array().unwrap()[..2],
        reasoning[..]
    );
    req.provider = Some(ProviderId::new("anthropic"));
    let body = build_request_json(&req).unwrap();
    assert_eq!(body["messages"][1]["content"].as_array().unwrap().len(), 2);
}

#[test]
fn a_route_switch_replays_no_foreign_reasoning() {
    // A chat may move between Anthropic, OpenAI and Gemini models — and
    // between two Anthropic models — mid-conversation. History rebuilt
    // across such a switch still carries the blocks the earlier model
    // signed, and this model would reject them. Dropping them is the
    // answer: sending no reasoning is always a valid shape.
    let block = json!({"type": "thinking", "thinking": "plan", "signature": "sig-1"});
    let step = |provider: Option<&str>, model: &str| ChatMessage {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "checking".into(),
        }],
        reasoning: MessageReasoning::captured(
            ReasoningOrigin {
                provider: provider.map(ProviderId::new),
                model: model.into(),
            },
            vec![block.clone()],
        ),
    };
    for origin in [
        // Another Anthropic model: same wire shape, signature this model
        // never produced.
        step(Some("anthropic"), "claude-sonnet-5"),
        // Another provider entirely.
        step(Some("openai"), "claude-opus-5"),
        // The same protocol through a different serving provider.
        step(Some("model_gateway"), "claude-opus-5"),
    ] {
        let mut req = reasoning_request("claude-opus-5", None);
        req.messages = vec![ChatMessage::text(Role::User, "hi"), origin];
        let body = build_request_json(&req).unwrap();
        let content = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "no block was replayed: {body}");
        assert_eq!(content[0]["type"], "text");
    }
}

#[test]
fn reasoning_is_omitted_when_the_request_does_not_think() {
    // A constrained request cannot think (forced tool), so the replay
    // stays off entirely: omission is always valid, and blocks sent to a
    // non-thinking request would be a shape the API has not promised.
    let mut req = reasoning_request("claude-opus-5", None);
    req.response_format = Some(ResponseFormat::JsonSchema {
        name: "note".into(),
        schema: json!({
            "type": "object",
            "properties": { "body": { "type": "string" } },
            "required": ["body"],
        }),
    });
    req.messages.push(ChatMessage {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "checking".into(),
        }],
        reasoning: MessageReasoning::captured(
            ReasoningOrigin {
                provider: Some(ProviderId::new("anthropic")),
                model: "claude-opus-5".into(),
            },
            vec![json!({"type": "thinking", "thinking": "t", "signature": "s"})],
        ),
    });
    req.messages.push(ChatMessage::text(Role::User, "go on"));
    let body = build_request_json(&req).unwrap();
    assert!(body.get("thinking").is_none());
    assert_eq!(
        body["messages"][1]["content"].as_array().unwrap().len(),
        1,
        "no reasoning block is attached: {body}"
    );
}

#[test]
fn maps_stop_reasons() {
    use StopOutcome::{Interrupted, Reason};
    assert_eq!(
        map_stop_reason("anthropic", "tool_use"),
        Reason(StopReason::ToolUse)
    );
    assert_eq!(
        map_stop_reason("anthropic", "max_tokens"),
        Reason(StopReason::MaxTokens)
    );
    assert_eq!(
        map_stop_reason("anthropic", "refusal"),
        Reason(StopReason::Refusal)
    );
    assert_eq!(
        map_stop_reason("anthropic", "future_reason"),
        Reason(StopReason::EndTurn)
    );
    assert!(matches!(
        map_stop_reason("model_gateway", "pause_turn"),
        Interrupted(error) if error.message == "model_gateway: the provider paused the turn"
    ));
    assert!(matches!(
        map_stop_reason("model_gateway", "model_context_window_exceeded"),
        Interrupted(error)
            if error.kind == "prompt_too_long"
                && error.message
                    == "model_gateway: the model's context window was exceeded mid-response"
    ));
}

#[test]
fn refusal_carries_bounded_category_when_present() {
    let out = run(&[json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": "refusal",
            "stop_details": {
                "type": "refusal",
                "category": "cyber"
            }
        },
        "usage": {"output_tokens": 0}
    })]);
    assert_eq!(
        out,
        vec![
            ProviderEvent::Usage(Usage::default()),
            ProviderEvent::Refusal {
                details: RefusalDetails::from_category(Some("cyber")),
            },
        ]
    );
}

#[test]
fn refusal_after_text_deltas_preserves_the_streamed_prefix() {
    let out = run(&[
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "A partial "}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "answer"}}),
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "refusal",
                "stop_details": {"type": "refusal", "category": "general_harms"}
            },
            "usage": {"output_tokens": 3}
        }),
    ]);
    assert_eq!(
        out,
        vec![
            ProviderEvent::TextDelta {
                text: "A partial ".into(),
            },
            ProviderEvent::TextDelta {
                text: "answer".into(),
            },
            ProviderEvent::Usage(Usage {
                output_tokens: 3,
                ..Usage::default()
            }),
            ProviderEvent::Refusal {
                details: RefusalDetails::from_category(Some("general_harms")),
            },
        ]
    );
}

#[test]
fn refusal_allows_missing_or_null_stop_details() {
    for delta in [
        json!({"stop_reason": "refusal"}),
        json!({"stop_reason": "refusal", "stop_details": null}),
        json!({"stop_reason": "refusal", "stop_details": {"category": null}}),
    ] {
        let out = run(&[json!({"type": "message_delta", "delta": delta})]);
        assert_eq!(
            out,
            vec![ProviderEvent::Refusal {
                details: RefusalDetails::default(),
            }]
        );
    }
}

// ── Image blocks ───────────────────────────────────────────────

fn png_ref(blob: u128) -> tidebreak_core::ImageRef {
    tidebreak_core::ImageRef {
        blob_id: uuid::Uuid::from_u128(blob),
        media_type: tidebreak_core::ImageMediaType::Png,
        width: 800,
        height: 600,
        byte_len: 3,
    }
}

fn request_with(content: Vec<ContentBlock>, images: ImageAttachments) -> ChatRequest {
    ChatRequest {
        provider: Some(ProviderId::new("anthropic")),
        model: "claude-opus-4-8".into(),
        reasoning_model: false,
        system: None,
        messages: vec![ChatMessage {
            role: Role::User,
            content,
            reasoning: MessageReasoning::default(),
        }],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        reasoning_effort: None,
        images,
        ..Default::default()
    }
}

#[test]
fn an_image_block_becomes_a_base64_source_block() {
    let image = png_ref(1);
    let mut images = ImageAttachments::new();
    images.insert(
        image.blob_id,
        tidebreak_core::ImageData::new(tidebreak_core::ImageMediaType::Png, vec![1, 2, 3]),
    );
    let req = request_with(
        vec![
            ContentBlock::Text {
                text: "what is this?".into(),
            },
            ContentBlock::Image { image },
        ],
        images,
    );

    let body = build_request_json(&req).unwrap();
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "base64");
    assert_eq!(content[1]["source"]["media_type"], "image/png");
    assert_eq!(content[1]["source"]["data"], BASE64.encode([1, 2, 3]));
    // A single image needs no ordinal label.
    assert_eq!(content.as_array().unwrap().len(), 2);
}

#[test]
fn multiple_images_are_labelled_so_the_model_can_refer_to_each() {
    let (first, second) = (png_ref(1), png_ref(2));
    let mut images = ImageAttachments::new();
    for image in [&first, &second] {
        images.insert(
            image.blob_id,
            tidebreak_core::ImageData::new(tidebreak_core::ImageMediaType::Png, vec![7]),
        );
    }
    let req = request_with(
        vec![
            ContentBlock::Image { image: first },
            ContentBlock::Image { image: second },
        ],
        images,
    );

    let body = build_request_json(&req).unwrap();
    let content = body["messages"][0]["content"].as_array().unwrap().clone();
    assert_eq!(content.len(), 4);
    assert_eq!(content[0]["text"], "Image 1:");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[2]["text"], "Image 2:");
    assert_eq!(content[3]["type"], "image");
}

#[test]
fn an_unhydrated_image_fails_the_request_instead_of_being_dropped() {
    // Reduction rewrites intentionally dropped images into text, so bytes
    // missing here mean something went wrong. Silently sending the turn
    // would ask the model about an image it never received.
    let req = request_with(
        vec![ContentBlock::Image { image: png_ref(9) }],
        ImageAttachments::new(),
    );
    let err = build_request_json(&req).unwrap_err();
    assert!(err.to_string().contains("no hydrated bytes"), "{err}");
}

#[test]
fn text_and_tool_blocks_keep_their_existing_wire_shape() {
    // Guards the refactor away from blanket passthrough: non-image blocks
    // must serialize exactly as before.
    let blocks = vec![
        ContentBlock::Text { text: "hi".into() },
        ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: "done".into(),
            is_error: false,
        },
    ];
    let shaped = anthropic_content(
        &blocks,
        &ImageAttachments::new(),
        false,
        Some(&ProviderId::new("anthropic")),
        "claude-opus-5",
    )
    .unwrap();
    assert_eq!(shaped, serde_json::to_value(&blocks).unwrap());
}

// ── Vendor web search ──────────────────────────────────────────

fn search_request(model: &str) -> ChatRequest {
    ChatRequest {
        provider: Some(ProviderId::new("anthropic")),
        model: model.into(),
        messages: vec![ChatMessage::text(Role::User, "what happened today?")],
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
        }],
        vendor_web_search: Some(tidebreak_core::provider::VendorWebSearch { max_uses: 3 }),
        ..Default::default()
    }
}

#[test]
fn the_vendor_search_tool_is_declared_beside_the_client_tools() {
    let body = build_request_json(&search_request("claude-opus-5")).unwrap();
    let tools = body["tools"].as_array().unwrap();
    // The client tool is untouched: a turn that may search may still call
    // everything the host advertised.
    assert_eq!(tools[0]["name"], "read_file");
    assert_eq!(
        tools[1],
        json!({
            "type": "web_search_20260209",
            "name": "web_search",
            "max_uses": 3,
            // The last tool carries the cache breakpoint, which is settled
            // after the whole array is built.
            "cache_control": {"type": "ephemeral"},
        })
    );

    // Absent the control, nothing about the request changes.
    let mut plain = search_request("claude-opus-5");
    plain.vendor_web_search = None;
    let body = build_request_json(&plain).unwrap();
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
}

#[test]
fn the_search_tool_version_follows_the_model() {
    for id in [
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-opus-4-6",
        "claude-sonnet-4-6-20260101",
        "claude-opus-4-8",
    ] {
        assert_eq!(web_search_tool_type(id), "web_search_20260209", "{id}");
    }
    for id in [
        // Haiku is the small tier in every generation.
        "claude-haiku-4-5-20251001",
        "claude-haiku-5",
        // Older than the current revision.
        "claude-opus-4-5",
        "claude-3-5-sonnet-20241022",
        // No readable generation: the basic tool is the one every
        // search-capable model accepts.
        "some-gateway-alias",
    ] {
        assert_eq!(web_search_tool_type(id), "web_search_20250305", "{id}");
    }
}

#[test]
fn a_prior_client_side_search_is_replayed_under_another_name() {
    // The request declares a *server* tool called `web_search`. Replaying
    // client tool_use blocks under that same name is undefined, so history
    // goes back renamed; results pair by id and are untouched.
    let mut req = search_request("claude-opus-5");
    req.messages.push(ChatMessage {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "web_search".into(),
                input: json!({"query": "yesterday"}),
            },
            ContentBlock::ToolUse {
                id: "toolu_2".into(),
                name: "read_file".into(),
                input: json!({"path": "a"}),
            },
        ],
        reasoning: MessageReasoning::default(),
    });
    req.messages.push(ChatMessage {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: "{}".into(),
            is_error: false,
        }],
        reasoning: MessageReasoning::default(),
    });

    let body = build_request_json(&req).unwrap();
    let assistant = &body["messages"][1]["content"];
    assert_eq!(assistant[0]["name"], "web_search_prior");
    assert_eq!(assistant[0]["id"], "toolu_1");
    assert_eq!(assistant[0]["input"], json!({"query": "yesterday"}));
    assert_eq!(assistant[1]["name"], "read_file");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "toolu_1");

    // Without the vendor tool there is no collision, so the name stands.
    req.vendor_web_search = None;
    let body = build_request_json(&req).unwrap();
    assert_eq!(body["messages"][1]["content"][0]["name"], "web_search");
}

#[test]
fn a_provider_executed_call_without_native_replay_becomes_cleartext_prose() {
    // No same-route native blocks — foreign provider, missing capture, or
    // truncated history — so the call goes back as titles/URLs the next
    // model can still use.
    let mut req = search_request("claude-opus-5");
    req.messages.push(ChatMessage {
        role: Role::Assistant,
        content: vec![ContentBlock::ProviderExecutedToolCall {
            name: "web_search".into(),
            input: json!({"query": "rust 2027"}),
            output: json!({
                "provider": "anthropic",
                "results": [{"title": "A", "url": "https://a"}]
            }),
            is_error: false,
            replay: None,
        }],
        reasoning: MessageReasoning::default(),
    });
    let body = build_request_json(&req).unwrap();
    let block = &body["messages"][1]["content"][0];
    assert_eq!(block["type"], "text");
    assert_eq!(
        block["text"],
        "[web_search: rust 2027 -> 1 results]\n- A — https://a"
    );
}

#[test]
fn a_same_route_provider_executed_call_replays_native_blocks() {
    let origin = search_origin("claude-opus-5");
    let native = vec![
        json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": {"query": "rust 2027"},
        }),
        json!({
            "type": "web_search_tool_result",
            "tool_use_id": "srvtoolu_1",
            "content": [{
                "type": "web_search_result",
                "url": "https://a",
                "title": "A",
                "encrypted_content": "opaque",
            }],
        }),
    ];
    let mut req = search_request("claude-opus-5");
    req.messages.push(ChatMessage {
        role: Role::Assistant,
        content: vec![ContentBlock::ProviderExecutedToolCall {
            name: "web_search".into(),
            input: json!({"query": "rust 2027"}),
            output: json!({
                "provider": "anthropic",
                "results": [{"title": "A", "url": "https://a", "snippet": ""}]
            }),
            is_error: false,
            replay: Some(ProviderToolReplay::captured(origin, native.clone())),
        }],
        reasoning: MessageReasoning::default(),
    });
    let body = build_request_json(&req).unwrap();
    let content = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0], native[0]);
    assert_eq!(content[1]["type"], "web_search_tool_result");
    assert_eq!(
        content[1]["content"][0]["encrypted_content"], "opaque",
        "native encrypted content survives encoding"
    );
    // The transcript-tail cache breakpoint lands on the last block; that
    // is orthogonal to search replay and must not strip the result.
    assert_eq!(content[1]["cache_control"], json!({"type": "ephemeral"}));

    // A different model on the same provider cannot take those blocks.
    req.model = "claude-sonnet-5".into();
    let body = build_request_json(&req).unwrap();
    assert_eq!(body["messages"][1]["content"][0]["type"], "text");
    assert!(body["messages"][1]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("https://a"));
}

/// The frames Anthropic sends for one completed server-side search.
fn search_frames(result_content: Value) -> Vec<Value> {
    vec![
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""},
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Let me check."},
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {
                "type": "server_tool_use",
                "id": "srvtoolu_1",
                "name": "web_search",
                "input": {},
            },
        }),
        json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"query\":\"rus"},
        }),
        json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "t 2027\"}"},
        }),
        json!({"type": "content_block_stop", "index": 1}),
        json!({
            "type": "content_block_start",
            "index": 2,
            "content_block": {
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_1",
                "content": result_content,
            },
        }),
        json!({"type": "content_block_stop", "index": 2}),
    ]
}

#[test]
fn a_completed_vendor_search_becomes_one_provider_executed_call() {
    let mut frames = search_frames(json!([
        {
            "type": "web_search_result",
            "url": "https://example.com/a",
            "title": "A",
            "encrypted_content": "opaque-and-enormous",
            "page_age": "April 30, 2026",
        },
        {
            "type": "web_search_result",
            "url": "https://example.com/b",
            "title": "B",
            "encrypted_content": "opaque",
        },
        // No url: nothing citable, so nothing to report.
        {"type": "web_search_result", "title": "C"},
    ]));
    frames.push(json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}));
    let origin = search_origin("claude-opus-5");
    let out = run_with_origin(&frames, Some(origin.clone()));

    let ProviderEvent::ProviderExecutedToolCall {
        name,
        input,
        output,
        is_error,
        replay,
    } = &out[1]
    else {
        panic!("expected provider-executed search: {out:?}");
    };
    assert_eq!(name, "web_search");
    assert_eq!(input, &json!({"query": "rust 2027"}));
    assert_eq!(
        output,
        &json!({
            "provider": "anthropic",
            "results": [
                {
                    "url": "https://example.com/a",
                    "title": "A",
                    "snippet": "",
                    "metadata": {"page_age": "April 30, 2026"},
                },
                {"url": "https://example.com/b", "title": "B", "snippet": ""},
            ],
        })
    );
    assert!(!*is_error);
    let replay = replay.as_ref().expect("native replay was captured");
    assert_eq!(replay.origin(), Some(&origin));
    assert_eq!(
        replay.blocks()[0],
        json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": {"query": "rust 2027"},
        })
    );
    assert_eq!(
        replay.blocks()[1]["content"][0]["encrypted_content"],
        "opaque-and-enormous"
    );
    assert!(matches!(
        out.last(),
        Some(ProviderEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
}

#[test]
fn a_failed_vendor_search_is_reported_rather_than_dropped() {
    // The result content is a single error object here, not an array.
    // Indexing it as one would panic or silently report zero results.
    let frames = search_frames(json!({
        "type": "web_search_tool_result_error",
        "error_code": "max_uses_exceeded",
    }));
    let out = run_with_origin(&frames, Some(search_origin("claude-opus-5")));
    let ProviderEvent::ProviderExecutedToolCall {
        name,
        input,
        output,
        is_error,
        replay,
    } = out.last().unwrap()
    else {
        panic!("expected failed search: {out:?}");
    };
    assert_eq!(name, "web_search");
    assert_eq!(input, &json!({"query": "rust 2027"}));
    assert_eq!(output, &json!({"error_code": "max_uses_exceeded"}));
    assert!(*is_error);
    assert!(replay.is_some());

    // A shape this adapter has not seen is still a search that ran.
    let out = run(&search_frames(json!("surprise")));
    assert!(matches!(
        out.last().unwrap(),
        ProviderEvent::ProviderExecutedToolCall { is_error: true, .. }
    ));
}

#[tokio::test]
async fn a_paused_turn_is_resumed_inside_the_adapter() {
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use std::sync::{Arc, Mutex};

    fn sse(frames: &[Value]) -> String {
        frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect()
    }

    #[derive(Clone, Default)]
    struct Script(Arc<Mutex<Vec<Value>>>);

    async fn respond(
        State(script): State<Script>,
        axum::Json(body): axum::Json<Value>,
    ) -> impl IntoResponse {
        let mut seen = script.0.lock().unwrap();
        seen.push(body);
        let leg = seen.len();
        let mut frames = search_frames(json!([{
            "type": "web_search_result",
            "url": "https://example.com/a",
            "title": "A",
            "encrypted_content": "opaque",
        }]));
        if leg == 1 {
            // Thinking must stay first in the assistant content, so make
            // room ahead of the search blocks before streaming a split
            // signature through the raw pause-turn capture path.
            for frame in &mut frames {
                if let Some(index) = frame.get_mut("index") {
                    *index = json!(index.as_u64().unwrap() + 1);
                }
            }
            frames.splice(
                0..0,
                [
                    json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "thinking", "thinking": ""},
                    }),
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "thinking_delta", "thinking": "check the source"},
                    }),
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "signature_delta", "signature": "signed-"},
                    }),
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "signature_delta", "signature": "tail"},
                    }),
                    json!({"type": "content_block_stop", "index": 0}),
                ],
            );
        }
        // The first response pauses mid-turn; the second finishes it.
        frames.push(json!({
            "type": "message_delta",
            "delta": {"stop_reason": if leg == 1 { "pause_turn" } else { "end_turn" }},
            "usage": {"output_tokens": 5},
        }));
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            sse(&frames),
        )
    }

    let script = Script::default();
    let app = Router::new()
        .fallback(post(respond))
        .with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let provider = AnthropicProvider::new("key").with_base_url(format!("http://{address}"));
    let events: Vec<ProviderEvent> = provider
        .stream(search_request("claude-opus-5"))
        .await
        .unwrap()
        .collect()
        .await;
    server.abort();

    // Two requests, one turn: the pause never reaches the consumer, and
    // exactly one stop closes the stream.
    let requests = script.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ProviderEvent::Stop { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.last(),
        Some(ProviderEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
    // Both legs' searches surface.
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ProviderEvent::ProviderExecutedToolCall { .. }))
            .count(),
        2
    );

    // The continuation replays the paused response verbatim — encrypted
    // search content included, because that is what the provider resumes
    // against — with no user message invented to carry it.
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["role"], "assistant");
    let blocks = messages[1]["content"].as_array().unwrap();
    assert_eq!(
        blocks[0],
        json!({
            "type": "thinking",
            "thinking": "check the source",
            "signature": "signed-tail",
        }),
        "the raw continuation preserves the complete split signature"
    );
    assert_eq!(blocks[1], json!({"type": "text", "text": "Let me check."}));
    assert_eq!(
        blocks[2],
        json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": {"query": "rust 2027"},
        })
    );
    assert_eq!(blocks[3]["type"], "web_search_tool_result");
    assert_eq!(
        blocks[3]["content"][0]["encrypted_content"], "opaque",
        "the provider validates the resumed turn against what it sent"
    );
}

#[tokio::test]
async fn a_conversation_is_declared_to_a_gateway_and_withheld_from_anthropic() {
    use axum::extract::State;
    use axum::http::{header, HeaderMap};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<HeaderMap>>>);

    async fn capture(State(capture): State<Capture>, headers: HeaderMap) -> impl IntoResponse {
        capture.0.lock().unwrap().push(headers);
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        )
    }

    let capture_state = Capture::default();
    let app = Router::new()
        .fallback(post(capture))
        .with_state(capture_state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base_url = format!("http://{address}");

    let conversation = tidebreak_core::id::SessionId::new();
    let request = || ChatRequest {
        model: "claude-opus-4-8".into(),
        conversation: Some(conversation),
        messages: vec![ChatMessage::text(Role::User, "hi")],
        ..Default::default()
    };

    let gateway = AnthropicProvider::new("token")
        .with_base_url(&base_url)
        .with_conversation_attribution();
    let mut stream = gateway.stream(request()).await.unwrap();
    while stream.next().await.is_some() {}

    // Same request, same conversation, an adapter that was not configured
    // for a gateway: how the host groups its chats is not Anthropic's.
    let direct = AnthropicProvider::new("key").with_base_url(&base_url);
    let mut stream = direct.stream(request()).await.unwrap();
    while stream.next().await.is_some() {}

    let requests = capture_state.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]
            .get(crate::router::GATEWAY_CONVERSATION_HEADER)
            .unwrap(),
        conversation.to_string().as_str()
    );
    assert!(requests[1]
        .get(crate::router::GATEWAY_CONVERSATION_HEADER)
        .is_none());
    server.abort();
}

#[tokio::test]
async fn the_token_source_is_asked_for_the_request_conversation() {
    // The conversation must reach the token source, not just the header:
    // a gateway source mints inside the chat's attestation context, and a
    // token fetched without the conversation would record no observations
    // for the chat's tool calls.
    struct Recording(std::sync::Mutex<Vec<Option<tidebreak_core::id::SessionId>>>);

    #[async_trait::async_trait]
    impl crate::BearerTokenSource for Recording {
        async fn bearer_token(&self) -> tidebreak_core::Result<String> {
            unreachable!("the adapter must ask per conversation");
        }

        async fn bearer_token_for(
            &self,
            conversation: Option<tidebreak_core::id::SessionId>,
        ) -> tidebreak_core::Result<String> {
            self.0.lock().unwrap().push(conversation);
            Ok("mg_at_test".into())
        }
    }

    let source = std::sync::Arc::new(Recording(std::sync::Mutex::new(Vec::new())));
    let provider = AnthropicProvider::new("unused")
        .with_base_url("http://127.0.0.1:9")
        .with_token_source(source.clone());
    let conversation = tidebreak_core::id::SessionId::new();
    // The request itself fails (nothing listens); the token exchange has
    // already happened by then, which is all this asserts.
    let _ = provider
        .stream(ChatRequest {
            model: "claude-opus-4-8".into(),
            conversation: Some(conversation),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            ..Default::default()
        })
        .await;
    assert_eq!(source.0.lock().unwrap().as_slice(), &[Some(conversation)]);
}
