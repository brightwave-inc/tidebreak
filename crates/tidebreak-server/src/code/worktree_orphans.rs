//! What a reset is about to lose track of.
//!
//! Code worktrees do not live in the data directory. Decision 53 moved them to
//! a user-visible root on purpose, because a worktree is user work —
//! uncommitted code on a real branch. The reset in [`crate::desktop_schema`]
//! deletes the database and nothing outside the data directory, so it drops
//! the `code_workspace` rows and leaves the trees behind: directories full of
//! possibly-uncommitted work, entries in each source repository's
//! `.git/worktrees`, and branches on the user's real repository.
//!
//! Only a profile below the migration pin still takes a reset (decision 61),
//! so this runs once per such profile and then never again.
//!
//! Deleting them is not the answer, and neither is silence. So the reset
//! writes them down. Right before the database goes, read the rows that name
//! the trees and record them in a sidecar file beside the schema marker, which
//! survives a reset by design. The trees are still orphaned afterwards, but
//! they are now recoverable by hand, and adoptable by a first-run surface
//! later.
//!
//! Everything here is best effort. It runs against a database whose schema
//! this binary no longer matches, on a path that has to end with the app
//! booting, so a scan that fails is reported and never blocks the reset.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
use serde::{Deserialize, Serialize};

/// Sidecar naming the trees a reset left on disk.
pub(crate) const ORPHANED_WORKTREES_FILE: &str = "orphaned-code-worktrees.json";

/// Where an unreadable sidecar is moved before a fresh one replaces it, so a
/// record we cannot parse is still not a record we destroyed.
const PREVIOUS_WORKTREES_FILE: &str = "orphaned-code-worktrees.previous.json";

/// Every code worktree a reset is known to have left behind, oldest first.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OrphanedWorktrees {
    pub(crate) worktrees: Vec<OrphanedWorktree>,
}

/// One tree on disk that no `code_workspace` row points at any more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OrphanedWorktree {
    /// Absolute path to the tree, which is still on disk.
    pub(crate) worktree_path: String,
    /// Branch the workspace created in the source repository.
    pub(crate) branch_name: Option<String>,
    /// Root of the repository whose `.git/worktrees` still lists the tree.
    pub(crate) repo_root_path: Option<String>,
    /// What the workspace was called, to make the record readable.
    pub(crate) title: Option<String>,
    /// When the reset that orphaned this tree ran.
    pub(crate) recorded_at: DateTime<Utc>,
}

/// Record every code worktree still on disk that `database` is the last record
/// of, merging into whatever earlier resets already recorded.
///
/// Reports rather than fails: the caller is a reset that has to end with a
/// bootable app.
pub(crate) async fn record_orphaned_worktrees(database: &Path, data_dir: &Path) {
    let found = match scan(database).await {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(
                database = %database.display(),
                %error,
                "could not read code worktrees out of the database this epoch reset is about to \
                 delete; any worktrees it recorded are orphaned without a record"
            );
            return;
        }
    };
    if found.is_empty() {
        return;
    }
    let sidecar = data_dir.join(ORPHANED_WORKTREES_FILE);
    if let Err(error) = merge_into_sidecar(&sidecar, found.clone()) {
        tracing::error!(
            sidecar = %sidecar.display(),
            %error,
            count = found.len(),
            "failed to record the code worktrees this epoch reset orphaned; they are still on \
             disk and now have no record"
        );
        return;
    }
    tracing::warn!(
        sidecar = %sidecar.display(),
        count = found.len(),
        "this epoch reset dropped the workspaces owning code worktrees that are still on disk; \
         their paths, branches, and source repositories are recorded in the sidecar, and nothing \
         removes the trees, their `.git/worktrees` entries, or their branches"
    );
}

/// Read the doomed database's worktree rows, keeping only trees still on disk.
async fn scan(database: &Path) -> Result<Vec<OrphanedWorktree>, String> {
    if !database.exists() {
        return Ok(Vec::new());
    }
    // Read-only, so a scan can never create or migrate the file it is about to
    // delete.
    let url = format!("sqlite://{}?mode=ro", database.display());
    let connection = Database::connect(&url)
        .await
        .map_err(|error| error.to_string())?;

    // `SELECT *` because this database predates the current schema by an
    // unknown number of epochs: naming a column that no longer exists fails the
    // whole scan, and the join fails outright before code repositories existed.
    let joined = "SELECT w.*, r.root_path AS repo_root_path \
                  FROM code_workspace w LEFT JOIN code_repo r ON r.id = w.repo_id";
    let rows = match query(&connection, joined).await {
        Ok(rows) => Ok(rows),
        Err(_) => query(&connection, "SELECT * FROM code_workspace").await,
    };
    // Close before returning either way: Windows refuses to delete a file this
    // process still has open, and the caller deletes this one next. Yield so
    // the current-thread runtime can finish dropping WAL mappings before that
    // delete runs.
    let _ = connection.close().await;
    tokio::task::yield_now().await;

    let recorded_at = Utc::now();
    let mut found = Vec::new();
    for row in rows.map_err(|error| error.to_string())? {
        let Ok(worktree_path) = row.try_get::<String>("", "worktree_path") else {
            continue;
        };
        // A workspace whose tree is already gone is not an orphan.
        if !Path::new(&worktree_path).is_dir() {
            continue;
        }
        found.push(OrphanedWorktree {
            worktree_path,
            branch_name: row.try_get("", "branch_name").ok(),
            repo_root_path: row.try_get("", "repo_root_path").ok(),
            title: row.try_get("", "title").ok(),
            recorded_at,
        });
    }
    Ok(found)
}

async fn query(
    connection: &sea_orm::DatabaseConnection,
    sql: &str,
) -> Result<Vec<sea_orm::QueryResult>, sea_orm::DbErr> {
    connection
        .query_all_raw(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
}

/// Add `found` to the sidecar, keeping the first record of each tree.
///
/// Epoch bumps come in runs, and each one can orphan trees an earlier one
/// already recorded. The earliest record is the honest one: it says when the
/// tree stopped being a workspace.
fn merge_into_sidecar(sidecar: &Path, found: Vec<OrphanedWorktree>) -> std::io::Result<()> {
    let mut record = read_sidecar(sidecar)?;
    let mut known: BTreeSet<String> = record
        .worktrees
        .iter()
        .map(|worktree| worktree.worktree_path.clone())
        .collect();
    for worktree in found {
        if known.insert(worktree.worktree_path.clone()) {
            record.worktrees.push(worktree);
        }
    }
    write_sidecar(sidecar, &record)
}

/// Read what earlier resets recorded.
///
/// A sidecar we cannot parse is moved aside rather than overwritten: it is
/// still the only pointer to somebody's uncommitted work.
fn read_sidecar(sidecar: &Path) -> std::io::Result<OrphanedWorktrees> {
    let bytes = match std::fs::read(sidecar) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OrphanedWorktrees::default())
        }
        Err(error) => return Err(error),
    };
    match serde_json::from_slice(&bytes) {
        Ok(record) => Ok(record),
        Err(error) => {
            let previous = sidecar.with_file_name(PREVIOUS_WORKTREES_FILE);
            tracing::warn!(
                sidecar = %sidecar.display(),
                previous = %previous.display(),
                %error,
                "could not parse the record of previously orphaned code worktrees; keeping it \
                 beside the new one"
            );
            crate::desktop_schema::replace_file(sidecar, &previous)?;
            Ok(OrphanedWorktrees::default())
        }
    }
}

fn write_sidecar(sidecar: &Path, record: &OrphanedWorktrees) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    let temporary: PathBuf = sidecar.with_file_name(format!(
        ".{ORPHANED_WORKTREES_FILE}.{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let result = std::fs::write(&temporary, &bytes)
        .and_then(|()| crate::desktop_schema::replace_file(&temporary, sidecar));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
