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
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_OWNERSHIP_CHECK_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(5);

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
        let guard = match lock.try_acquire(connection).await.map_err(|error| {
            AgentError::config(format!(
                "could not acquire PostgreSQL self-host ownership: {error}"
            ))
        })? {
            Either::Left(guard) => guard,
            Either::Right(_) => {
                return Err(AgentError::config(
                    "another Tidebreak self-host process already owns this PostgreSQL database. Stop it before starting another process against the same TIDEBREAK_DATABASE_URL",
                ));
            }
        };
        Ok(Self { guard })
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
        Err(AgentError::msg(
            "lost the PostgreSQL self-host ownership connection; stopping the server before another Tidebreak process can take over the database",
        ))
    }
}
