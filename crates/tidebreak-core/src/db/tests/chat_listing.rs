//! The list of work: ordering, pins, the archive, unread, and pruning.

use super::*;

const OWNER: fn() -> OwnerId = OwnerId::local;

fn untitled_chat(created_at: i64) -> Chat {
    Chat {
        title: None,
        created_at: DateTime::<Utc>::from_timestamp(created_at, 0).unwrap(),
        ..sample_chat()
    }
}

fn ids(listings: &[ChatListing]) -> Vec<SessionId> {
    listings.iter().map(|listing| listing.chat.id).collect()
}

async fn listing(store: &DbStore, id: SessionId) -> ChatListing {
    store
        .get_chat_listing_scoped(&OWNER(), id)
        .await
        .unwrap()
        .expect("the conversation is listed")
}

async fn accept(store: &DbStore, chat: SessionId) -> TurnRun {
    match store
        .accept_turn(TurnId::new(), chat, "test", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    }
}

/// Claim and complete the one due turn, the way the turn worker does.
async fn finish(store: &DbStore, turn: &TurnRun) {
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("the accepted turn is due");
    let output = Message {
        id: MessageId::new(),
        chat_id: turn.chat_id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "done".into(),
        llm_content: None,
        created_at: claimed_at,
    };
    store
        .complete_turn_and_append_event(
            turn.id,
            lease_token,
            0,
            claimed_at,
            &output,
            0,
            crate::provider::Usage::default(),
            crate::provider::StopReason::EndTurn,
        )
        .await
        .unwrap()
        .expect("the claimed turn completes");
}

#[tokio::test]
async fn the_list_follows_activity_not_creation() {
    let (_dir, store) = temp_store().await;
    let older = untitled_chat(1_000);
    let newer = untitled_chat(2_000);
    store.create_chat(&older).await.unwrap();
    store.create_chat(&newer).await.unwrap();

    // Nothing has happened yet, so creation orders the list.
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![newer.id, older.id]);
    assert_eq!(listed[1].last_activity_at, older.created_at);
    assert_eq!(listed[1].turn_count, 0);

    // A turn in the older conversation moves it to the top, running.
    let turn = accept(&store, older.id).await;
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![older.id, newer.id]);
    assert!(listed[0].running);
    assert_eq!(listed[0].turn_count, 1);
    assert!(listed[0].last_activity_at > newer.created_at);

    // Renaming the newer one is activity too.
    assert!(store
        .update_chat_metadata(
            newer.id,
            Some(Some("Renamed".into())),
            None,
            None,
            None,
            None
        )
        .await
        .unwrap());
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![newer.id, older.id]);

    // Finishing the turn moves the older one back up and stops it running.
    finish(&store, &turn).await;
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![older.id, newer.id]);
    assert!(!listed[0].running);
}

#[tokio::test]
async fn a_finished_turn_stays_unread_until_the_owner_opens_it() {
    let (_dir, store) = temp_store().await;
    let chat = untitled_chat(1_000);
    store.create_chat(&chat).await.unwrap();

    let turn = accept(&store, chat.id).await;
    assert!(
        !listing(&store, chat.id).await.unread,
        "a running turn has not finished yet"
    );
    finish(&store, &turn).await;
    assert!(listing(&store, chat.id).await.unread);

    assert!(store
        .mark_chat_read_scoped(&OWNER(), chat.id)
        .await
        .unwrap());
    assert!(!listing(&store, chat.id).await.unread);

    // Someone else cannot clear it, and cannot tell it exists.
    let stranger = OwnerId::new("user:stranger").unwrap();
    assert!(!store
        .mark_chat_read_scoped(&stranger, chat.id)
        .await
        .unwrap());
}

#[tokio::test]
async fn pins_sit_on_top_and_archiving_moves_a_chat_out_of_the_list() {
    let (_dir, store) = temp_store().await;
    let first = untitled_chat(1_000);
    let second = untitled_chat(2_000);
    let third = untitled_chat(3_000);
    for chat in [&first, &second, &third] {
        store.create_chat(chat).await.unwrap();
    }

    assert!(store
        .set_chat_pinned_scoped(&OWNER(), first.id, true)
        .await
        .unwrap());
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![first.id, third.id, second.id]);
    let pinned_at = listed[0].pinned_at.expect("pinned");

    // Pinning again keeps the first pin, so the pinned group does not shuffle.
    assert!(store
        .set_chat_pinned_scoped(&OWNER(), first.id, true)
        .await
        .unwrap());
    assert_eq!(listing(&store, first.id).await.pinned_at, Some(pinned_at));

    // Archiving unpins and leaves the list for the archive.
    assert!(store
        .set_chat_archived_scoped(&OWNER(), first.id, true)
        .await
        .unwrap());
    let archived = listing(&store, first.id).await;
    assert_eq!(archived.pinned_at, None);
    assert!(archived.archived_at.is_some());
    assert_eq!(
        ids(&store
            .list_chat_listings_scoped(&OWNER(), false)
            .await
            .unwrap()),
        vec![third.id, second.id]
    );
    assert_eq!(
        ids(&store
            .list_chat_listings_scoped(&OWNER(), true)
            .await
            .unwrap()),
        vec![first.id]
    );

    // Unarchiving brings it back where its activity puts it.
    assert!(store
        .set_chat_archived_scoped(&OWNER(), first.id, false)
        .await
        .unwrap());
    assert_eq!(
        ids(&store
            .list_chat_listings_scoped(&OWNER(), false)
            .await
            .unwrap()),
        vec![third.id, second.id, first.id]
    );

    // A new turn in an archived conversation brings it back too.
    assert!(store
        .set_chat_archived_scoped(&OWNER(), second.id, true)
        .await
        .unwrap());
    accept(&store, second.id).await;
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed)[0], second.id);
    assert_eq!(listed[0].archived_at, None);

    // Someone else's conversation cannot be pinned or archived.
    let stranger = OwnerId::new("user:stranger").unwrap();
    assert!(!store
        .set_chat_pinned_scoped(&stranger, third.id, true)
        .await
        .unwrap());
    assert!(!store
        .set_chat_archived_scoped(&stranger, third.id, true)
        .await
        .unwrap());
}

#[tokio::test]
async fn pruning_deletes_only_conversations_nothing_happened_in() {
    let (_dir, store) = temp_store().await;
    let empty = untitled_chat(1_000);
    let named = Chat {
        title: Some("Keep me".into()),
        ..untitled_chat(1_000)
    };
    let pinned = untitled_chat(1_000);
    let archived = untitled_chat(1_000);
    let worked = untitled_chat(1_000);
    let recent = untitled_chat(5_000);
    for chat in [&empty, &named, &pinned, &archived, &worked, &recent] {
        store.create_chat(chat).await.unwrap();
    }
    assert!(store
        .set_chat_pinned_scoped(&OWNER(), pinned.id, true)
        .await
        .unwrap());
    assert!(store
        .set_chat_archived_scoped(&OWNER(), archived.id, true)
        .await
        .unwrap());
    let turn = accept(&store, worked.id).await;
    finish(&store, &turn).await;

    let cutoff = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
    assert_eq!(
        store.prune_empty_chats(cutoff).await.unwrap(),
        vec![empty.id]
    );
    assert_eq!(store.get_chat(empty.id).await.unwrap(), None);
    for kept in [&named, &pinned, &archived, &worked, &recent] {
        assert!(
            store.get_chat(kept.id).await.unwrap().is_some(),
            "pruning removed a conversation that held something"
        );
    }
    assert!(store.prune_empty_chats(cutoff).await.unwrap().is_empty());
}
