//! The message index: what goes in, what a search reads back, and how the
//! index keeps up with deletes, incognito, and archiving.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, EntityTrait, Statement};

use super::{sample_chat, temp_store};
use crate::code::{Event, ToolDetail, ToolOutcome};
use crate::db::code::{append_event, get_session, search_repo_transcripts};
use crate::db::DbStore;
use crate::id::{MessageId, SessionId, TurnId};
use crate::message_search::{
    MessageSearchCursor, MessageSearchKind, MessageSearchPage, MessageSearchRequest,
    MessageSearchSource,
};
use crate::model::{Message, Role};
use crate::{OwnerId, Store};

async fn search(store: &DbStore, owner: &OwnerId, query: &str) -> MessageSearchPage {
    search_page(store, owner, query, 50, None).await
}

async fn search_page(
    store: &DbStore,
    owner: &OwnerId,
    query: &str,
    limit: u32,
    cursor: Option<MessageSearchCursor>,
) -> MessageSearchPage {
    store
        .search_messages_scoped(
            owner,
            &MessageSearchRequest {
                query: query.into(),
                limit,
                cursor,
            },
        )
        .await
        .unwrap()
}

async fn say(
    store: &DbStore,
    chat: SessionId,
    role: Role,
    content: &str,
    at: DateTime<Utc>,
) -> MessageId {
    let id = MessageId::new();
    store
        .append_message(&Message {
            id,
            chat_id: chat,
            turn_id: TurnId::new(),
            role,
            content: content.into(),
            llm_content: None,
            reasoning: Default::default(),
            created_at: at,
        })
        .await
        .unwrap();
    id
}

/// Every row the index holds for `session`, read straight from the table.
async fn index_rows(store: &DbStore, session: SessionId) -> i64 {
    store
        .conn
        .query_one_raw(Statement::from_sql_and_values(
            sea_orm::DbBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM message_search WHERE session_id = ?",
            [session.0.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

/// The terms the FTS5 table holds, across every session.
async fn fts_rows(store: &DbStore) -> i64 {
    store
        .conn
        .query_one_raw(Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM message_search_fts",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

/// The text a hit's ranges mark in its snippet.
fn marked(page: &MessageSearchPage) -> Vec<Vec<String>> {
    page.hits
        .iter()
        .map(|hit| {
            let units: Vec<u16> = hit.snippet.encode_utf16().collect();
            hit.ranges
                .iter()
                .map(|range| {
                    String::from_utf16(&units[range.start as usize..range.end as usize]).unwrap()
                })
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn chat_messages_match_folded_words_and_the_last_word_as_a_prefix() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let start = Utc::now();
    let question = say(&store, chat.id, Role::User, "Où est le Café?", start).await;
    let answer = say(
        &store,
        chat.id,
        Role::Assistant,
        "The café opens at nine.\nInvoices are due Friday.",
        start + Duration::seconds(1),
    )
    .await;
    // Tool results and system text are not part of what anyone wrote.
    say(
        &store,
        chat.id,
        Role::Tool,
        "tool output secret",
        start + Duration::seconds(2),
    )
    .await;

    let page = search(&store, &owner, "CAFE").await;
    assert_eq!(
        page.hits
            .iter()
            .map(|hit| (hit.message_id, hit.source))
            .collect::<Vec<_>>(),
        [
            (Some(answer), MessageSearchSource::Assistant),
            (Some(question), MessageSearchSource::User),
        ],
        "newest first"
    );
    assert!(page
        .hits
        .iter()
        .all(|hit| hit.kind == MessageSearchKind::Chat
            && hit.session_id == chat.id
            && hit.title.as_deref() == Some("hello")
            && !hit.archived));
    assert_eq!(marked(&page), [vec!["café"], vec!["Café"]]);
    assert!(page.indexing.complete);

    let page = search(&store, &owner, "due invoi").await;
    assert_eq!(page.hits.len(), 1);
    assert_eq!(
        page.hits[0].snippet,
        "The café opens at nine. Invoices are due Friday."
    );
    assert_eq!(marked(&page), [vec!["Invoi", "due"]]);

    assert!(search(&store, &owner, "secret").await.hits.is_empty());
}

#[tokio::test]
async fn query_syntax_is_read_as_literal_words() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    say(
        &store,
        chat.id,
        Role::User,
        "please NOT deploy (yet) near: v2-beta release or wait",
        Utc::now(),
    )
    .await;

    // Each of these would be an FTS5 operator, a syntax error, or a column
    // filter if it reached the match engine as typed. Read as words, each
    // matches exactly when every word it holds is in the message.
    for (query, found) in [
        ("NOT", 1),
        ("\"deploy", 1),
        ("(yet)", 1),
        ("v2-beta", 1),
        ("deploy*", 1),
        ("NEAR(deploy yet)", 1),
        ("release:", 1),
        ("-beta", 1),
        ("NOT (", 1),
        ("yet)) OR ((\"", 1),
        ("deploy OR nothing-here", 0),
        ("AND", 0),
        ("\"", 0),
        ("*", 0),
        (":", 0),
        ("\u{0}", 0),
    ] {
        assert_eq!(
            search(&store, &owner, query).await.hits.len(),
            found,
            "{query}"
        );
    }
}

#[tokio::test]
async fn a_search_reads_only_its_owners_conversations() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("alice").unwrap();
    let bob = OwnerId::new("bob").unwrap();
    let alice_chat = sample_chat();
    let bob_chat = sample_chat();
    store.create_chat_scoped(&alice, &alice_chat).await.unwrap();
    store.create_chat_scoped(&bob, &bob_chat).await.unwrap();
    say(
        &store,
        alice_chat.id,
        Role::User,
        "shared keyword",
        Utc::now(),
    )
    .await;
    say(
        &store,
        bob_chat.id,
        Role::User,
        "shared keyword",
        Utc::now(),
    )
    .await;

    for (owner, chat) in [(&alice, alice_chat.id), (&bob, bob_chat.id)] {
        let page = search(&store, owner, "keyword").await;
        assert_eq!(
            page.hits
                .iter()
                .map(|hit| hit.session_id)
                .collect::<Vec<_>>(),
            [chat]
        );
    }
    let nobody = OwnerId::new("carol").unwrap();
    assert!(search(&store, &nobody, "keyword").await.hits.is_empty());
}

#[tokio::test]
async fn deleting_a_conversation_empties_its_part_of_the_index() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let kept = sample_chat();
    let deleted = sample_chat();
    for chat in [&kept, &deleted] {
        store.create_chat(chat).await.unwrap();
        say(&store, chat.id, Role::User, "gone soon", Utc::now()).await;
    }
    assert_eq!(fts_rows(&store).await, 2);

    store.delete_chat(deleted.id).await.unwrap();

    assert_eq!(index_rows(&store, deleted.id).await, 0);
    assert_eq!(
        fts_rows(&store).await,
        1,
        "the FTS5 terms went with the row"
    );
    let page = search(&store, &owner, "gone").await;
    assert_eq!(
        page.hits
            .iter()
            .map(|hit| hit.session_id)
            .collect::<Vec<_>>(),
        [kept.id]
    );
}

#[tokio::test]
async fn memory_incognito_keeps_a_conversation_out_of_the_index() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    say(&store, chat.id, Role::User, "private plans", Utc::now()).await;
    assert_eq!(search(&store, &owner, "plans").await.hits.len(), 1);

    assert!(store
        .set_chat_memory_incognito(chat.id, true)
        .await
        .unwrap());
    assert_eq!(index_rows(&store, chat.id).await, 0);
    say(&store, chat.id, Role::User, "more plans", Utc::now()).await;
    assert!(search(&store, &owner, "plans").await.hits.is_empty());
    assert_eq!(index_rows(&store, chat.id).await, 0);

    // Leaving incognito brings back everything the conversation holds,
    // including what was said while it was on.
    assert!(store
        .set_chat_memory_incognito(chat.id, false)
        .await
        .unwrap());
    assert_eq!(search(&store, &owner, "plans").await.hits.len(), 2);
}

#[tokio::test]
async fn an_archived_conversation_is_found_and_marked_archived() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    say(&store, chat.id, Role::User, "shelved idea", Utc::now()).await;

    assert!(store
        .set_chat_archived_scoped(&owner, chat.id, true)
        .await
        .unwrap());
    let page = search(&store, &owner, "shelved").await;
    assert_eq!(page.hits.len(), 1);
    assert!(page.hits[0].archived);

    assert!(store
        .set_chat_archived_scoped(&owner, chat.id, false)
        .await
        .unwrap());
    assert!(!search(&store, &owner, "shelved").await.hits[0].archived);
}

#[tokio::test]
async fn pages_follow_the_cursor_newest_first() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let start = Utc::now();
    let mut written = Vec::new();
    for index in 0..5 {
        written.push(
            say(
                &store,
                chat.id,
                Role::User,
                &format!("page entry {index}"),
                start + Duration::seconds(index),
            )
            .await,
        );
    }
    written.reverse();

    let mut read = Vec::new();
    let mut cursor = None;
    for expected in [2, 2, 1] {
        let page = search_page(&store, &owner, "entry", 2, cursor).await;
        assert_eq!(page.hits.len(), expected);
        read.extend(page.hits.iter().map(|hit| hit.message_id.unwrap()));
        cursor = page
            .next_cursor
            .as_deref()
            .map(|cursor| MessageSearchCursor::decode(cursor).unwrap());
    }
    assert_eq!(cursor, None, "the last page names no next page");
    assert_eq!(read, written);
}

fn tool_started(call_id: &str, detail: ToolDetail) -> Event {
    Event::ToolStarted {
        call_id: call_id.into(),
        name: "Tool".into(),
        detail,
        parent_call_id: None,
    }
}

fn tool_completed(call_id: &str, detail: Option<ToolDetail>) -> Event {
    Event::ToolCompleted {
        call_id: call_id.into(),
        outcome: ToolOutcome::Succeeded,
        preview: "tool call preview".into(),
        output: None,
        action: None,
        result: None,
        detail,
        parent_call_id: None,
    }
}

/// BACK-18: the archive search used to match the text of whole journal
/// rows, so a search for a JSON key found every event that carried it. The
/// index holds only what the transcript shows.
#[tokio::test]
async fn code_sessions_index_what_was_said_and_never_journal_keys() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let (session_id, turn_id) = super::code::seed_owner(&store, &owner, "search").await;
    let session = get_session(&store, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    let workspace_id = session.workspace_id.unwrap();
    let repo_id = crate::db::code::get_workspace(&store, &owner, workspace_id)
        .await
        .unwrap()
        .unwrap()
        .repo_id;

    let mut seqs = Vec::new();
    for event in [
        Event::TurnStarted { turn_id },
        tool_started(
            "read-1",
            ToolDetail::FileRead {
                path: "src/lib.rs".into(),
            },
        ),
        tool_completed("read-1", None),
        // A call whose start named nothing is indexed by its completion.
        tool_started(
            "run-1",
            ToolDetail::Other {
                summary: String::new(),
            },
        ),
        tool_completed(
            "run-1",
            Some(ToolDetail::Command {
                cmd: "cargo nextest run".into(),
                cwd: "/repo".into(),
            }),
        ),
        // A completion that restates the call replaces what its start said.
        tool_started(
            "echo-1",
            ToolDetail::Command {
                cmd: "echo partial".into(),
                cwd: "/repo".into(),
            },
        ),
        tool_completed(
            "echo-1",
            Some(ToolDetail::Command {
                cmd: "echo complete".into(),
                cwd: "/repo".into(),
            }),
        ),
        Event::AssistantDelta {
            text: "streamed only".into(),
        },
        Event::AssistantMessage {
            text: "All green.".into(),
            parent_call_id: None,
        },
    ] {
        seqs.push(
            append_event(&store, &owner, session_id, 0, &event)
                .await
                .unwrap(),
        );
    }

    for key in [
        "tool",
        "call",
        "delta",
        "message",
        "type",
        "preview",
        "succeeded",
        "outcome",
        "cwd",
        "repo",
    ] {
        assert!(
            search(&store, &owner, key).await.hits.is_empty(),
            "{key} is a journal key or value, not something anyone wrote"
        );
        assert!(
            search_repo_transcripts(&store, &owner, repo_id, key, 50)
                .await
                .unwrap()
                .matches
                .is_empty(),
            "the archive search matched {key}"
        );
    }
    assert!(search(&store, &owner, "streamed").await.hits.is_empty());
    assert!(search(&store, &owner, "partial").await.hits.is_empty());

    let expect = |hits: &MessageSearchPage, source, seq: Option<i64>| {
        assert_eq!(hits.hits.len(), 1);
        let hit = &hits.hits[0];
        assert_eq!(hit.kind, MessageSearchKind::Code);
        assert_eq!(hit.session_id, session_id);
        assert_eq!(hit.workspace_id, Some(workspace_id));
        assert_eq!(
            hit.title.as_deref(),
            Some("first"),
            "the workspace names it"
        );
        assert_eq!(hit.source, source);
        assert_eq!(hit.event_seq, seq);
        assert_eq!(hit.turn_id, Some(turn_id));
    };
    expect(
        &search(&store, &owner, "lib.rs").await,
        MessageSearchSource::Tool,
        Some(seqs[1]),
    );
    expect(
        &search(&store, &owner, "nextest").await,
        MessageSearchSource::Tool,
        Some(seqs[4]),
    );
    expect(
        &search(&store, &owner, "complete").await,
        MessageSearchSource::Tool,
        Some(seqs[6]),
    );
    expect(
        &search(&store, &owner, "green").await,
        MessageSearchSource::Assistant,
        Some(seqs[8]),
    );
    expect(
        &search(&store, &owner, "hello").await,
        MessageSearchSource::User,
        None,
    );

    let archive = search_repo_transcripts(&store, &owner, repo_id, "green", 50)
        .await
        .unwrap();
    assert_eq!(archive.matches.len(), 1);
    assert_eq!(archive.matches[0].preview, "All green.");
}

#[tokio::test]
async fn deleting_a_workspace_session_empties_its_part_of_the_index() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let (session_id, _) = super::code::seed_owner(&store, &owner, "gone").await;
    append_event(
        &store,
        &owner,
        session_id,
        0,
        &Event::AssistantMessage {
            text: "soon deleted".into(),
            parent_call_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(index_rows(&store, session_id).await, 2);

    let transaction = sea_orm::TransactionTrait::begin(&store.conn).await.unwrap();
    crate::db::ops::code::delete_session_dependents_on(&transaction, session_id)
        .await
        .unwrap();
    crate::db::entities::session::Entity::delete_by_id(session_id.0)
        .exec(&transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    assert_eq!(index_rows(&store, session_id).await, 0);
    assert_eq!(fts_rows(&store).await, 0);
    assert!(search(&store, &owner, "deleted").await.hits.is_empty());
}

#[tokio::test]
async fn the_backfill_adds_a_session_and_the_page_says_so_until_it_has() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    say(&store, chat.id, Role::User, "written long ago", Utc::now()).await;
    // Forget the conversation the way an index that predates it would, and
    // queue it the way the migration does.
    for sql in [
        "DELETE FROM message_search",
        "INSERT INTO message_search_backfill (session_id) SELECT id FROM session WHERE true",
    ] {
        store.conn.execute_unprepared(sql).await.unwrap();
    }
    let page = search(&store, &owner, "long").await;
    assert!(page.hits.is_empty());
    assert!(!page.indexing.complete);
    assert_eq!(page.indexing.pending_conversations, 1);

    assert_eq!(store.backfill_message_search(8).await.unwrap(), 0);

    let page = search(&store, &owner, "long").await;
    assert_eq!(page.hits.len(), 1);
    assert!(page.indexing.complete);
    assert_eq!(page.indexing.pending_conversations, 0);
    // A second pass has nothing to do and changes nothing.
    assert_eq!(store.backfill_message_search(8).await.unwrap(), 0);
    assert_eq!(index_rows(&store, chat.id).await, 1);
}
