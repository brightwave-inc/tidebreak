#![cfg(feature = "postgres")]

use std::path::Path;
use std::time::Duration;

use sea_orm::sqlx::{Connection, PgConnection};
use tidebreak_core::{Config, KeychainSecretProvider, Profile};

const ADMIN_TOKEN: &str = "store-owner-test-token-padded-to-32";
const OWNERSHIP_APPLICATION_NAME: &str = "tidebreak-store-owner";

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

fn self_host_config(data_dir: &Path) -> Config {
    let tokens = data_dir.join("tokens");
    std::fs::write(&tokens, format!("admin {ADMIN_TOKEN} admin\n"))
        .expect("write self-host token fixture");
    let mut config = Config::desktop(data_dir);
    config.profile = Profile::SelfHost;
    config.auth_tokens_file = Some(tokens);
    config.listen_addr = Some("127.0.0.1:0".parse().expect("loopback address"));
    config
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_store_ownership_is_exclusive_and_outlives_a_dead_owner() {
    let url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let _database_url = EnvRestore::set("TIDEBREAK_DATABASE_URL", &url);
    KeychainSecretProvider::use_mock();

    let first_dir = tempfile::tempdir().expect("first data dir");
    let second_dir = tempfile::tempdir().expect("second data dir");
    let first = tidebreak_server::bind(self_host_config(first_dir.path()))
        .await
        .expect("first self-host server owns the database");
    let second_config = self_host_config(second_dir.path());
    let refusal = match tidebreak_server::bind(second_config.clone()).await {
        Err(error) => error.to_string(),
        Ok(_) => panic!("a second self-host server must not own the same database"),
    };
    assert!(
        refusal.contains("already owns this PostgreSQL database"),
        "the boot refusal must name database ownership: {refusal}"
    );

    let serving = tokio::spawn(first.serve());
    let mut admin = PgConnection::connect(&url)
        .await
        .expect("connect to inspect the ownership session");
    let owner_pid: i32 = sea_orm::sqlx::query_scalar(
        "SELECT activity.pid \
         FROM pg_stat_activity AS activity \
         JOIN pg_locks AS lock ON lock.pid = activity.pid \
         WHERE activity.datname = current_database() \
           AND activity.application_name = $1 \
           AND lock.locktype = 'advisory' \
           AND lock.granted",
    )
    .bind(OWNERSHIP_APPLICATION_NAME)
    .fetch_one(&mut admin)
    .await
    .expect("find the ownership connection");
    let terminated: bool = sea_orm::sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(owner_pid)
        .fetch_one(&mut admin)
        .await
        .expect("terminate the ownership connection");
    assert!(
        terminated,
        "PostgreSQL must terminate the ownership session"
    );

    let replacement = tidebreak_server::bind(second_config)
        .await
        .expect("a replacement can take ownership after the backend closes");
    let stopped = tokio::time::timeout(Duration::from_secs(1), serving)
        .await
        .expect("the old server must stop before its replacement can overlap")
        .expect("the server task must not panic")
        .expect_err("losing ownership must fail the serve loop");
    assert!(
        stopped
            .to_string()
            .contains("lost the PostgreSQL self-host ownership connection"),
        "the serve failure must name the lost lease: {stopped}"
    );

    drop(replacement);

    // A pod stop is a signal, not a Terminate message: the owner's socket
    // closes while its backend is asleep in the monitor query. The next boot
    // must still get the store within the acquisition wait, not hours later
    // when TCP keepalive finally ends that backend.
    let silent_dir = tempfile::tempdir().expect("silent-owner data dir");
    let silent = tidebreak_server::bind(self_host_config(silent_dir.path()))
        .await
        .expect("a self-host server owns the database again");
    let serving = tokio::spawn(silent.serve());
    let monitoring = async {
        loop {
            let asleep: bool = sea_orm::sqlx::query_scalar(
                "SELECT EXISTS ( \
                   SELECT 1 FROM pg_stat_activity \
                   WHERE datname = current_database() \
                     AND application_name = $1 \
                     AND state = 'active' \
                     AND query LIKE 'SELECT pg_sleep%')",
            )
            .bind(OWNERSHIP_APPLICATION_NAME)
            .fetch_one(&mut admin)
            .await
            .expect("inspect the owner's monitor query");
            if asleep {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), monitoring)
        .await
        .expect("the serving owner must be asleep in its monitor query");
    serving.abort();
    let _ = serving.await;

    let successor_dir = tempfile::tempdir().expect("successor data dir");
    let successor = tidebreak_server::bind(self_host_config(successor_dir.path()))
        .await
        .expect(
            "a replacement takes ownership once the dead owner's backend notices the closed socket",
        );
    drop(successor);

    let secret = "store-owner-url-secret";
    std::env::set_var(
        "TIDEBREAK_DATABASE_URL",
        format!("postgres://owner:{secret}@[invalid"),
    );
    let invalid_dir = tempfile::tempdir().expect("invalid-url data dir");
    let invalid = match tidebreak_server::bind(self_host_config(invalid_dir.path())).await {
        Err(error) => error.to_string(),
        Ok(_) => panic!("an invalid PostgreSQL URL must refuse boot"),
    };
    assert!(
        invalid.contains("invalid TIDEBREAK_DATABASE_URL"),
        "the refusal must name the invalid setting: {invalid}"
    );
    assert!(
        !invalid.contains(secret),
        "the refusal must not expose database credentials: {invalid}"
    );
}
