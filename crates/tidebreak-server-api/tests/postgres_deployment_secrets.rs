#![cfg(feature = "postgres")]

//! Self-host secrets kept encrypted in PostgreSQL (decision 102), on the
//! backend a self-host deployment runs and through its real boot path.
//!
//! Skipped silently without `TIDEBREAK_POSTGRES_TEST_URL`, and hard-failed in
//! CI, where `TIDEBREAK_REQUIRE_POSTGRES_TEST` is set. The run creates a
//! database of its own beside the configured one, so no row another run or
//! suite wrote ever meets this run's keys.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use tidebreak_core::{Config, DbStore, DeploymentSecretWrite, Profile, SecretProvider};
use tidebreak_server_core::database_secrets::{DatabaseSecretProvider, SecretKey};

/// Each test points `TIDEBREAK_DATABASE_URL` at its own database for the
/// server it boots, and the variable is process-wide, so they take turns.
static DATABASE_URL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvRestore {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

/// A database of this run's own, beside the configured one.
struct TestDatabase {
    admin_url: String,
    url: String,
    name: String,
}

impl TestDatabase {
    async fn create() -> Option<Self> {
        let admin_url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
            Ok(url) => url,
            Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
                panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
            }
            Err(_) => return None,
        };
        let (prefix, database, query) = split_postgres_url(&admin_url);
        let suffix = format!("secrets_{}", uuid::Uuid::new_v4().simple());
        let head: String = database
            .chars()
            .take(62usize.saturating_sub(suffix.len()))
            .collect();
        let name = format!("{head}_{suffix}");
        let admin = Database::connect(&admin_url).await.unwrap();
        admin
            .execute_unprepared(&format!("CREATE DATABASE \"{name}\""))
            .await
            .unwrap();
        admin.close().await.unwrap();
        let url = format!("{prefix}{name}{query}");
        Some(Self {
            admin_url,
            url,
            name,
        })
    }

    async fn drop_database(self) {
        let admin = Database::connect(&self.admin_url).await.unwrap();
        admin
            .execute_unprepared(&format!(
                "DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)",
                self.name
            ))
            .await
            .unwrap();
        admin.close().await.unwrap();
    }
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

/// A key file as `openssl rand -base64 32` writes it, from bytes drawn at
/// run time.
fn write_key_file(dir: &Path, name: &str) -> PathBuf {
    let mut key = Vec::with_capacity(32);
    key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let path = dir.join(name);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&key);
    std::fs::write(&path, format!("{encoded}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    }
    path
}

/// A self-host config that keeps its secrets in the database under the key
/// in `key_file`. The admin token is drawn at run time.
fn self_host_config(data_dir: &Path, key_file: &Path) -> Config {
    let tokens = data_dir.join("tokens");
    let token = format!("secrets-admin-{}", uuid::Uuid::new_v4().simple());
    std::fs::write(&tokens, format!("admin {token} admin\n")).unwrap();
    let mut config = Config::desktop(data_dir);
    config.profile = Profile::SelfHost;
    config.auth_tokens_file = Some(tokens);
    config.listen_addr = Some("127.0.0.1:0".parse().unwrap());
    config.secret_key_file = Some(key_file.to_owned());
    config
}

/// A value made at run time, so no credential-shaped literal sits in the
/// source.
fn fake_value(label: &str) -> String {
    format!("{label}-{}", uuid::Uuid::new_v4().simple())
}

/// The table's columns as PostgreSQL describes them, with the primary key
/// last.
async fn schema(url: &str) -> Vec<String> {
    let connection = Database::connect(url).await.unwrap();
    let mut described: Vec<String> = connection
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT column_name::text AS name, data_type::text AS kind, \
             is_nullable::text AS nullable \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = 'deployment_secrets' \
             ORDER BY ordinal_position",
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| {
            format!(
                "{} {} null={}",
                row.try_get::<String>("", "name").unwrap(),
                row.try_get::<String>("", "kind").unwrap(),
                row.try_get::<String>("", "nullable").unwrap()
            )
        })
        .collect();
    let primary_key = connection
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT attribute.attname::text AS name \
             FROM pg_index AS index \
             JOIN pg_attribute AS attribute \
               ON attribute.attrelid = index.indrelid \
              AND attribute.attnum = ANY(index.indkey) \
             WHERE index.indrelid = 'deployment_secrets'::regclass AND index.indisprimary",
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect::<Vec<_>>();
    described.push(format!("primary key {}", primary_key.join(", ")));
    connection.close().await.unwrap();
    described
}

/// How many rows hold `value` anywhere in their nonce or ciphertext bytes.
async fn rows_holding(url: &str, value: &str) -> i64 {
    let connection = Database::connect(url).await.unwrap();
    let count = connection
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS holding FROM deployment_secrets \
             WHERE position(convert_to($1, 'UTF8') IN ciphertext) > 0 \
                OR position(convert_to($1, 'UTF8') IN nonce) > 0",
            [value.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "holding")
        .unwrap();
    connection.close().await.unwrap();
    count
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_keeps_secrets_encrypted_bound_to_their_name_and_to_one_key() {
    let _turn = DATABASE_URL_LOCK.lock().await;
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let _database_url = EnvRestore::set("TIDEBREAK_DATABASE_URL", &database.url);
    let keys = tempfile::tempdir().unwrap();
    let key_file = write_key_file(keys.path(), "secret.key");
    let key_id = SecretKey::from_file(&key_file).unwrap().id().to_owned();

    // The first boot migrates the empty database and opens its custody.
    let first_dir = tempfile::tempdir().unwrap();
    let first = tidebreak_server::bind(self_host_config(first_dir.path(), &key_file))
        .await
        .expect("a self-host server boots with a key file and no stored secrets");
    drop(first);
    assert_eq!(
        schema(&database.url).await,
        [
            "name text null=NO",
            "key_id text null=NO",
            "nonce bytea null=NO",
            "ciphertext bytea null=NO",
            "updated_at timestamp with time zone null=NO",
            "primary key name",
        ]
    );

    let store = Arc::new(DbStore::connect(&database.url).await.unwrap());
    let secrets =
        DatabaseSecretProvider::open(store.clone(), SecretKey::from_file(&key_file).unwrap())
            .await
            .unwrap();

    // A round trip, then an overwrite under a fresh nonce, with no value in
    // the clear in any row.
    let first_value = fake_value("first");
    secrets
        .set_secret("provider.test", &first_value)
        .await
        .unwrap();
    assert_eq!(
        secrets.get_secret("provider.test").await.unwrap(),
        Some(first_value.clone())
    );
    let first_row = store
        .deployment_secret("provider.test")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_row.key_id, key_id);
    let value = fake_value("second");
    secrets.set_secret("provider.test", &value).await.unwrap();
    assert_eq!(
        secrets.get_secret("provider.test").await.unwrap(),
        Some(value.clone())
    );
    let row = store
        .deployment_secret("provider.test")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first_row.nonce, row.nonce, "every write draws a new nonce");
    assert_eq!(rows_holding(&database.url, &first_value).await, 0);
    assert_eq!(rows_holding(&database.url, &value).await, 0);

    // A delete removes the row, and a second delete is not an error.
    secrets
        .set_secret("provider.deleted", &fake_value("deleted"))
        .await
        .unwrap();
    secrets.delete_secret("provider.deleted").await.unwrap();
    assert_eq!(secrets.get_secret("provider.deleted").await.unwrap(), None);
    assert_eq!(
        store.deployment_secret("provider.deleted").await.unwrap(),
        None
    );
    secrets.delete_secret("provider.deleted").await.unwrap();

    // A ciphertext copied to another name fails to decrypt there.
    let mut moved = row.clone();
    moved.name = "provider.moved".to_owned();
    assert_eq!(
        store.put_deployment_secret(&moved).await.unwrap(),
        DeploymentSecretWrite::Done
    );
    let error = secrets
        .get_secret("provider.moved")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("provider.moved") && error.contains("could not be decrypted"),
        "{error}"
    );
    assert!(!error.contains(&value), "{error}");
    secrets.delete_secret("provider.moved").await.unwrap();

    // So does a ciphertext altered in place.
    let mut tampered = row.clone();
    tampered.ciphertext[0] ^= 0x01;
    store.put_deployment_secret(&tampered).await.unwrap();
    let error = secrets
        .get_secret("provider.test")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("provider.test") && error.contains("could not be decrypted"),
        "{error}"
    );
    store.put_deployment_secret(&row).await.unwrap();
    assert_eq!(
        secrets.get_secret("provider.test").await.unwrap(),
        Some(value.clone())
    );

    // A row is never replaced or removed under another key.
    let other_key_file = write_key_file(keys.path(), "other.key");
    let other_id = SecretKey::from_file(&other_key_file)
        .unwrap()
        .id()
        .to_owned();
    let mut foreign = row.clone();
    foreign.key_id = other_id.clone();
    assert_eq!(
        store.put_deployment_secret(&foreign).await.unwrap(),
        DeploymentSecretWrite::OtherKey
    );
    assert_eq!(
        store
            .delete_deployment_secret("provider.test", &other_id)
            .await
            .unwrap(),
        DeploymentSecretWrite::OtherKey
    );
    assert_eq!(
        store.deployment_secret("provider.test").await.unwrap(),
        Some(row.clone())
    );

    // The wrong key file refuses the real boot and changes nothing.
    let wrong_dir = tempfile::tempdir().unwrap();
    let refusal =
        match tidebreak_server::bind(self_host_config(wrong_dir.path(), &other_key_file)).await {
            Err(error) => error.to_string(),
            Ok(_) => panic!("a self-host server booted with a key that wrote none of its secrets"),
        };
    assert!(
        refusal.contains("does not match the stored secrets"),
        "the refusal must say the key file does not match: {refusal}"
    );
    assert!(
        refusal.contains("Restore the original key file"),
        "the refusal must name the remedy: {refusal}"
    );
    assert_eq!(
        store.deployment_secret("provider.test").await.unwrap(),
        Some(row)
    );
    assert_eq!(
        store
            .deployment_secrets()
            .await
            .unwrap()
            .into_iter()
            .map(|secret| secret.key_id)
            .collect::<Vec<_>>(),
        [key_id]
    );

    // The original key file boots again.
    let again_dir = tempfile::tempdir().unwrap();
    let again = tidebreak_server::bind(self_host_config(again_dir.path(), &key_file))
        .await
        .expect("the original key file boots over its own secrets");
    drop(again);
    assert_eq!(
        secrets.get_secret("provider.test").await.unwrap(),
        Some(value)
    );

    drop(secrets);
    drop(store);
    database.drop_database().await;
}

/// A row the configured key wrote that no longer decrypts, as after a
/// damaged restore, refuses the real boot and names the secret. Without the
/// check the server started, and every provider whose credential sat in that
/// row read as unconfigured.
#[tokio::test(flavor = "multi_thread")]
async fn postgres_refuses_to_boot_over_a_row_that_no_longer_decrypts() {
    let _turn = DATABASE_URL_LOCK.lock().await;
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let _database_url = EnvRestore::set("TIDEBREAK_DATABASE_URL", &database.url);
    let keys = tempfile::tempdir().unwrap();
    let key_file = write_key_file(keys.path(), "secret.key");

    // Boot once so the server migrates the empty database, then store a
    // credential the way the server does: in the one bundle row.
    let first_dir = tempfile::tempdir().unwrap();
    drop(
        tidebreak_server::bind(self_host_config(first_dir.path(), &key_file))
            .await
            .expect("a self-host server boots with a key file and no stored secrets"),
    );
    let store = Arc::new(DbStore::connect(&database.url).await.unwrap());
    let value = fake_value("bundled");
    DatabaseSecretProvider::open(store.clone(), SecretKey::from_file(&key_file).unwrap())
        .await
        .unwrap()
        .set_secret(tidebreak_core::BUNDLE_KEY, &value)
        .await
        .unwrap();
    let original = store
        .deployment_secret(tidebreak_core::BUNDLE_KEY)
        .await
        .unwrap()
        .unwrap();

    // Damage the row in place, as a bad restore might, keeping its key id.
    let connection = Database::connect(&database.url).await.unwrap();
    let damaged = connection
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE deployment_secrets \
             SET ciphertext = set_byte(ciphertext, 0, get_byte(ciphertext, 0) # 1) \
             WHERE name = $1",
            [tidebreak_core::BUNDLE_KEY.into()],
        ))
        .await
        .unwrap();
    assert_eq!(damaged.rows_affected(), 1);
    connection.close().await.unwrap();
    let before = store
        .deployment_secret(tidebreak_core::BUNDLE_KEY)
        .await
        .unwrap();

    let damaged_dir = tempfile::tempdir().unwrap();
    let refusal =
        match tidebreak_server::bind(self_host_config(damaged_dir.path(), &key_file)).await {
            Err(error) => error.to_string(),
            Ok(_) => panic!("a self-host server booted over a stored secret it cannot decrypt"),
        };
    assert!(
        refusal.contains(tidebreak_core::BUNDLE_KEY) && refusal.contains("cannot decrypt"),
        "the refusal must name the secret that no longer decrypts: {refusal}"
    );
    assert!(
        refusal.contains("Restore the database"),
        "the refusal must name the remedy: {refusal}"
    );
    assert!(!refusal.contains(&value), "{refusal}");
    assert_eq!(
        store
            .deployment_secret(tidebreak_core::BUNDLE_KEY)
            .await
            .unwrap(),
        before,
        "the refusal changed no row"
    );

    // Putting the undamaged row back lets the same key file boot again.
    store.put_deployment_secret(&original).await.unwrap();
    let repaired_dir = tempfile::tempdir().unwrap();
    drop(
        tidebreak_server::bind(self_host_config(repaired_dir.path(), &key_file))
            .await
            .expect("the key file boots once the row decrypts again"),
    );

    drop(store);
    database.drop_database().await;
}
