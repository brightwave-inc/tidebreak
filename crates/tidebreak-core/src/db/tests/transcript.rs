use super::*;

/// Reasoning is journaled as deltas and has no column of its own, so the
/// transcript rebuilds it by matching the payload's variant tag in SQL. That
/// makes the serialized tag a persisted shape rather than an implementation
/// detail: rename the variant and every historical chat silently loses its
/// reasoning while nothing fails to compile.
#[tokio::test]
async fn reasoning_deltas_rebuild_into_the_transcript() {
    use crate::event::AgentEvent;
    use crate::provider::{StopReason, Usage};

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "claude", "question")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            lease_token,
            accepted.available_at,
            accepted.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    for (ordinal, event) in [
        AgentEvent::ReasoningDelta {
            text: "weighing ".into(),
        },
        AgentEvent::TextDelta {
            text: "not reasoning".into(),
        },
        AgentEvent::ReasoningDelta {
            text: "two approaches".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_turn_event(
                chat.id,
                turn_id,
                lease_token,
                i32::try_from(ordinal).unwrap() + 1,
                accepted.available_at,
                &event,
            )
            .await
            .unwrap()
            .expect("a live attempt may journal its own deltas");
    }
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "the answer".into(),
        llm_content: None,
        created_at: accepted.available_at,
    };
    store
        .complete_turn_run_and_append_event(
            turn_id,
            lease_token,
            0,
            output.created_at,
            &output,
            0,
            Usage::default(),
            StopReason::EndTurn,
        )
        .await
        .unwrap()
        .expect("live completion");

    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    let terminal = transcript
        .terminal_turns
        .first()
        .expect("completed turn remains in the transcript");
    assert_eq!(
        (
            terminal.turn_id,
            terminal.message_id,
            &terminal.status,
            terminal.partial_content.as_str(),
            terminal.reasoning.as_str(),
            terminal.refusal.as_ref(),
            terminal.failure_kind.as_ref(),
        ),
        (
            turn_id,
            Some(output.id),
            &crate::storage::ChatTerminalTurnStatus::Completed,
            "",
            "weighing two approaches",
            None,
            None,
        ),
        "one turn's deltas rebuild beside the message it produced"
    );
    assert!(terminal.finished_at >= output.created_at);
}

/// Regression for #1220: failed and cancelled turns have no assistant message,
/// but their visible stream is still durable journal data.
#[tokio::test]
async fn message_less_terminal_turns_rebuild_partial_text_and_reasoning() {
    use crate::event::AgentEvent;
    use crate::provider::Usage;

    let (_dir, store) = temp_store().await;

    let cancelled_chat = sample_chat();
    store.create_chat(&cancelled_chat).await.unwrap();
    let cancelled_id = TurnId::new();
    let cancelled = match store
        .accept_turn(cancelled_id, cancelled_chat.id, "claude", "stop this")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let cancelled_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            cancelled_lease,
            cancelled.available_at,
            cancelled.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    for (ordinal, event) in [
        AgentEvent::ReasoningDelta {
            text: "considering cancellation".into(),
        },
        AgentEvent::TextDelta {
            text: "partial cancelled answer".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_turn_event(
                cancelled_chat.id,
                cancelled_id,
                cancelled_lease,
                i32::try_from(ordinal).unwrap() + 1,
                cancelled.available_at,
                &event,
            )
            .await
            .unwrap()
            .expect("a live attempt may journal its visible stream");
    }
    let cancellation_requested_at = cancelled.available_at + chrono::Duration::seconds(1);
    store
        .request_turn_cancellation(cancelled_id, cancellation_requested_at)
        .await
        .unwrap()
        .expect("running cancellation is accepted");
    store
        .finish_turn_cancellation_and_append_event(
            cancelled_id,
            cancelled_lease,
            cancellation_requested_at + chrono::Duration::seconds(1),
            0,
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .expect("worker acknowledges cancellation");

    let failed_chat = sample_chat();
    store.create_chat(&failed_chat).await.unwrap();
    let failed_id = TurnId::new();
    let failed = match store
        .accept_turn(failed_id, failed_chat.id, "claude", "fail this")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let failed_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            failed_lease,
            failed.available_at,
            failed.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    for (ordinal, event) in [
        AgentEvent::ReasoningDelta {
            text: "considering failure".into(),
        },
        AgentEvent::TextDelta {
            text: "partial failed answer".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_turn_event(
                failed_chat.id,
                failed_id,
                failed_lease,
                i32::try_from(ordinal).unwrap() + 1,
                failed.available_at,
                &event,
            )
            .await
            .unwrap()
            .expect("a live attempt may journal its visible stream");
    }
    store
        .record_turn_run_failure_and_append_event(
            failed_id,
            failed_lease,
            failed.available_at + chrono::Duration::seconds(1),
            TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "provider",
            Some("internal detail must not cross the renderer boundary"),
        )
        .await
        .unwrap()
        .expect("live failure terminalizes");

    let cancelled_snapshot = store
        .get_chat_transcript(cancelled_chat.id)
        .await
        .unwrap()
        .unwrap()
        .terminal_turns
        .pop()
        .expect("cancelled turn remains in the transcript");
    assert_eq!(
        (
            cancelled_snapshot.status,
            cancelled_snapshot.message_id,
            cancelled_snapshot.partial_content,
            cancelled_snapshot.reasoning,
        ),
        (
            crate::storage::ChatTerminalTurnStatus::Cancelled,
            None,
            "partial cancelled answer".into(),
            "considering cancellation".into(),
        )
    );

    let failed_snapshot = store
        .get_chat_transcript(failed_chat.id)
        .await
        .unwrap()
        .unwrap()
        .terminal_turns
        .pop()
        .expect("failed turn remains in the transcript");
    assert_eq!(
        (
            failed_snapshot.status,
            failed_snapshot.message_id,
            failed_snapshot.partial_content,
            failed_snapshot.reasoning,
            failed_snapshot.failure_kind,
        ),
        (
            crate::storage::ChatTerminalTurnStatus::Failed,
            None,
            "partial failed answer".into(),
            "considering failure".into(),
            Some("provider".into()),
        )
    );
}

/// A cancellation acknowledged with partial output commits it as the turn's
/// durable assistant message in the same transition (#1182): the transcript
/// serves the prose from the message row rather than rebuilding journal
/// deltas, and the next turn's context (built from message rows) includes it.
#[tokio::test]
async fn cancellation_with_partial_output_commits_a_durable_message() {
    use crate::event::AgentEvent;
    use crate::provider::Usage;
    use crate::{AssistantCitationInput, CitationLocator};

    let (_dir, store) = temp_store().await;

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "claude", "stop this")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            lease,
            accepted.available_at,
            accepted.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    store
        .append_turn_event(
            chat.id,
            turn_id,
            lease,
            1,
            accepted.available_at,
            &AgentEvent::TextDelta {
                text: "the answer so far".into(),
            },
        )
        .await
        .unwrap()
        .expect("a live attempt may journal its visible stream");
    let requested_at = accepted.available_at + chrono::Duration::seconds(1);
    store
        .request_turn_cancellation(turn_id, requested_at)
        .await
        .unwrap()
        .expect("running cancellation is accepted");

    let document_id = DocumentId::new();
    store
        .upsert_document(&DocumentUpsert {
            id: document_id,
            project_id: None,
            chat_id: Some(chat.id),
            origin_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "the cited answer".into(),
            updated_at: requested_at,
        })
        .await
        .unwrap();
    let citation = AssistantCitationInput {
        document_id,
        locator: CitationLocator::Lines { start: 1, end: 1 },
    };
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: crate::format_citation_directive(
            "the answer so far",
            document_id,
            &citation.locator,
        ),
        llm_content: None,
        created_at: requested_at,
    };
    store
        .finish_turn_cancellation_and_append_event(
            turn_id,
            lease,
            requested_at + chrono::Duration::seconds(1),
            0,
            Usage::default(),
            Some(&output),
            std::slice::from_ref(&citation),
        )
        .await
        .unwrap()
        .expect("worker acknowledges cancellation with output");

    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    let snapshot = transcript
        .terminal_turns
        .last()
        .expect("cancelled turn remains in the transcript");
    assert_eq!(
        (
            snapshot.status.clone(),
            snapshot.message_id,
            snapshot.partial_content.as_str(),
        ),
        (
            crate::storage::ChatTerminalTurnStatus::Cancelled,
            Some(output.id),
            "",
        ),
        "the committed message, not the journal rebuild, carries the prose"
    );
    let committed = transcript
        .messages
        .iter()
        .find(|message| message.id == output.id)
        .expect("partial output is a durable message");
    assert_eq!(
        (committed.role, committed.content.as_str()),
        (Role::Assistant, output.content.as_str())
    );

    // An exact retry of the same acknowledgement stays idempotent.
    store
        .finish_turn_cancellation_and_append_event(
            turn_id,
            lease,
            requested_at + chrono::Duration::seconds(2),
            0,
            Usage::default(),
            Some(&output),
            std::slice::from_ref(&citation),
        )
        .await
        .unwrap()
        .expect("exact cancellation retry recovers");
    assert_eq!(
        store
            .list_messages(chat.id)
            .await
            .unwrap()
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .count(),
        1,
        "a retried acknowledgement must not duplicate the output message"
    );

    assert!(
        store
            .finish_turn_cancellation_and_append_event(
                turn_id,
                lease,
                requested_at + chrono::Duration::seconds(3),
                0,
                Usage::default(),
                None,
                &[],
            )
            .await
            .is_err(),
        "a retry may not drop the committed partial output"
    );

    let changed_id = Message {
        id: MessageId::new(),
        ..output.clone()
    };
    assert!(
        store
            .finish_turn_cancellation_and_append_event(
                turn_id,
                lease,
                requested_at + chrono::Duration::seconds(4),
                0,
                Usage::default(),
                Some(&changed_id),
                std::slice::from_ref(&citation),
            )
            .await
            .is_err(),
        "a retry may not substitute another output message identity"
    );

    let changed_content = Message {
        content: crate::format_citation_directive(
            "a different answer",
            document_id,
            &citation.locator,
        ),
        ..output.clone()
    };
    assert!(
        store
            .finish_turn_cancellation_and_append_event(
                turn_id,
                lease,
                requested_at + chrono::Duration::seconds(5),
                0,
                Usage::default(),
                Some(&changed_content),
                std::slice::from_ref(&citation),
            )
            .await
            .is_err(),
        "a retry may not change the committed partial output content"
    );

    let changed_citation = AssistantCitationInput {
        document_id,
        locator: CitationLocator::Lines { start: 1, end: 2 },
    };
    let changed_citations = Message {
        content: crate::format_citation_directive(
            "the answer so far",
            document_id,
            &changed_citation.locator,
        ),
        ..output.clone()
    };
    assert!(
        store
            .finish_turn_cancellation_and_append_event(
                turn_id,
                lease,
                requested_at + chrono::Duration::seconds(6),
                0,
                Usage::default(),
                Some(&changed_citations),
                std::slice::from_ref(&changed_citation),
            )
            .await
            .is_err(),
        "a retry may not change the committed partial-output citations"
    );
}

/// Regression for #1714: cancelling during a tool call can leave a committed
/// assistant step even though the turn itself has no output message. Associate
/// the cancellation with the last committed step so its journaled prose is not
/// rendered again as a message-less terminal partial.
#[tokio::test]
async fn cancellation_after_committed_step_does_not_rebuild_its_prose() {
    use crate::event::AgentEvent;
    use crate::provider::Usage;

    let (_dir, store) = temp_store().await;

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "claude", "start then stop")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            lease,
            accepted.available_at,
            accepted.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    store
        .append_turn_event(
            chat.id,
            turn_id,
            lease,
            1,
            accepted.available_at,
            &AgentEvent::TextDelta {
                text: "I will check that now.".into(),
            },
        )
        .await
        .unwrap()
        .expect("a live attempt may journal its visible stream");

    let committed = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "I will check that now.".into(),
        llm_content: None,
        created_at: accepted.available_at,
    };
    assert_eq!(
        store
            .append_claimed_assistant_message_with_citations(
                &committed,
                &[],
                lease,
                accepted.available_at,
            )
            .await
            .unwrap(),
        AppendClaimedMessageOutcome::Appended
    );

    let requested_at = accepted.available_at + chrono::Duration::seconds(1);
    store
        .request_turn_cancellation(turn_id, requested_at)
        .await
        .unwrap()
        .expect("running cancellation is accepted");
    store
        .finish_turn_cancellation_and_append_event(
            turn_id,
            lease,
            requested_at + chrono::Duration::seconds(1),
            0,
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .expect("worker acknowledges cancellation during the tool call");

    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    let snapshot = transcript
        .terminal_turns
        .last()
        .expect("cancelled turn remains in the transcript");
    assert_eq!(
        (
            snapshot.status.clone(),
            snapshot.message_id,
            snapshot.partial_content.as_str(),
        ),
        (
            crate::storage::ChatTerminalTurnStatus::Cancelled,
            Some(committed.id),
            "",
        ),
        "the committed step owns the cancellation instead of being duplicated from journal deltas"
    );
}
