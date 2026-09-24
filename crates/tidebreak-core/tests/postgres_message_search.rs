#![cfg(feature = "postgres")]

//! The message index on PostgreSQL, the backend a shared deployment runs.
//!
//! The SQLite suite in `db::tests::message_search` covers the same rules, but
//! the two backends index differently (FTS5 there, a `tsvector` under GIN
//! here), so each rule a person relies on is driven here too: literal
//! queries, folded words and prefixes, owner scoping, delete consistency,
//! incognito, and the archive search's refusal to match journal keys.
//!
//! Skipped silently without `TIDEBREAK_POSTGRES_TEST_URL`, and hard-failed in
//! CI, where `TIDEBREAK_REQUIRE_POSTGRES_TEST` is set.

use chrono::Utc;
use sea_orm::{ConnectionTrait, Database};
use tidebreak_core::db::code::{
    append_event, insert_repo, insert_session, insert_turn, insert_workspace,
    search_repo_transcripts,
};
use tidebreak_core::{
    Attention, AttentionSource, Chat, CodeRepo, CodeWorkspace, CodeWorkspaceStatus, DbStore,
    Event, HarnessKind, Message, MessageId, MessageSearchPage, MessageSearchRequest,
    MessageSearchSource, OwnerId, PermissionMode, RepoId, Role, Session, SessionId, SessionKind,
    SessionLifecycle, Store, ToolDetail, Turn, TurnId, TurnStatus, WorkspaceId,
};

static POSTGRES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A fresh database beside the configured one, migrated, or `None` when no
/// PostgreSQL is configured.
async fn fresh_store(suffix: &str) -> Option<(DbStore, String, String)> {
    let url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return None,
    };
    let suffix = format!("{suffix}_{}", uuid::Uuid::new_v4().simple());
    let (prefix, database, query) = split_postgres_url(&url);
    let head: String = database
        .chars()
        .take(62usize.saturating_sub(suffix.len()))
        .collect();
    let name = format!("{head}_{suffix}");
    let admin = Database::connect(&url).await.unwrap();
    admin
        .execute_unprepared(&format!("CREATE DATABASE \"{name}\""))
        .await
        .unwrap();
    admin.close().await.unwrap();
    let store = DbStore::connect(&format!("{prefix}{name}{query}"))
        .await
        .unwrap();
    Some((store, url, name))
}

async fn drop_database(url: &str, name: &str) {
    let admin = Database::connect(url).await.unwrap();
    admin
        .execute_unprepared(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
        .await
        .unwrap();
    admin.close().await.unwrap();
}

fn split_postgres_url(url: &str) -> (&str, &str, &str) {
    let (base, query) = match url.find('?') {
        Some(at) => url.split_at(at),
        None => (url, ""),
    };
    let at = base
        .rfind('/')
        .expect("TIDEBREAK_POSTGRES_TEST_URL names a database");
    (&base[..=at], &base[at + 1..], query)
}

fn chat(title: &str) -> Chat {
    Chat {
        id: SessionId::new(),
        project_id: None,
        title: Some(title.into()),
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    }
}

async fn say(store: &DbStore, chat: SessionId, role: Role, content: &str) -> MessageId {
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
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    id
}

async fn search(store: &DbStore, owner: &OwnerId, query: &str) -> MessageSearchPage {
    store
        .search_messages_scoped(
            owner,
            &MessageSearchRequest {
                query: query.into(),
                limit: 50,
                cursor: None,
            },
        )
        .await
        .unwrap()
}

/// Seed one owner's repository, workspace, code session, and first turn.
async fn seed_code_session(store: &DbStore, owner: &OwnerId) -> (RepoId, SessionId, TurnId) {
    let repo_id = RepoId::new();
    insert_repo(
        store,
        &CodeRepo {
            id: repo_id,
            owner: owner.clone(),
            root_path: "/srv/search-repo".into(),
            display_name: "search".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: Utc::now(),
            removed_at: None,
            cloned_from: None,
            origin_host: None,
            origin_owner: None,
            origin_name: None,
        },
    )
    .await
    .unwrap();
    let workspace_id = WorkspaceId::new();
    insert_workspace(
        store,
        &CodeWorkspace {
            id: workspace_id,
            owner: owner.clone(),
            repo_id,
            title: "first".into(),
            worktree_path: "/srv/search-worktree".into(),
            branch_name: "tidebreak/search".into(),
            base_ref: "main".into(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
            setup_error: None,
        },
    )
    .await
    .unwrap();
    let session_id = SessionId::new();
    insert_session(
        store,
        &Session {
            visibility: tidebreak_core::SessionVisibility::Private,
            id: session_id,
            owner: owner.clone(),
            owner_kind: None,
            workspace_id: Some(workspace_id),
            kind: SessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: Some("2.1.233".into()),
            harness_resume_ref: None,
            permission_mode: PermissionMode::Ask,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: SessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
            execution_location: tidebreak_core::ExecutionLocation::Machine,
            acts_as: None,
        },
    )
    .await
    .unwrap();
    let turn_id = TurnId::new();
    insert_turn(
        store,
        owner,
        &Turn {
            actor: None,
            id: turn_id,
            session_id,
            ordinal: 1,
            status: TurnStatus::Running,
            model: None,
            fast_mode: false,
            user_input: "Rename the Überblick panel".into(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            rewrite: None,
            started_at: Utc::now(),
            ended_at: None,
            park_ref: None,
            park_wait: None,
        },
    )
    .await
    .unwrap();
    (repo_id, session_id, turn_id)
}

#[tokio::test]
async fn postgres_search_reads_literal_words_for_their_owner_only() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Some((store, url, name)) = fresh_store("search").await else {
        return;
    };
    let alice = OwnerId::new("alice").unwrap();
    let bob = OwnerId::new("bob").unwrap();
    let alice_chat = chat("Harbour");
    let bob_chat = chat("Lunch");
    store.create_chat_scoped(&alice, &alice_chat).await.unwrap();
    store.create_chat_scoped(&bob, &bob_chat).await.unwrap();
    let question = say(
        &store,
        alice_chat.id,
        Role::User,
        "please NOT deploy (yet): the Café's v2-beta",
    )
    .await;
    say(&store, alice_chat.id, Role::Tool, "tool output secret").await;
    say(&store, bob_chat.id, Role::User, "deploy the café lunch order").await;

    // Folded words, the last one a prefix, for the owner alone.
    let page = search(&store, &alice, "CAFE depl").await;
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].message_id, Some(question));
    assert_eq!(page.hits[0].session_id, alice_chat.id);
    let units: Vec<u16> = page.hits[0].snippet.encode_utf16().collect();
    let marked: Vec<String> = page.hits[0]
        .ranges
        .iter()
        .map(|range| String::from_utf16(&units[range.start as usize..range.end as usize]).unwrap())
        .collect();
    assert_eq!(marked, ["depl", "Café"]);
    assert_eq!(
        search(&store, &bob, "cafe")
            .await
            .hits
            .iter()
            .map(|hit| hit.session_id)
            .collect::<Vec<_>>(),
        [bob_chat.id]
    );
    assert!(search(&store, &bob, "beta").await.hits.is_empty());
    assert!(search(&store, &alice, "secret").await.hits.is_empty());

    // tsquery syntax is never read as syntax.
    for (query, found) in [
        ("NOT", 1),
        ("(yet)", 1),
        ("v2-beta", 1),
        ("deploy & !yet", 1),
        ("deploy | nothing", 0),
        ("'yet'", 1),
        ("yet:*", 1),
        ("<-> yet", 1),
        ("\\", 0),
        ("AND", 0),
    ] {
        assert_eq!(search(&store, &alice, query).await.hits.len(), found, "{query}");
    }

    // Incognito takes the conversation out of search, and back.
    assert!(store
        .set_chat_memory_incognito_scoped(&alice, alice_chat.id, true)
        .await
        .unwrap());
    assert!(search(&store, &alice, "deploy").await.hits.is_empty());
    assert!(store
        .set_chat_memory_incognito_scoped(&alice, alice_chat.id, false)
        .await
        .unwrap());
    assert_eq!(search(&store, &alice, "deploy").await.hits.len(), 1);

    // Deleting the conversation takes its rows with it.
    store.delete_chat(alice_chat.id).await.unwrap();
    assert!(search(&store, &alice, "deploy").await.hits.is_empty());
    let rows: i64 = store_rows(&url, &name).await;
    assert_eq!(rows, 1, "only Bob's message is left in the index");

    store.close().await.unwrap();
    drop_database(&url, &name).await;
}

/// How many rows the index holds, read on a connection of its own.
async fn store_rows(url: &str, name: &str) -> i64 {
    let (prefix, _, query) = split_postgres_url(url);
    let connection = Database::connect(&format!("{prefix}{name}{query}"))
        .await
        .unwrap();
    let rows = connection
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*)::bigint AS n FROM message_search",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    connection.close().await.unwrap();
    rows
}

/// BACK-18 on PostgreSQL: the archive search matches what was said, not
/// the keys and values of stored journal rows.
#[tokio::test]
async fn postgres_code_search_matches_what_was_said_and_never_journal_keys() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Some((store, url, name)) = fresh_store("code_search").await else {
        return;
    };
    let owner = OwnerId::local();
    let (repo_id, session_id, turn_id) = seed_code_session(&store, &owner).await;
    let mut seqs = Vec::new();
    for event in [
        Event::TurnStarted { turn_id },
        Event::ToolStarted {
            call_id: "read-1".into(),
            name: "Read".into(),
            detail: ToolDetail::FileRead {
                path: "src/panel.rs".into(),
            },
            parent_call_id: None,
        },
        Event::AssistantMessage {
            text: "Renamed it to Overview.".into(),
            parent_call_id: None,
        },
    ] {
        seqs.push(append_event(&store, &owner, session_id, 0, &event).await.unwrap());
    }

    for key in ["tool", "call", "type", "message", "detail", "path", "read"] {
        assert!(
            search(&store, &owner, key).await.hits.is_empty(),
            "{key} is a journal key or value"
        );
        assert!(search_repo_transcripts(&store, &owner, repo_id, key, 50)
            .await
            .unwrap()
            .matches
            .is_empty());
    }
    let page = search(&store, &owner, "panel.rs").await;
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].source, MessageSearchSource::Tool);
    assert_eq!(page.hits[0].event_seq, Some(seqs[1]));
    assert_eq!(page.hits[0].turn_id, Some(turn_id));
    let page = search(&store, &owner, "uberblick").await;
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].source, MessageSearchSource::User);
    let archive = search_repo_transcripts(&store, &owner, repo_id, "overview", 50)
        .await
        .unwrap();
    assert_eq!(archive.matches.len(), 1);
    assert_eq!(archive.matches[0].preview, "Renamed it to Overview.");

    store.close().await.unwrap();
    drop_database(&url, &name).await;
}
