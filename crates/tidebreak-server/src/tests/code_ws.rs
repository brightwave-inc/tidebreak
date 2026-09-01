//! WebSocket replay, live journals, and the updates digest.

use super::code::*;

use std::time::Duration;

use futures::StreamExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CodeEvent, CodeSessionId};
use tidebreak_harness::HarnessEvent;

#[tokio::test(flavor = "multi_thread")]
async fn ws_replays_then_lives_without_gaps_or_duplicates() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta { text: "one".into() },
            HarnessEvent::AssistantDelta { text: "two".into() },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(20)),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();

    let _ = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hi" }))
        .send()
        .await
        .unwrap();

    let mut seqs = Vec::new();
    let mut streamed = String::new();
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            // Deltas ride the live stream without a row, so they repeat the
            // cursor rather than advancing it. The ordering contract is about
            // the journal.
            if value["transient"] == true {
                streamed.push_str(value["event"]["text"].as_str().unwrap());
                continue;
            }
            let seq = value["seq"].as_i64().unwrap();
            seqs.push(seq);
            if value["event"]["type"] == "turn_completed" {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turn did not complete over the socket");
    assert_eq!(streamed, "onetwo", "deltas must still stream live");
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
    assert_eq!(
        seqs.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        seqs.len(),
        "duplicate seq on the live socket: {seqs:?}"
    );

    // Concurrent write after connect: journal a notice and publish a later seq
    // first so the socket must fill the gap from the journal.
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let current = *seqs.last().unwrap();
    let _ = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
        1,
        &tidebreak_core::CodeEvent::HarnessNotice {
            level: tidebreak_core::HarnessNoticeLevel::Info,
            message: "gap-a".into(),
        },
    )
    .await
    .unwrap();
    let seq_b = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
        1,
        &tidebreak_core::CodeEvent::HarnessNotice {
            level: tidebreak_core::HarnessNoticeLevel::Info,
            message: "gap-b".into(),
        },
    )
    .await
    .unwrap();
    runtime.bus.publish(
        parsed,
        tidebreak_core::SequencedCodeEvent {
            seq: seq_b,
            event: tidebreak_core::CodeEvent::HarnessNotice {
                level: tidebreak_core::HarnessNoticeLevel::Info,
                message: "gap-b".into(),
            },
        },
    );
    let mut recovered = Vec::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("gap recovery timed out")
            .expect("socket closed")
            .unwrap();
        let WsMessage::Text(text) = frame else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
        recovered.push(value["seq"].as_i64().unwrap());
    }
    assert_eq!(recovered, vec![current + 1, current + 2]);
}

/// Delta rows are no longer written, but journals that already hold them
/// must still read back. This appends them the way a pre-record-57 session
/// did and replays the result.
#[tokio::test(flavor = "multi_thread")]
async fn ws_replay_emits_every_durable_sequence_after_the_cursor_in_order() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let replay_after_seq = journaled_events(&runtime.db, parsed)
        .await
        .last()
        .map_or(0, |event| event.seq);

    let first_delta_seq = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
        1,
        &tidebreak_core::CodeEvent::AssistantDelta { text: "hel".into() },
    )
    .await
    .unwrap();
    let second_delta_seq = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
        1,
        &tidebreak_core::CodeEvent::AssistantDelta { text: "lo".into() },
    )
    .await
    .unwrap();

    let mut replay_request =
        format!("ws://{addr}/code/sessions/{session_id}/events?after={replay_after_seq}")
            .into_client_request()
            .unwrap();
    replay_request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut replay_socket, _) = connect_async(replay_request).await.unwrap();

    let mut replayed = Vec::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(2), replay_socket.next())
            .await
            .expect("durable replay timed out")
            .expect("replay socket closed")
            .unwrap();
        let WsMessage::Text(text) = frame else {
            continue;
        };
        replayed.push(serde_json::from_str::<serde_json::Value>(text.as_str()).unwrap());
    }
    assert_eq!(
        replayed
            .iter()
            .map(|frame| frame["seq"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![first_delta_seq, second_delta_seq]
    );
    assert_eq!(replayed[0]["event"]["text"], "hel");
    assert_eq!(replayed[1]["event"]["text"], "lo");
    assert_eq!(replayed[0]["replayed"], true);
    assert_eq!(replayed[1]["replayed"], true);
}

/// Record 57: the message states the whole answer, so the deltas that built
/// it are streamed and dropped. A plausible wrong implementation stops
/// publishing them too and passes every durable assertion here.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_journals_its_message_and_none_of_the_deltas_that_built_it() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "half a ".into(),
            },
            HarnessEvent::AssistantDelta {
                text: "sentence".into(),
            },
            HarnessEvent::AssistantMessage {
                text: "half a sentence".into(),
                parent_call_id: None,
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(10)),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();

    let turn = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hi" }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);

    let mut streamed = Vec::new();
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            if value["event"]["type"] == "assistant_delta" {
                assert_eq!(value["transient"], true, "a delta must not claim a row");
                streamed.push(value["event"]["text"].as_str().unwrap().to_owned());
            }
            if value["event"]["type"] == "turn_completed" {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turn did not complete over the socket");
    assert_eq!(streamed, vec!["half a ", "sentence"]);

    let journaled = journaled_events(&runtime.db, parsed).await;
    assert!(
        !journaled
            .iter()
            .any(|framed| matches!(framed.event, CodeEvent::AssistantDelta { .. })),
        "a delta reached the journal: {journaled:?}"
    );
    let messages: Vec<&str> = journaled
        .iter()
        .filter_map(|framed| match &framed.event {
            CodeEvent::AssistantMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(messages, vec!["half a sentence"]);
}

/// Interrupt an answer mid-sentence and the words the reader watched arrive
/// must survive a reload. The engine never sent a message for them, so the
/// server writes the one it owed.
#[tokio::test(flavor = "multi_thread")]
async fn text_streamed_before_an_interrupt_is_written_down() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "thinking out ".into(),
            },
            HarnessEvent::AssistantDelta {
                text: "loud".into(),
            },
            HarnessEvent::AssistantDelta {
                text: " and on".into(),
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(150)),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let (mut events, _) = runtime.bus.attach(parsed);

    let turn_req = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "go" }));
    let interrupt = async {
        // Wait for the first delta so there is streamed text to lose.
        loop {
            let event = events.recv().await.unwrap();
            if matches!(event.event, CodeEvent::AssistantDelta { .. }) {
                break;
            }
        }
        client
            .post(format!(
                "http://{addr}/code/sessions/{session_id}/interrupt"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
    };
    let (turn, interrupted) = tokio::join!(turn_req.send(), interrupt);
    assert_eq!(interrupted.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(turn.unwrap().status(), reqwest::StatusCode::ACCEPTED);

    let recovered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let journaled = journaled_events(&runtime.db, parsed).await;
            let text = journaled.iter().find_map(|framed| match &framed.event {
                CodeEvent::AssistantMessage { text, .. } => Some(text.clone()),
                _ => None,
            });
            if let Some(text) = text {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("streamed text must survive the interrupt");
    assert!(
        recovered.starts_with("thinking out"),
        "expected the streamed text, got {recovered:?}"
    );
}

/// Replay is capped, and the client is told when the cap bit. Silently
/// dropping the head would let a long session open on its middle and read as
/// if that were where it began.
#[tokio::test(flavor = "multi_thread")]
async fn a_capped_replay_tells_the_socket_that_history_was_dropped() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();

    let overflow = tidebreak_core::db::code::MAX_REPLAY_EVENTS + 2;
    let epoch = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap()
    .spawn_epoch;
    for index in 0..overflow {
        tidebreak_core::db::code::append_event(
            &runtime.db,
            &tidebreak_core::OwnerId::local(),
            parsed,
            epoch,
            &tidebreak_core::CodeEvent::HarnessNotice {
                level: tidebreak_core::HarnessNoticeLevel::Info,
                message: format!("notice {index}"),
            },
        )
        .await
        .unwrap();
    }

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("replay timed out")
        .expect("socket closed")
        .unwrap();
    let WsMessage::Text(text) = frame else {
        panic!("expected a text frame");
    };
    let first: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(first["truncated"], true, "{first}");
    // The window keeps the newest events, so it starts a cap's worth below
    // the head of the journal rather than at the cursor the client asked
    // from.
    let newest = journaled_events(&runtime.db, parsed)
        .await
        .last()
        .expect("the session journaled something")
        .seq;
    assert_eq!(
        first["seq"].as_i64().unwrap(),
        newest - tidebreak_core::db::code::MAX_REPLAY_EVENTS as i64 + 1
    );

    let second = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("second frame timed out")
        .expect("socket closed")
        .unwrap();
    let WsMessage::Text(text) = second else {
        panic!("expected a text frame");
    };
    let second: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert!(
        second.get("truncated").is_none(),
        "only the first frame of the window carries the flag: {second}"
    );
}

/// A reader who opens the pane mid-answer must see the sentence from its
/// start, not from wherever they happened to arrive. Nothing durable holds
/// that text yet, so the live tail is the only source.
#[tokio::test(flavor = "multi_thread")]
async fn connecting_mid_answer_replays_the_text_that_already_streamed() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "first ".into(),
            },
            HarnessEvent::AssistantDelta {
                text: "second ".into(),
            },
            HarnessEvent::AssistantDelta {
                text: "third".into(),
            },
            HarnessEvent::AssistantMessage {
                text: "first second third".into(),
                parent_call_id: None,
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(200)),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let (mut events, _) = runtime.bus.attach(parsed);

    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "go" }))
                .send()
                .await
                .unwrap()
        }
    });

    // Let two deltas go by, then connect as a second reader would.
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut seen = 0;
        while seen < 2 {
            if matches!(
                events.recv().await.unwrap().event,
                CodeEvent::AssistantDelta { .. }
            ) {
                seen += 1;
            }
        }
    })
    .await
    .expect("no deltas streamed");

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();

    let mut assembled = String::new();
    let mut finalized = None;
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            match value["event"]["type"].as_str() {
                Some("assistant_delta") => {
                    assembled.push_str(value["event"]["text"].as_str().unwrap());
                }
                Some("assistant_message") => {
                    finalized = Some(value["event"]["text"].as_str().unwrap().to_owned());
                    break;
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("the late reader never saw the message");
    assert_eq!(
        assembled, "first second third",
        "a mid-answer reader must be caught up before the message lands"
    );
    assert_eq!(finalized.as_deref(), Some("first second third"));
    let _ = turn.await;
}

/// A reconnect may keep a prefix and miss later deltas while the socket is
/// down. The server sends the complete tail as a replacement.
#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_mid_answer_replaces_with_the_complete_live_tail() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();

    let epoch = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap()
    .spawn_epoch;
    let marker = CodeEvent::HarnessNotice {
        level: tidebreak_core::HarnessNoticeLevel::Info,
        message: "reconnect cursor".into(),
    };
    let cursor = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
        epoch,
        &marker,
    )
    .await
    .unwrap();
    runtime.bus.publish(
        parsed,
        tidebreak_core::SequencedCodeEvent {
            seq: cursor,
            event: marker,
        },
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            if value["seq"] == cursor {
                break;
            }
        }
    })
    .await
    .expect("the socket did not reach the reconnect cursor");

    runtime.bus.publish_transient(
        parsed,
        CodeEvent::AssistantDelta {
            text: "first ".into(),
        },
    );
    runtime.bus.publish_transient(
        parsed,
        CodeEvent::AssistantDelta {
            text: "second ".into(),
        },
    );

    let mut assembled = String::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a live delta timed out")
            .expect("the live socket closed")
            .unwrap();
        let WsMessage::Text(text) = frame else {
            panic!("expected a text frame");
        };
        let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(value["seq"], cursor);
        assert_eq!(value["transient"], true);
        assembled.push_str(value["event"]["text"].as_str().unwrap());
    }
    assert_eq!(assembled, "first second ");
    drop(socket);

    runtime.bus.publish_transient(
        parsed,
        CodeEvent::AssistantDelta {
            text: "third".into(),
        },
    );

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after={cursor}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut resumed, _) = connect_async(request).await.unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(5), resumed.next())
        .await
        .expect("the replacement tail timed out")
        .expect("the resumed socket closed")
        .unwrap();
    let WsMessage::Text(text) = frame else {
        panic!("expected a text frame");
    };
    let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(value["seq"], cursor);
    assert_eq!(value["event"]["type"], "assistant_delta");
    assert_eq!(value["event"]["text"], "first second third");
    assert_eq!(value["transient"], true);
    assert_eq!(value["replacement"], true);
    assembled = value["event"]["text"].as_str().unwrap().to_owned();

    runtime.bus.publish_transient(
        parsed,
        CodeEvent::AssistantDelta {
            text: " fourth".into(),
        },
    );
    let frame = tokio::time::timeout(Duration::from_secs(5), resumed.next())
        .await
        .expect("the resumed delta timed out")
        .expect("the resumed socket closed")
        .unwrap();
    let WsMessage::Text(text) = frame else {
        panic!("expected a text frame");
    };
    let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(value["event"]["type"], "assistant_delta");
    assert_eq!(value["event"]["text"], " fourth");
    assert!(value.get("replacement").is_none());
    assembled.push_str(value["event"]["text"].as_str().unwrap());
    assert_eq!(assembled, "first second third fourth");
}

#[tokio::test]
async fn superseded_worker_cannot_append_to_the_journal() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id: CodeSessionId = json_id(&session).parse().unwrap();
    let current = session["lifecycle"].as_str().unwrap().to_owned();
    assert_eq!(current, "idle");
    let bumped = tidebreak_core::db::code::bump_spawn_epoch(&runtime.db, session_id, None)
        .await
        .unwrap();
    let err = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session_id,
        bumped - 1,
        &tidebreak_core::CodeEvent::TurnInterrupted,
    )
    .await
    .unwrap_err();
    match err {
        tidebreak_core::db::code::CodeJournalError::StaleSpawnEpoch {
            attempted, current, ..
        } => {
            assert_eq!(attempted, bumped - 1);
            assert_eq!(current, bumped);
        }
        other => panic!("expected stale epoch, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_channel_restates_the_full_digest_on_reconnect() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session);

    let mut request = format!("ws://{addr}/code/updates")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let first = next_json(&mut socket).await;
    assert_eq!(first["type"], "snapshot");
    let sessions = first["sessions"].as_array().expect("snapshot sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session"], session_id);
    assert_eq!(sessions[0]["workspace"], json_id(&workspace));
    assert_eq!(sessions[0]["title"], "first change");
    assert_eq!(sessions[0]["turn_count"], 0);

    let _ = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hi" }))
        .send()
        .await
        .unwrap();

    let mut saw_turn = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let notice = tokio::time::timeout(Duration::from_millis(500), next_json(&mut socket))
            .await
            .ok();
        let Some(notice) = notice else {
            continue;
        };
        if notice["type"] == "digest" && notice["turn_count"] == 1 {
            saw_turn = true;
            break;
        }
    }
    assert!(saw_turn, "live digest must carry the new turn count");
    drop(socket);

    let mut request = format!("ws://{addr}/code/updates")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let restated = next_json(&mut socket).await;
    assert_eq!(restated["type"], "snapshot");
    let sessions = restated["sessions"].as_array().expect("restated sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session"], session_id);
    assert_eq!(sessions[0]["turn_count"], 1);
    assert_eq!(sessions[0]["attention"]["state"]["type"], "done_unreviewed");
}
