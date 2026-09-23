//! The list of work over the chat routes: pin, archive, and read.

use super::conversations::patch_chat;
use super::*;

async fn list_chat_listings(router: &Router, bearer: &str, uri: &str) -> Vec<ChatListing> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    json_body(response).await
}

fn listed_ids(listings: &[ChatListing]) -> Vec<SessionId> {
    listings.iter().map(|listing| listing.chat.id).collect()
}

/// Pin, archive, and read are server state behind the chat routes, so the
/// CLI and the phone see the same list the desktop does.
#[tokio::test]
async fn pin_archive_and_read_ride_the_chat_routes() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let older = make_chat(&router, &bearer).await;
    let newer = make_chat(&router, &bearer).await;
    assert_eq!(
        listed_ids(&list_chat_listings(&router, &bearer, "/chats").await),
        vec![newer.id, older.id]
    );

    let pinned = patch_chat(
        &router,
        &bearer,
        older.id,
        serde_json::json!({"pinned": true}),
    )
    .await;
    assert_eq!(pinned.status(), StatusCode::OK);
    let pinned: ChatListing = json_body(pinned).await;
    assert!(pinned.pinned_at.is_some());
    assert_eq!(pinned.turn_count, 0);
    assert_eq!(
        listed_ids(&list_chat_listings(&router, &bearer, "/chats").await),
        vec![older.id, newer.id]
    );

    let both = patch_chat(
        &router,
        &bearer,
        newer.id,
        serde_json::json!({"pinned": true, "archived": true}),
    )
    .await;
    assert_eq!(both.status(), StatusCode::BAD_REQUEST);

    let archived = patch_chat(
        &router,
        &bearer,
        older.id,
        serde_json::json!({"archived": true}),
    )
    .await;
    assert_eq!(archived.status(), StatusCode::OK);
    let archived: ChatListing = json_body(archived).await;
    assert!(archived.archived_at.is_some());
    assert_eq!(archived.pinned_at, None, "archiving unpins");
    assert_eq!(
        listed_ids(&list_chat_listings(&router, &bearer, "/chats").await),
        vec![newer.id]
    );
    assert_eq!(
        listed_ids(&list_chat_listings(&router, &bearer, "/chats?archived=true").await),
        vec![older.id]
    );

    // Archived is not deleted: the conversation still opens by id.
    let fetched = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}", older.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(fetched.status(), StatusCode::OK);
    assert!(json_body::<ChatListing>(fetched)
        .await
        .archived_at
        .is_some());

    let restored = patch_chat(
        &router,
        &bearer,
        older.id,
        serde_json::json!({"archived": false}),
    )
    .await;
    assert_eq!(restored.status(), StatusCode::OK);
    assert_eq!(
        listed_ids(&list_chat_listings(&router, &bearer, "/chats").await),
        vec![newer.id, older.id]
    );

    for (chat, expected) in [
        (older.id, StatusCode::NO_CONTENT),
        (SessionId::new(), StatusCode::NOT_FOUND),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{chat}/read"))
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}
