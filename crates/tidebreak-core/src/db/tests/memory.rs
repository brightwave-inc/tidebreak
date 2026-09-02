use super::{sample_chat, temp_store};
use crate::memory::{
    MemoryAuthor, MemoryBackend, MemoryError, MemoryEvidence, MemoryKind, MemoryLink,
    MemoryLinkRelation, MemoryListFilter, MemoryOrigin, MemoryProvenance, MemoryRecord,
    MemoryRecordId, MemoryRecordUpdate, MemoryScope, MemorySearchRequest, MemoryStatus,
    MemoryStatusChange,
};
use crate::{Message, MessageId, OwnerId, Role, Store, TurnId};
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

fn at(minute: u32) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&format!("2026-09-01T12:{minute:02}:00Z"))
        .unwrap()
        .with_timezone(&Utc)
}

fn user_record(
    scope: MemoryScope,
    status: MemoryStatus,
    title: &str,
    body: &str,
    minute: u32,
) -> MemoryRecord {
    MemoryRecord {
        id: MemoryRecordId::new(),
        scope,
        kind: MemoryKind::Fact,
        status,
        title: title.to_owned(),
        body: body.to_owned(),
        provenance: MemoryProvenance {
            author: MemoryAuthor::User,
            origin: MemoryOrigin::default(),
            evidence: Vec::new(),
        },
        links: Vec::new(),
        expires_at: None,
        superseded_by: None,
        observation_count: 0,
        revision: 1,
        created_at: at(minute),
        updated_at: at(minute),
    }
}

#[tokio::test]
async fn model_evidence_must_resolve_for_the_same_owner() {
    let (_directory, store) = temp_store().await;
    let alice = OwnerId::new("user:alice").unwrap();
    let bob = OwnerId::new("user:bob").unwrap();
    let chat = sample_chat();
    store.create_chat_scoped(&alice, &chat).await.unwrap();
    let message = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        reasoning: Default::default(),
        content: "Always run the release smoke test.".to_owned(),
        llm_content: None,
        created_at: at(0),
    };
    store.append_message(&message).await.unwrap();

    let mut record = user_record(
        MemoryScope::Personal,
        MemoryStatus::Proposed,
        "When preparing a release",
        "Run the release smoke test before publishing.",
        1,
    );
    record.provenance = MemoryProvenance {
        author: MemoryAuthor::Model,
        origin: MemoryOrigin {
            chat_id: Some(chat.id),
            turn_id: Some(message.turn_id),
            ..Default::default()
        },
        evidence: vec![MemoryEvidence::Message {
            message_id: message.id,
        }],
    };
    store.put(&alice, record.clone()).await.unwrap();

    record.id = MemoryRecordId::new();
    assert_eq!(
        store.put(&bob, record).await,
        Err(MemoryError::EvidenceNotFound(format!(
            "message {}",
            message.id
        )))
    );
}

#[tokio::test]
async fn update_search_digest_history_and_verified_delete_share_one_record() {
    let (_directory, store) = temp_store().await;
    let owner = OwnerId::local();
    let record = user_record(
        MemoryScope::Personal,
        MemoryStatus::Active,
        "When changing database migrations",
        "Run the migration chain test before publishing.",
        2,
    );
    store.put(&owner, record.clone()).await.unwrap();

    let updated = store
        .update(
            &owner,
            MemoryRecordUpdate {
                id: record.id,
                expected_revision: 1,
                kind: MemoryKind::Lesson,
                title: "When changing the database schema".to_owned(),
                body: "Run the stepwise migration test before publishing.".to_owned(),
                provenance: record.provenance.clone(),
                links: Vec::new(),
                expires_at: None,
                observation_count: 0,
            },
        )
        .await
        .unwrap()
        .record;
    assert_eq!(updated.revision, 2);

    let hits = store
        .search(
            &owner,
            MemorySearchRequest {
                query: "stepwise migration".to_owned(),
                scope: Some(MemoryScope::Personal),
                statuses: vec![MemoryStatus::Active],
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].matching_line,
        "Run the stepwise migration test before publishing."
    );

    let first_digest = store
        .assemble_context(&owner, MemoryScope::Personal)
        .await
        .unwrap();
    let second_digest = store
        .assemble_context(&owner, MemoryScope::Personal)
        .await
        .unwrap();
    assert_eq!(first_digest, second_digest);
    let expected_line = format!(
        "{} — When changing the database schema",
        updated.updated_at.format("%Y-%m-%d")
    );
    assert!(
        first_digest.markdown.contains(&expected_line),
        "digest {:?} lacks {expected_line:?}",
        first_digest.markdown
    );
    assert!(!first_digest.markdown.contains("stepwise migration test"));

    let history = store.revision_history(&owner, record.id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].snapshot.body, record.body);
    assert_eq!(history[1].snapshot.body, updated.body);

    assert!(store.delete(&owner, record.id).await.unwrap());
    assert!(store.get(&owner, record.id).await.unwrap().is_none());
    assert!(store
        .revision_history(&owner, record.id)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .search(
            &owner,
            MemorySearchRequest {
                query: "stepwise migration".to_owned(),
                scope: None,
                statuses: Vec::new(),
                limit: 10,
            },
        )
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .assemble_context(&owner, MemoryScope::Personal)
        .await
        .unwrap()
        .markdown
        .is_empty());
}

#[tokio::test]
async fn activating_an_update_proposal_archives_its_source() {
    let (_directory, store) = temp_store().await;
    let owner = OwnerId::local();
    let source = user_record(
        MemoryScope::Personal,
        MemoryStatus::Active,
        "When running checks",
        "Run tests before publishing.",
        3,
    );
    store.put(&owner, source.clone()).await.unwrap();

    let mut proposal = user_record(
        MemoryScope::Personal,
        MemoryStatus::Proposed,
        "When running focused checks",
        "Run formatting and focused tests before publishing.",
        4,
    );
    proposal.links = vec![MemoryLink {
        record_id: source.id,
        relation: MemoryLinkRelation::Updates,
    }];
    store.put(&owner, proposal.clone()).await.unwrap();
    store
        .set_status(
            &owner,
            MemoryStatusChange {
                id: proposal.id,
                expected_revision: 1,
                status: MemoryStatus::Active,
            },
        )
        .await
        .unwrap();

    let archived = store.get(&owner, source.id).await.unwrap().unwrap();
    assert_eq!(archived.status, MemoryStatus::Archived);
    assert_eq!(archived.superseded_by, Some(proposal.id));
    assert_eq!(archived.revision, 2);
    let active = store.get(&owner, proposal.id).await.unwrap().unwrap();
    assert_eq!(active.status, MemoryStatus::Active);
    assert_eq!(active.revision, 2);

    let digest = store
        .assemble_context(&owner, MemoryScope::Personal)
        .await
        .unwrap();
    assert!(digest.markdown.contains(&proposal.title));
    assert!(!digest.markdown.contains(&source.title));
}

#[tokio::test]
async fn an_active_cap_failure_rolls_back_the_record_and_revision() {
    let (_directory, store) = temp_store().await;
    let owner = OwnerId::local();
    let first = user_record(
        MemoryScope::Personal,
        MemoryStatus::Active,
        "First active record",
        "First body.",
        5,
    );
    store.put(&owner, first).await.unwrap();
    crate::db::entities::memory_scope_state::Entity::update_many()
        .col_expr(
            crate::db::entities::memory_scope_state::Column::ActiveRecordCap,
            sea_orm::sea_query::Expr::value(1_i64),
        )
        .filter(crate::db::entities::memory_scope_state::Column::Owner.eq(owner.as_str()))
        .filter(
            crate::db::entities::memory_scope_state::Column::ScopeKind
                .eq(MemoryScope::Personal.kind_str()),
        )
        .exec(&store.conn)
        .await
        .unwrap();

    let second = user_record(
        MemoryScope::Personal,
        MemoryStatus::Active,
        "Second active record",
        "Second body.",
        6,
    );
    assert_eq!(
        store.put(&owner, second.clone()).await,
        Err(MemoryError::ActiveRecordCapExceeded { cap: 1 })
    );
    assert!(store.get(&owner, second.id).await.unwrap().is_none());
    assert!(store
        .revision_history(&owner, second.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .list(&owner, MemoryListFilter::default())
            .await
            .unwrap()
            .len(),
        1
    );
}

/// A merge proposal justifies itself through its superseding links, so the
/// storage layer must reject one whose sources do not resolve rather than
/// store it evidence-less.
#[tokio::test]
async fn a_merge_proposal_with_unresolvable_sources_is_rejected() {
    let (_directory, store) = temp_store().await;
    let owner = OwnerId::local();
    let source = user_record(
        MemoryScope::Personal,
        MemoryStatus::Active,
        "When rebasing",
        "Rebase before trusting green.",
        7,
    );
    store.put(&owner, source.clone()).await.unwrap();

    let missing = MemoryRecordId::new();
    let mut proposal = user_record(
        MemoryScope::Personal,
        MemoryStatus::Proposed,
        "When updating a branch",
        "Rebase onto main and rerun checks before trusting green.",
        8,
    );
    proposal.provenance.author = MemoryAuthor::Model;
    proposal.links = vec![
        MemoryLink {
            record_id: source.id,
            relation: MemoryLinkRelation::Supersedes,
        },
        MemoryLink {
            record_id: missing,
            relation: MemoryLinkRelation::Supersedes,
        },
    ];
    assert_eq!(
        store.put(&owner, proposal.clone()).await,
        Err(MemoryError::InvalidRecord(format!(
            "linked memory record {missing} does not exist"
        )))
    );
    assert!(store.get(&owner, proposal.id).await.unwrap().is_none());
}

/// Approving a merge is one transaction: a source that cannot archive rolls
/// back the sources that could, and the proposal stays proposed.
#[tokio::test]
async fn a_failed_merge_approval_leaves_every_source_untouched() {
    let (_directory, store) = temp_store().await;
    let owner = OwnerId::local();
    let first = user_record(
        MemoryScope::Personal,
        MemoryStatus::Active,
        "When publishing releases",
        "Tag before publishing.",
        9,
    );
    let second = user_record(
        MemoryScope::Personal,
        MemoryStatus::Active,
        "When drafting releases",
        "Draft the notes before tagging.",
        10,
    );
    store.put(&owner, first.clone()).await.unwrap();
    store.put(&owner, second.clone()).await.unwrap();

    let mut proposal = user_record(
        MemoryScope::Personal,
        MemoryStatus::Proposed,
        "When cutting a release",
        "Draft the notes, tag, then publish.",
        11,
    );
    proposal.provenance.author = MemoryAuthor::Model;
    proposal.links = vec![
        MemoryLink {
            record_id: first.id,
            relation: MemoryLinkRelation::Supersedes,
        },
        MemoryLink {
            record_id: second.id,
            relation: MemoryLinkRelation::Supersedes,
        },
    ];
    store.put(&owner, proposal.clone()).await.unwrap();

    // Archive one source after the proposal stored, so approval finds it
    // no longer active and must fail midway.
    store
        .set_status(
            &owner,
            MemoryStatusChange {
                id: second.id,
                expected_revision: 1,
                status: MemoryStatus::Archived,
            },
        )
        .await
        .unwrap();

    let error = store
        .set_status(
            &owner,
            MemoryStatusChange {
                id: proposal.id,
                expected_revision: 1,
                status: MemoryStatus::Active,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, MemoryError::InvalidRecord(_)));

    let untouched = store.get(&owner, first.id).await.unwrap().unwrap();
    assert_eq!(untouched.status, MemoryStatus::Active);
    assert_eq!(untouched.revision, 1);
    assert_eq!(untouched.superseded_by, None);
    assert_eq!(
        store
            .revision_history(&owner, first.id)
            .await
            .unwrap()
            .len(),
        1
    );
    let still_proposed = store.get(&owner, proposal.id).await.unwrap().unwrap();
    assert_eq!(still_proposed.status, MemoryStatus::Proposed);
    assert_eq!(still_proposed.revision, 1);
}
