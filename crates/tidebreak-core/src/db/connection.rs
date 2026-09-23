//! The store's database handle: one writer, and on SQLite a pool of readers.
//!
//! SQLite admits one writer at a time. When readers and writers share one
//! pool, a writer waiting in SQLite's busy handler keeps its pooled
//! connection for as long as it waits, so a few blocked writers leave no
//! connection for reads. A SQLite store opened with more than one connection
//! therefore opens two pools over the same file:
//!
//! - A writer pool of exactly one connection. Every write and every
//!   transaction runs on it, one at a time. sqlx hands out pooled connections
//!   first come, first served, so waiting writers queue in the order they
//!   asked instead of racing SQLite's busy handler. A transaction begins with
//!   `BEGIN IMMEDIATE`: it holds SQLite's write lock from its first statement,
//!   so it never fails later on a lock upgrade, and a writer in another
//!   process makes it wait at `BEGIN` under the busy timeout.
//! - A read pool of `query_only` connections. A plain `SELECT` outside a
//!   transaction runs here. WAL lets these readers run beside the writer, so a
//!   read never waits behind a write.
//!
//! A read on the read pool sees every write that committed before it started,
//! because a write outside a transaction commits before it returns. Reads
//! inside a transaction run on the transaction's own connection.
//!
//! PostgreSQL has row locks and concurrent writers, so its store keeps one
//! pool and this type passes everything straight through.

use std::fmt::{Debug, Display};
use std::future::Future;
use std::pin::Pin;

use sea_orm::{
    AccessMode, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr,
    ExecResult, IsolationLevel, QueryResult, SqliteTransactionMode, Statement, TransactionError,
    TransactionOptions, TransactionTrait,
};

/// The connections one [`super::DbStore`] runs its statements on.
#[derive(Clone)]
pub(crate) struct StoreConnection {
    /// Every write, every transaction, and every read when there is no read
    /// pool.
    write: DatabaseConnection,
    /// Plain reads outside a transaction. SQLite only, and only when the store
    /// was opened with more than one connection.
    read: Option<DatabaseConnection>,
}

impl StoreConnection {
    /// One pool for everything: PostgreSQL, or a single-connection SQLite
    /// store.
    pub(crate) fn single(write: DatabaseConnection) -> Self {
        Self { write, read: None }
    }

    /// A SQLite writer plus a `query_only` read pool over the same file.
    pub(crate) fn split(write: DatabaseConnection, read: DatabaseConnection) -> Self {
        Self {
            write,
            read: Some(read),
        }
    }

    /// The writer, for tests that inspect the schema it migrated.
    #[cfg(test)]
    pub(crate) fn writer(&self) -> &DatabaseConnection {
        &self.write
    }

    /// Which database this store runs on.
    pub(crate) fn get_database_backend(&self) -> DbBackend {
        self.write.get_database_backend()
    }

    /// The read pool, when this store has one.
    #[cfg(test)]
    pub(crate) fn read_pool(&self) -> Option<&DatabaseConnection> {
        self.read.as_ref()
    }

    /// Close the read pool, then the writer, releasing every file handle.
    ///
    /// The writer goes last: SQLite checkpoints the WAL and removes its side
    /// files when the last connection to a database closes.
    pub(crate) async fn close(self) -> Result<(), DbErr> {
        let read = match self.read {
            Some(read) => read.close().await,
            None => Ok(()),
        };
        let write = self.write.close().await;
        read.and(write)
    }

    fn sqlite(&self) -> bool {
        self.write.get_database_backend() == DbBackend::Sqlite
    }

    fn for_query(&self, sql: &str) -> &DatabaseConnection {
        match &self.read {
            Some(read) if is_plain_read(sql) => read,
            _ => &self.write,
        }
    }
}

/// Whether `sql` only reads, so a read-pool connection may run it.
///
/// Deliberately narrow: only a statement whose first word is `SELECT`
/// qualifies. `INSERT … RETURNING` arrives through the query path too, and a
/// `WITH` clause can end in a write, so both go to the writer. A read that
/// lands on the writer is only slower. A write that lands on the read pool
/// fails, because those connections are `query_only`.
pub(crate) fn is_plain_read(sql: &str) -> bool {
    sql.trim_start()
        .split(|character: char| !character.is_ascii_alphabetic())
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("select"))
}

#[async_trait::async_trait]
impl ConnectionTrait for StoreConnection {
    fn get_database_backend(&self) -> DbBackend {
        self.write.get_database_backend()
    }

    async fn execute_raw(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        self.write.execute_raw(stmt).await
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        self.write.execute_unprepared(sql).await
    }

    async fn query_one_raw(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        self.for_query(&stmt.sql).query_one_raw(stmt).await
    }

    async fn query_all_raw(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        self.for_query(&stmt.sql).query_all_raw(stmt).await
    }

    fn support_returning(&self) -> bool {
        self.write.support_returning()
    }
}

#[async_trait::async_trait]
impl TransactionTrait for StoreConnection {
    type Transaction = DatabaseTransaction;

    async fn begin(&self) -> Result<DatabaseTransaction, DbErr> {
        if self.sqlite() {
            self.begin_with_options(TransactionOptions::default()).await
        } else {
            self.write.begin().await
        }
    }

    async fn begin_with_config(
        &self,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<DatabaseTransaction, DbErr> {
        if self.sqlite() {
            self.begin_with_options(TransactionOptions {
                isolation_level,
                access_mode,
                sqlite_transaction_mode: None,
            })
            .await
        } else {
            self.write
                .begin_with_config(isolation_level, access_mode)
                .await
        }
    }

    async fn begin_with_options(
        &self,
        mut options: TransactionOptions,
    ) -> Result<DatabaseTransaction, DbErr> {
        if self.sqlite() && options.sqlite_transaction_mode.is_none() {
            options.sqlite_transaction_mode = Some(SqliteTransactionMode::Immediate);
        }
        self.write.begin_with_options(options).await
    }

    async fn transaction<F, T, E>(&self, callback: F) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(
                &'c DatabaseTransaction,
            ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
            + Send,
        T: Send,
        E: Display + Debug + Send,
    {
        let transaction = self.begin().await.map_err(TransactionError::Connection)?;
        run_in(transaction, callback).await
    }

    async fn transaction_with_config<F, T, E>(
        &self,
        callback: F,
        isolation_level: Option<IsolationLevel>,
        access_mode: Option<AccessMode>,
    ) -> Result<T, TransactionError<E>>
    where
        F: for<'c> FnOnce(
                &'c DatabaseTransaction,
            ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
            + Send,
        T: Send,
        E: Display + Debug + Send,
    {
        let transaction = self
            .begin_with_config(isolation_level, access_mode)
            .await
            .map_err(TransactionError::Connection)?;
        run_in(transaction, callback).await
    }
}

/// Run `callback` in `transaction`: commit on `Ok`, roll back on `Err`.
async fn run_in<F, T, E>(
    transaction: DatabaseTransaction,
    callback: F,
) -> Result<T, TransactionError<E>>
where
    F: for<'c> FnOnce(
            &'c DatabaseTransaction,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
        + Send,
    T: Send,
    E: Display + Debug + Send,
{
    let outcome = callback(&transaction).await;
    match outcome {
        Ok(value) => {
            transaction
                .commit()
                .await
                .map_err(TransactionError::Connection)?;
            Ok(value)
        }
        Err(error) => {
            transaction
                .rollback()
                .await
                .map_err(TransactionError::Connection)?;
            Err(TransactionError::Transaction(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_plain_read;

    #[test]
    fn only_a_leading_select_counts_as_a_plain_read() {
        for sql in [
            "SELECT 1",
            "  select \"id\" FROM \"session\"",
            "\nSELECT(1)",
            "Select * from event",
        ] {
            assert!(is_plain_read(sql), "{sql}");
        }
        for sql in [
            "INSERT INTO event (seq) VALUES (1) RETURNING seq",
            "UPDATE session SET spawn_epoch = 2 RETURNING spawn_epoch",
            "WITH doomed AS (SELECT 1) DELETE FROM event",
            "DELETE FROM event",
            "PRAGMA table_info(event)",
            "SELECTED",
            "",
        ] {
            assert!(!is_plain_read(sql), "{sql}");
        }
    }
}
