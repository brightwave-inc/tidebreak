use super::*;
use crate::connected_app::{ConnectedApp, ConnectedAppKind};
use crate::id::ConnectedAppId;

fn record(name: &str, kind: ConnectedAppKind, second: i64) -> ConnectedApp {
    let at = DateTime::<Utc>::from_timestamp(1_710_000_000 + second, 0).unwrap();
    ConnectedApp {
        id: ConnectedAppId::new(),
        name: name.to_owned(),
        kind,
        definition: serde_json::json!({ "name": name }),
        created_at: at,
        updated_at: at,
    }
}

#[tokio::test]
async fn replacement_is_kind_scoped_and_preserves_record_identity() {
    let (_dir, store) = temp_store().await;
    let sentry = record("sentry", ConnectedAppKind::McpServer, 0);
    let rest = record("Sentry API", ConnectedAppKind::RestApi, 0);
    store
        .replace_connected_apps(ConnectedAppKind::McpServer, std::slice::from_ref(&sentry))
        .await
        .unwrap();
    store
        .replace_connected_apps(ConnectedAppKind::RestApi, std::slice::from_ref(&rest))
        .await
        .unwrap();

    // An edit under the same id updates in place and keeps `created_at`; a
    // replacement listing only the edited record deletes nothing of another
    // kind.
    let mut edited = sentry.clone();
    edited.definition = serde_json::json!({ "name": "sentry", "command": "sentry-mcp" });
    edited.updated_at = DateTime::<Utc>::from_timestamp(1_710_000_060, 0).unwrap();
    store
        .replace_connected_apps(ConnectedAppKind::McpServer, std::slice::from_ref(&edited))
        .await
        .unwrap();
    let apps = store.list_connected_apps().await.unwrap();
    assert_eq!(apps.len(), 2);
    let stored = apps.iter().find(|app| app.id == sentry.id).unwrap();
    assert_eq!(stored.definition, edited.definition);
    assert_eq!(stored.created_at, sentry.created_at, "identity persists");
    assert!(
        apps.iter().any(|app| app.id == rest.id),
        "kinds are isolated"
    );

    // An empty replacement clears exactly its kind.
    store
        .replace_connected_apps(ConnectedAppKind::McpServer, &[])
        .await
        .unwrap();
    let apps = store.list_connected_apps().await.unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].id, rest.id);

    // A mixed-kind call is a caller bug, refused before any write.
    assert!(store
        .replace_connected_apps(ConnectedAppKind::McpServer, std::slice::from_ref(&rest))
        .await
        .is_err());
}

/// The absorption migration is the hard cut docs/connected-apps.md specifies:
/// persisted MCP servers become `mcp_server` records, stored manifests are
/// re-keyed onto record ids (dropping bindings to servers that no longer
/// exist), every pre-existing grant is dropped, and the absorbed setting row
/// stops being a source of truth.
#[tokio::test]
async fn absorption_migrates_servers_rekeys_manifests_and_drops_grants() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("upgrade.db");
    let url = format!("sqlite://{}?mode=rwc", database.display());
    let conn = Database::connect(&url).await.unwrap();
    // Stop right before the absorption migration by name, not by position:
    // migrations registered after it must not push the cut out from under
    // this test's legacy seed data.
    let before_absorption = u32::try_from(
        migration::Migrator::migrations()
            .iter()
            .position(|migration| migration.name() == "m20260803_000043_add_connected_apps")
            .expect("the absorption migration is registered"),
    )
    .unwrap();
    migration::Migrator::up(&conn, Some(before_absorption))
        .await
        .unwrap();

    let now = DateTime::<Utc>::from_timestamp(1_710_000_000, 0).unwrap();
    let app_id = uuid::Uuid::new_v4();
    let revision_id = uuid::Uuid::new_v4();
    let seed = |sql: &str, values: Vec<sea_orm::Value>| {
        conn.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            sql,
            values,
        ))
    };
    seed(
        "INSERT INTO setting (key, value_json) VALUES ('mcp_servers_v1', ?)",
        vec![serde_json::json!({
            "servers": [
                { "name": "sentry", "command": "sentry-mcp", "enabled": true },
            ],
        })
        .into()],
    )
    .await
    .unwrap();
    seed(
        "INSERT INTO app (id, name, current_revision_id, revision_count, created_at, updated_at) \
         VALUES (?, 'Sentry triage', ?, 1, ?, ?)",
        vec![app_id.into(), revision_id.into(), now.into(), now.into()],
    )
    .await
    .unwrap();
    // One binding survives the cut re-keyed; the one naming a server that is
    // no longer configured is dropped — narrowed, never widened.
    seed(
        "INSERT INTO app_revision \
         (id, app_id, ordinal, manifest_json, byte_len, sha256, created_at) \
         VALUES (?, ?, 1, ?, 10, ?, ?)",
        vec![
            revision_id.into(),
            app_id.into(),
            serde_json::json!({
                "name": "Sentry triage",
                "bindings": [
                    { "server": "sentry", "tools": ["mcp__sentry__list_issues"] },
                    { "server": "gone", "tools": ["mcp__gone__tool"] },
                ],
            })
            .into(),
            vec![0_u8; 32].into(),
            now.into(),
        ],
    )
    .await
    .unwrap();
    seed(
        "INSERT INTO app_grant (app_id, bindings_json, created_at) VALUES (?, ?, ?)",
        vec![
            app_id.into(),
            serde_json::json!([{
                "server": "sentry",
                "tools": ["mcp__sentry__list_issues"],
                "fingerprint": "00".repeat(32),
            }])
            .into(),
            now.into(),
        ],
    )
    .await
    .unwrap();

    migration::Migrator::up(&conn, None).await.unwrap();
    let store = DbStore { conn };

    let apps = store.list_connected_apps().await.unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].kind, ConnectedAppKind::McpServer);
    assert_eq!(apps[0].name, "sentry");
    assert_eq!(
        apps[0].definition,
        serde_json::json!({ "name": "sentry", "command": "sentry-mcp", "enabled": true }),
        "the absorbed definition is the server object verbatim"
    );

    let revision = store
        .get_app_revision(crate::id::AppRevisionId(revision_id))
        .await
        .unwrap()
        .expect("the revision row survives the cut");
    assert_eq!(revision.manifest.bindings.len(), 1);
    assert_eq!(revision.manifest.bindings[0].app, apps[0].id);
    assert_eq!(
        revision.manifest.bindings[0].tools,
        ["mcp__sentry__list_issues"]
    );

    assert!(
        store
            .get_app_grant(crate::id::AppId(app_id))
            .await
            .unwrap()
            .is_none(),
        "pre-existing grants are dropped, not translated"
    );
    assert!(
        store.get_setting("mcp_servers_v1").await.unwrap().is_none(),
        "the absorbed setting row is no longer a source of truth"
    );
}
