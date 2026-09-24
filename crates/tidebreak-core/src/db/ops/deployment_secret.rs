//! The self-host profile's encrypted secrets: one `deployment_secrets` row per
//! secret (decision 102).
//!
//! This layer stores and returns encrypted bytes and never sees a value. The
//! server encrypts each value before it calls [`put`] and decrypts after
//! [`get`]. A row written under one key is never replaced or removed under
//! another: [`put`] and [`delete`] leave it as it was and answer
//! [`DeploymentSecretWrite::OtherKey`].

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement};

use crate::error::Result;

use super::super::{store_err, DbStore};
use super::turn::canonical_db_timestamp;

/// One encrypted secret, as the `deployment_secrets` table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentSecret {
    /// The key the secret is stored under, such as
    /// `tidebreak.secret_bundle_v1`.
    pub name: String,
    /// Names the encryption key the row was written under.
    pub key_id: String,
    /// The nonce the row was encrypted with.
    pub nonce: Vec<u8>,
    /// The encrypted value, authentication tag included.
    pub ciphertext: Vec<u8>,
    /// When the row was last written.
    pub updated_at: DateTime<Utc>,
}

/// What a change to `deployment_secrets` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentSecretWrite {
    /// The row holds what was written, or, after a delete, no row of that
    /// name remains.
    Done,
    /// A row of that name was written under another key. It is unchanged.
    OtherKey,
}

const COLUMNS: &str = "\"name\", \"key_id\", \"nonce\", \"ciphertext\", \"updated_at\"";

fn placeholder(backend: DbBackend, number: usize) -> String {
    match backend {
        DbBackend::Postgres => format!("${number}"),
        _ => "?".to_owned(),
    }
}

/// The row stored under `name`, if any.
pub(in crate::db) async fn get(store: &DbStore, name: &str) -> Result<Option<DeploymentSecret>> {
    let backend = store.conn.get_database_backend();
    store
        .conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT {COLUMNS} FROM \"deployment_secrets\" WHERE \"name\" = {}",
                placeholder(backend, 1)
            ),
            [name.into()],
        ))
        .await
        .map_err(store_err)?
        .map(|row| secret_from_row(&row))
        .transpose()
}

/// Store `secret`. An existing row of the same name is replaced only when it
/// was written under the same key.
pub(in crate::db) async fn put(
    store: &DbStore,
    secret: &DeploymentSecret,
) -> Result<DeploymentSecretWrite> {
    let backend = store.conn.get_database_backend();
    let updated_at = canonical_db_timestamp(secret.updated_at)?;
    // The `WHERE` makes a row written under another key a conflict that
    // changes nothing, so the statement affects no row.
    let written = store
        .conn
        .execute_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "INSERT INTO \"deployment_secrets\" ({COLUMNS}) VALUES ({}, {}, {}, {}, {}) \
                 ON CONFLICT (\"name\") DO UPDATE SET \
                 \"nonce\" = excluded.\"nonce\", \
                 \"ciphertext\" = excluded.\"ciphertext\", \
                 \"updated_at\" = excluded.\"updated_at\" \
                 WHERE \"deployment_secrets\".\"key_id\" = excluded.\"key_id\"",
                placeholder(backend, 1),
                placeholder(backend, 2),
                placeholder(backend, 3),
                placeholder(backend, 4),
                placeholder(backend, 5)
            ),
            [
                secret.name.as_str().into(),
                secret.key_id.as_str().into(),
                secret.nonce.clone().into(),
                secret.ciphertext.clone().into(),
                updated_at.into(),
            ],
        ))
        .await
        .map_err(store_err)?;
    Ok(if written.rows_affected() == 0 {
        DeploymentSecretWrite::OtherKey
    } else {
        DeploymentSecretWrite::Done
    })
}

/// Remove the row stored under `name` when it was written under `key_id`.
/// Removing a name that holds no row is done.
pub(in crate::db) async fn delete(
    store: &DbStore,
    name: &str,
    key_id: &str,
) -> Result<DeploymentSecretWrite> {
    let backend = store.conn.get_database_backend();
    let removed = store
        .conn
        .execute_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "DELETE FROM \"deployment_secrets\" WHERE \"name\" = {} AND \"key_id\" = {}",
                placeholder(backend, 1),
                placeholder(backend, 2)
            ),
            [name.into(), key_id.into()],
        ))
        .await
        .map_err(store_err)?;
    if removed.rows_affected() > 0 {
        return Ok(DeploymentSecretWrite::Done);
    }
    // Nothing matched both columns: either no row has this name, or the row
    // that has it was written under another key and must stay.
    Ok(match get(store, name).await? {
        Some(_) => DeploymentSecretWrite::OtherKey,
        None => DeploymentSecretWrite::Done,
    })
}

/// Every key id the stored rows were written under, sorted, each once.
pub(in crate::db) async fn key_ids(store: &DbStore) -> Result<Vec<String>> {
    let backend = store.conn.get_database_backend();
    store
        .conn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT DISTINCT \"key_id\" FROM \"deployment_secrets\" ORDER BY \"key_id\"",
        ))
        .await
        .map_err(store_err)?
        .iter()
        .map(|row| row.try_get::<String>("", "key_id").map_err(store_err))
        .collect()
}

fn secret_from_row(row: &QueryResult) -> Result<DeploymentSecret> {
    Ok(DeploymentSecret {
        name: row.try_get("", "name").map_err(store_err)?,
        key_id: row.try_get("", "key_id").map_err(store_err)?,
        nonce: row.try_get("", "nonce").map_err(store_err)?,
        ciphertext: row.try_get("", "ciphertext").map_err(store_err)?,
        updated_at: row.try_get("", "updated_at").map_err(store_err)?,
    })
}
