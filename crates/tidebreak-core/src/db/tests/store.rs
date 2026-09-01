use super::*;

#[tokio::test]
async fn bundled_sqlite_supports_fts5() {
    let (_dir, store) = temp_store().await;
    store
        .conn
        .execute_unprepared("CREATE VIRTUAL TABLE fts_probe USING fts5(content)")
        .await
        .unwrap();
    store
        .conn
        .execute_unprepared("INSERT INTO fts_probe(content) VALUES ('hybrid retrieval')")
        .await
        .unwrap();
    let row = store
        .conn
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT content FROM fts_probe WHERE fts_probe MATCH 'hybrid'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<String>("", "content").unwrap(),
        "hybrid retrieval"
    );
}

#[tokio::test]
async fn settings_roundtrip_and_overwrite() {
    let (_dir, store) = temp_store().await;
    assert_eq!(store.get_setting("model").await.unwrap(), None);
    store
        .set_setting("model", &serde_json::json!("claude"))
        .await
        .unwrap();
    assert_eq!(
        store.get_setting("model").await.unwrap(),
        Some(serde_json::json!("claude"))
    );
    store
        .set_setting("model", &serde_json::json!("gpt"))
        .await
        .unwrap();
    assert_eq!(
        store.get_setting("model").await.unwrap(),
        Some(serde_json::json!("gpt"))
    );
}

#[tokio::test]
async fn all_roles_round_trip() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let roles = [Role::System, Role::User, Role::Assistant, Role::Tool];
    for (i, role) in roles.iter().enumerate() {
        store
            .append_message(&Message {
                id: MessageId::new(),
                chat_id: chat.id,
                turn_id: TurnId::new(),
                role: *role,
                reasoning: Default::default(),
                content: String::new(),
                llm_content: None,
                created_at: DateTime::<Utc>::from_timestamp(i as i64, 0).unwrap(),
            })
            .await
            .unwrap();
    }
    let got: Vec<Role> = store
        .list_messages(chat.id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.role)
        .collect();
    assert_eq!(got, roles);
}
