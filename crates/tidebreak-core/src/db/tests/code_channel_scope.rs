use super::temp_store;
use crate::code::{CodeChannelRepositoryState, CodeGrantKind};
use crate::db::code::*;
use crate::db::{entities, DbStore};
use crate::OwnerId;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

async fn grant(store: &DbStore, owner: &OwnerId, kind: CodeGrantKind) -> crate::CodeExternalGrant {
    mint_external_grant(
        store,
        owner,
        MintGrantSubject {
            channel_kind: "slack",
            external_identity: kind.as_str(),
            workspace_identity: "T1",
            kind,
        },
        &uuid::Uuid::new_v4().simple().to_string().repeat(2),
        &uuid::Uuid::new_v4().simple().to_string().repeat(2),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn channel_scope_approves_several_repositories_before_a_task_and_keeps_requests() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::new("service").unwrap();
    let admin = OwnerId::new("admin").unwrap();
    let grant = grant(&store, &owner, CodeGrantKind::Workspace).await;
    let repositories = vec!["acme/first".to_owned(), "acme/second".to_owned()];
    for repo in &repositories {
        ensure_pending_channel_repository(&store, &owner, grant.id, "C1", repo, "U1", "Casey")
            .await
            .unwrap();
    }
    let pending = list_channel_repository_confirms(&store, &owner, grant.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending
        .iter()
        .all(|r| r.state == CodeChannelRepositoryState::Pending));
    assert!(
        approve_channel_repositories(&store, &owner, grant.id, "C1", &repositories, &admin)
            .await
            .unwrap()
    );
    // A second channel can have its whole scope approved before its first request.
    assert!(
        approve_channel_repositories(&store, &owner, grant.id, "C2", &repositories, &admin)
            .await
            .unwrap()
    );
    assert!(approve_channel_repositories(
        &store,
        &owner,
        grant.id,
        "C1",
        &["acme/third".to_owned()],
        &admin
    )
    .await
    .unwrap());
    for channel in ["C1", "C2"] {
        for repo in &repositories {
            assert!(
                channel_repository_is_confirmed(&store, &owner, grant.id, channel, repo)
                    .await
                    .unwrap()
            );
            let retried = ensure_pending_channel_repository(
                &store, &owner, grant.id, channel, repo, "U1", "Casey",
            )
            .await
            .unwrap();
            assert_eq!(retried.state, CodeChannelRepositoryState::Confirmed);
        }
    }
    assert!(
        !channel_repository_is_confirmed(&store, &owner, grant.id, "C2", "acme/third")
            .await
            .unwrap()
    );
    assert!(
        !channel_repository_is_confirmed(&store, &admin, grant.id, "C1", "acme/first")
            .await
            .unwrap()
    );
    assert!(list_channel_repository_confirms(&store, &owner, grant.id)
        .await
        .unwrap()
        .iter()
        .all(|r| r.confirmed_by.as_ref() == Some(&admin)));
}

#[tokio::test]
async fn channel_scope_recovers_superseded_requests_and_refuses_revoked_or_person_grants() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::new("service").unwrap();
    let admin = OwnerId::new("admin").unwrap();
    let grant = grant(&store, &owner, CodeGrantKind::Workspace).await;
    ensure_pending_channel_repository(&store, &owner, grant.id, "C1", "acme/first", "U1", "Casey")
        .await
        .unwrap();
    entities::code_channel_repository_confirm::Entity::update_many()
        .col_expr(
            entities::code_channel_repository_confirm::Column::State,
            sea_orm::sea_query::Expr::value("superseded"),
        )
        .filter(entities::code_channel_repository_confirm::Column::GrantId.eq(grant.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let retried = ensure_pending_channel_repository(
        &store,
        &owner,
        grant.id,
        "C1",
        "acme/first",
        "U2",
        "Jordan",
    )
    .await
    .unwrap();
    assert_eq!(retried.state, CodeChannelRepositoryState::Pending);
    assert_eq!(retried.set_by_identity, "U2");
    let repos = vec!["acme/first".to_owned()];
    let (approve, request) = tokio::join!(
        approve_channel_repositories(&store, &owner, grant.id, "C1", &repos, &admin),
        ensure_pending_channel_repository(
            &store,
            &owner,
            grant.id,
            "C1",
            "acme/first",
            "U1",
            "Casey"
        ),
    );
    assert!(approve.unwrap());
    request.unwrap();
    assert!(
        channel_repository_is_confirmed(&store, &owner, grant.id, "C1", "acme/first")
            .await
            .unwrap()
    );
    revoke_external_grant(&store, &owner, grant.id, "test")
        .await
        .unwrap();
    assert!(
        !approve_channel_repositories(&store, &owner, grant.id, "C1", &repos, &admin)
            .await
            .unwrap()
    );
    assert!(
        confirm_channel_repository(&store, &owner, grant.id, "C1", "acme/first", &admin)
            .await
            .unwrap()
            .is_none()
    );
    let personal = self::grant(&store, &owner, CodeGrantKind::Person).await;
    assert!(
        !approve_channel_repositories(&store, &owner, personal.id, "C1", &repos, &admin)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn channel_scope_preserves_existing_mixed_case_approvals() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::new("service").unwrap();
    let admin = OwnerId::new("admin").unwrap();
    let grant = grant(&store, &owner, CodeGrantKind::Workspace).await;
    ensure_pending_channel_repository(&store, &owner, grant.id, "C1", "ACME/Tools", "U1", "Casey")
        .await
        .unwrap();
    confirm_channel_repository(&store, &owner, grant.id, "C1", "ACME/Tools", &admin)
        .await
        .unwrap()
        .unwrap();
    assert!(
        channel_repository_is_confirmed(&store, &owner, grant.id, "C1", "acme/tools")
            .await
            .unwrap()
    );
    assert!(
        !channel_repository_is_confirmed(&store, &owner, grant.id, "C2", "acme/tools")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn channel_scope_bulk_approval_clears_historical_case_aliases() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::new("service").unwrap();
    let admin = OwnerId::new("admin").unwrap();
    let grant = grant(&store, &owner, CodeGrantKind::Workspace).await;
    ensure_pending_channel_repository(&store, &owner, grant.id, "C1", "ACME/Tools", "U1", "Casey")
        .await
        .unwrap();
    ensure_pending_channel_repository(&store, &owner, grant.id, "C1", "acme/tools", "U2", "Jordan")
        .await
        .unwrap();
    assert_eq!(
        list_channel_repository_confirms(&store, &owner, grant.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(approve_channel_repositories(
        &store,
        &owner,
        grant.id,
        "C1",
        &["acme/tools".to_owned()],
        &admin
    )
    .await
    .unwrap());
    let rows = list_channel_repository_confirms(&store, &owner, grant.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].repository, "acme/tools");
    assert_eq!(rows[0].state, CodeChannelRepositoryState::Confirmed);
    assert_eq!(rows[0].set_by_identity, "U1");
}
