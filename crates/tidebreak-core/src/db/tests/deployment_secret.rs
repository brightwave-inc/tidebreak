use super::*;
use crate::db::{DeploymentSecret, DeploymentSecretWrite};

fn sealed(name: &str, key_id: &str, fill: u8, second: i64) -> DeploymentSecret {
    DeploymentSecret {
        name: name.to_owned(),
        key_id: key_id.to_owned(),
        nonce: vec![fill; 12],
        ciphertext: vec![fill.wrapping_add(1); 40],
        updated_at: DateTime::<Utc>::from_timestamp(1_710_000_000 + second, 0).unwrap(),
    }
}

#[tokio::test]
async fn a_secret_round_trips_and_is_replaced_under_its_own_key() {
    let (_dir, store) = temp_store().await;
    assert_eq!(store.deployment_secret("bundle").await.unwrap(), None);
    assert!(store.deployment_secret_key_ids().await.unwrap().is_empty());

    let first = sealed("bundle", "key-one", 1, 0);
    assert_eq!(
        store.put_deployment_secret(&first).await.unwrap(),
        DeploymentSecretWrite::Done
    );
    assert_eq!(
        store.deployment_secret("bundle").await.unwrap(),
        Some(first)
    );

    let second = sealed("bundle", "key-one", 7, 60);
    assert_eq!(
        store.put_deployment_secret(&second).await.unwrap(),
        DeploymentSecretWrite::Done
    );
    assert_eq!(
        store.deployment_secret("bundle").await.unwrap(),
        Some(second)
    );
    assert_eq!(
        store.deployment_secret_key_ids().await.unwrap(),
        ["key-one"]
    );
}

/// A row written under one key is never replaced or removed under another,
/// so a server started with the wrong key file cannot destroy what the right
/// one wrote.
#[tokio::test]
async fn a_row_written_under_another_key_is_never_replaced_or_removed() {
    let (_dir, store) = temp_store().await;
    let original = sealed("bundle", "key-one", 1, 0);
    store.put_deployment_secret(&original).await.unwrap();

    assert_eq!(
        store
            .put_deployment_secret(&sealed("bundle", "key-two", 9, 60))
            .await
            .unwrap(),
        DeploymentSecretWrite::OtherKey
    );
    assert_eq!(
        store
            .delete_deployment_secret("bundle", "key-two")
            .await
            .unwrap(),
        DeploymentSecretWrite::OtherKey
    );
    assert_eq!(
        store.deployment_secret("bundle").await.unwrap(),
        Some(original),
        "the other key's writes left the row exactly as it was"
    );
    assert_eq!(
        store.deployment_secret_key_ids().await.unwrap(),
        ["key-one"]
    );

    assert_eq!(
        store
            .delete_deployment_secret("bundle", "key-one")
            .await
            .unwrap(),
        DeploymentSecretWrite::Done
    );
    assert_eq!(store.deployment_secret("bundle").await.unwrap(), None);
    // Removing what is already gone is done, not an error.
    assert_eq!(
        store
            .delete_deployment_secret("bundle", "key-one")
            .await
            .unwrap(),
        DeploymentSecretWrite::Done
    );
}

#[tokio::test]
async fn key_ids_name_each_key_once() {
    let (_dir, store) = temp_store().await;
    for secret in [
        sealed("b", "key-two", 1, 0),
        sealed("a", "key-one", 2, 0),
        sealed("c", "key-one", 3, 0),
    ] {
        store.put_deployment_secret(&secret).await.unwrap();
    }
    assert_eq!(
        store.deployment_secret_key_ids().await.unwrap(),
        ["key-one", "key-two"]
    );
}
