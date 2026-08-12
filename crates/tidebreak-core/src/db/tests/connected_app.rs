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
