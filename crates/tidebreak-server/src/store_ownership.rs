//! Exclusive ownership of one self-host PostgreSQL store.
//!
//! The filesystem instance lock protects one data directory. A self-host
//! deployment can point two different directories at the same PostgreSQL
//! database, so it also needs a database-scoped owner before migrations and
//! process-local workers start.

use tidebreak_core::{Config, Profile, Result};

#[cfg(feature = "postgres")]
use tidebreak_core::AgentError;

#[cfg(feature = "postgres")]
use sea_orm::sqlx::postgres::{
    PgAdvisoryLock, PgAdvisoryLockGuard, PgAdvisoryLockKey, PgConnectOptions,
};
#[cfg(feature = "postgres")]
use sea_orm::sqlx::{Connection, Either, PgConnection};

#[cfg(feature = "postgres")]
const POSTGRES_OWNERSHIP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(feature = "postgres")]
const POSTGRES_OWNERSHIP_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long each monitor sleep on the owner connection lasts. Short on
/// purpose: a backend blocked in `pg_sleep` never reads its socket, so when
/// this process dies without a Terminate message (a pod stop is a signal, not
/// a handshake) the backend only notices the dead client on its next reply.
/// A single 68-year sleep left the lock held until TCP keepalive gave up,
/// hours later, and every replacement refused to boot meanwhile.
#[cfg(feature = "postgres")]
const POSTGRES_OWNERSHIP_MONITOR_SLEEP_SECONDS: f64 = 5.0;
/// How long a booting process waits for a previous owner's backend to let go
/// before calling the store owned. Covers one monitor sleep plus the reply
/// that surfaces the closed socket, so a `Recreate` rollout's replacement
/// boots instead of crash-looping behind its predecessor.
#[cfg(feature = "postgres")]
const POSTGRES_OWNERSHIP_ACQUIRE_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
#[cfg(feature = "postgres")]
const POSTGRES_OWNERSHIP_ACQUIRE_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// Stable across Tidebreak and SQLx versions so a rolling deploy cannot make
/// two server versions choose different ownership keys.
#[cfg(feature = "postgres")]
const POSTGRES_OWNERSHIP_LOCK_KEY: i64 = i64::from_be_bytes(*b"TDBKSERV");
#[cfg(feature = "postgres")]
const POSTGRES_OWNERSHIP_APPLICATION_NAME: &str = "tidebreak-store-owner";

pub(crate) enum StoreOwnership {
    Local,
    #[cfg(feature = "postgres")]
    Postgres(PostgresStoreOwnership),
}

impl StoreOwnership {
    pub(crate) async fn acquire(config: &Config) -> Result<Self> {
        match config.profile {
            Profile::Desktop => Ok(Self::Local),
            Profile::SelfHost => {
                #[cfg(feature = "postgres")]
                {
                    crate::auth::PrincipalAuthenticator::from_config(config)?;
                    return PostgresStoreOwnership::acquire(config)
                        .await
                        .map(Self::Postgres);
                }
                #[cfg(not(feature = "postgres"))]
                {
                    Ok(Self::Local)
                }
            }
            _ => Ok(Self::Local),
        }
    }

    pub(crate) async fn verify(&mut self) -> Result<()> {
        match self {
            Self::Local => Ok(()),
            #[cfg(feature = "postgres")]
            Self::Postgres(ownership) => ownership.verify().await,
        }
    }
}

#[cfg(feature = "postgres")]
pub(crate) struct PostgresStoreOwnership {
    guard: PgAdvisoryLockGuard<PgConnection>,
}

#[cfg(feature = "postgres")]
impl PostgresStoreOwnership {
    async fn acquire(config: &Config) -> Result<Self> {
        let url = config.database_url()?;
        let options = url
            .parse::<PgConnectOptions>()
            .map_err(|_| {
                AgentError::config("invalid TIDEBREAK_DATABASE_URL for self-host ownership")
            })?
            .application_name(POSTGRES_OWNERSHIP_APPLICATION_NAME);
        let connection = tokio::time::timeout(
            POSTGRES_OWNERSHIP_CONNECT_TIMEOUT,
            PgConnection::connect_with(&options),
        )
        .await
        .map_err(|_| {
            AgentError::config("timed out connecting to PostgreSQL for self-host ownership")
        })?
        .map_err(|_| {
            AgentError::config(
                "could not connect to PostgreSQL for self-host ownership; check the database host, port, TLS settings, and credentials",
            )
        })?;

        let lock = PgAdvisoryLock::with_key(PgAdvisoryLockKey::BigInt(POSTGRES_OWNERSHIP_LOCK_KEY));
        let deadline = tokio::time::Instant::now() + POSTGRES_OWNERSHIP_ACQUIRE_WAIT;
        let mut connection = connection;
        loop {
            connection = match lock.try_acquire(connection).await.map_err(|error| {
                AgentError::config(format!(
                    "could not acquire PostgreSQL self-host ownership: {error}"
                ))
            })? {
                Either::Left(guard) => return Ok(Self { guard }),
                Either::Right(connection) => connection,
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(AgentError::config(
                    "another Tidebreak self-host process already owns this PostgreSQL database. Stop it before starting another process against the same TIDEBREAK_DATABASE_URL",
                ));
            }
            tokio::time::sleep(POSTGRES_OWNERSHIP_ACQUIRE_RETRY).await;
        }
    }

    pub(crate) async fn verify(&mut self) -> Result<()> {
        let result = tokio::time::timeout(
            POSTGRES_OWNERSHIP_CHECK_TIMEOUT,
            sea_orm::sqlx::query("SELECT 1").execute(self.guard.as_mut()),
        )
        .await;
        if matches!(result, Ok(Ok(_))) {
            return Ok(());
        }
        Err(ownership_lost_error())
    }

    /// Keep a query pending on the lock-holding connection. PostgreSQL ends
    /// the query as soon as that backend disappears, so `serve` can stop in
    /// the same `select!` instead of waiting for a periodic health check.
    /// The sleep runs in short rounds so the backend, in turn, notices this
    /// process disappearing (see [`POSTGRES_OWNERSHIP_MONITOR_SLEEP_SECONDS`]).
    pub(crate) async fn wait_until_lost(&mut self) -> AgentError {
        loop {
            if sea_orm::sqlx::query("SELECT pg_sleep($1)")
                .bind(POSTGRES_OWNERSHIP_MONITOR_SLEEP_SECONDS)
                .execute(self.guard.as_mut())
                .await
                .is_err()
            {
                return ownership_lost_error();
            }
        }
    }
}

#[cfg(feature = "postgres")]
fn ownership_lost_error() -> AgentError {
    AgentError::msg(
        "lost the PostgreSQL self-host ownership connection; stopping the server before another Tidebreak process can take over the database",
    )
}
