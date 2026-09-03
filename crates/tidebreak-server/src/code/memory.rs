//! Materialize owner memory into a code session's private read root.
//!
//! Files land under `{private_root}/memory/`, never in the worktree. The
//! digest and record files are deterministic: an unchanged store writes
//! byte-identical markdown.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use tidebreak_core::{
    render_memory_record_markdown, MemoryBackend, MemoryListFilter, MemoryRecord, MemoryScope,
    MemoryStatus, OwnerId, RepoId,
};

use super::scratch::{self, ScratchRoot};

pub(crate) const MEMORY_DIR: &str = "memory";
pub(crate) const MEMORY_INDEX: &str = "MEMORY.md";

/// Absolute directory the first-turn engine text names once.
#[must_use]
pub(crate) fn memory_dir_path(private_root: &ScratchRoot) -> PathBuf {
    private_root.path().join(MEMORY_DIR)
}

/// Write the owner's digest and active records into `private_root/memory`.
pub(crate) async fn materialize_session_memory(
    backend: &dyn MemoryBackend,
    owner: &OwnerId,
    repo_id: Option<RepoId>,
    private_root: &ScratchRoot,
) -> io::Result<PathBuf> {
    let dir = scratch::scratch_dir(private_root, MEMORY_DIR)?;

    let mut files: Vec<(String, String)> = Vec::new();
    let mut digest_parts = Vec::new();
    let personal = backend
        .assemble_context(owner, MemoryScope::Personal)
        .await
        .map_err(|err| io::Error::other(err.to_string()))?;
    if !personal.markdown.is_empty() {
        digest_parts.push(personal.markdown);
    }
    if let Some(repo_id) = repo_id {
        match backend
            .assemble_context(owner, MemoryScope::Repo { repo_id })
            .await
        {
            Ok(digest) if !digest.markdown.is_empty() => digest_parts.push(digest.markdown),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    files.push((MEMORY_INDEX.to_owned(), digest_parts.join("\n")));

    let mut records = list_active(backend, owner, MemoryScope::Personal).await?;
    if let Some(repo_id) = repo_id {
        records.extend(list_active(backend, owner, MemoryScope::Repo { repo_id }).await?);
    }
    records.sort_by_key(|record| record.id.0);
    for record in records {
        files.push((
            record_file_name(&record),
            render_memory_record_markdown(&record),
        ));
    }

    files.sort_by_key(|(name, _)| name.clone());
    // Every store read is done before the old tree goes, so a backend
    // fault leaves the previous files in place for the path the first
    // turn already named.
    clear_memory_dir(&dir)?;
    for (name, body) in files {
        dir.publish(OsStr::new(&name), body.as_bytes()).await?;
    }
    Ok(memory_dir_path(private_root))
}

async fn list_active(
    backend: &dyn MemoryBackend,
    owner: &OwnerId,
    scope: MemoryScope,
) -> io::Result<Vec<MemoryRecord>> {
    backend
        .list(
            owner,
            MemoryListFilter {
                scope: Some(scope),
                statuses: vec![MemoryStatus::Active],
                kinds: Vec::new(),
            },
        )
        .await
        .map_err(|err| io::Error::other(err.to_string()))
}

fn record_file_name(record: &MemoryRecord) -> String {
    format!("{}.md", record.id)
}

fn clear_memory_dir(dir: &scratch::ScratchDir) -> io::Result<()> {
    for entry in dir.read_dir()? {
        let entry = entry?;
        let name = entry.file_name();
        let metadata = dir.symlink_metadata(&name)?;
        if metadata.is_dir() {
            dir.remove_dir_all(&name)?;
        } else {
            dir.remove_file(&name)?;
        }
    }
    Ok(())
}

/// First-turn sentence that names the materialized path once.
#[must_use]
pub(crate) fn first_turn_memory_line(memory_dir: &Path) -> String {
    format!(
        "Tidebreak memory for this session is in {}. Read MEMORY.md first. These are dated point-in-time claims; this conversation is newer evidence.",
        memory_dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tempfile::TempDir;
    use tidebreak_core::{
        MemoryAuthor, MemoryKind, MemoryOrigin, MemoryProvenance, MemoryRecordId,
        MemoryWriteReceipt,
    };
    use uuid::Uuid;

    struct FixedBackend {
        records: Vec<MemoryRecord>,
        /// When set, every read fails, as a store outage would.
        failing: bool,
    }

    #[async_trait::async_trait]
    impl MemoryBackend for FixedBackend {
        fn caps(&self) -> tidebreak_core::MemoryCaps {
            tidebreak_core::MemoryCaps {
                extraction: tidebreak_core::MemoryCapLevel::Unsupported,
                lexical_search: tidebreak_core::MemoryCapLevel::Supported,
                semantic_search: tidebreak_core::MemoryCapLevel::Unsupported,
                consolidation: tidebreak_core::MemoryCapLevel::Unsupported,
                context_assembly: tidebreak_core::MemoryCapLevel::Supported,
                revision_history: tidebreak_core::MemoryCapLevel::Unsupported,
                verified_delete: tidebreak_core::MemoryCapLevel::Unsupported,
                asynchronous_writes: tidebreak_core::MemoryCapLevel::Unsupported,
                agent_editable_surfaces: tidebreak_core::MemoryCapLevel::Unsupported,
            }
        }

        async fn put(
            &self,
            _owner: &OwnerId,
            _record: MemoryRecord,
        ) -> tidebreak_core::MemoryResult<MemoryWriteReceipt> {
            unimplemented!()
        }

        async fn ingest(
            &self,
            _owner: &OwnerId,
            _request: tidebreak_core::MemoryIngestRequest,
        ) -> tidebreak_core::MemoryResult<tidebreak_core::MemoryIngestReceipt> {
            Err(tidebreak_core::MemoryError::Unsupported(
                tidebreak_core::MemoryCapability::Extraction,
            ))
        }

        async fn get(
            &self,
            _owner: &OwnerId,
            _id: MemoryRecordId,
        ) -> tidebreak_core::MemoryResult<Option<MemoryRecord>> {
            Ok(None)
        }

        async fn list(
            &self,
            _owner: &OwnerId,
            filter: MemoryListFilter,
        ) -> tidebreak_core::MemoryResult<Vec<MemoryRecord>> {
            if self.failing {
                return Err(tidebreak_core::MemoryError::Unsupported(
                    tidebreak_core::MemoryCapability::ContextAssembly,
                ));
            }
            Ok(self
                .records
                .iter()
                .filter(|record| {
                    filter.scope.is_none_or(|scope| record.scope == scope)
                        && (filter.statuses.is_empty() || filter.statuses.contains(&record.status))
                })
                .cloned()
                .collect())
        }

        async fn update(
            &self,
            _owner: &OwnerId,
            _update: tidebreak_core::MemoryRecordUpdate,
        ) -> tidebreak_core::MemoryResult<MemoryWriteReceipt> {
            unimplemented!()
        }

        async fn set_status(
            &self,
            _owner: &OwnerId,
            _change: tidebreak_core::MemoryStatusChange,
        ) -> tidebreak_core::MemoryResult<MemoryWriteReceipt> {
            unimplemented!()
        }

        async fn delete(
            &self,
            _owner: &OwnerId,
            _id: MemoryRecordId,
        ) -> tidebreak_core::MemoryResult<bool> {
            Ok(false)
        }

        async fn search(
            &self,
            _owner: &OwnerId,
            _request: tidebreak_core::MemorySearchRequest,
        ) -> tidebreak_core::MemoryResult<Vec<tidebreak_core::MemorySearchHit>> {
            Ok(Vec::new())
        }

        async fn assemble_context(
            &self,
            owner: &OwnerId,
            scope: MemoryScope,
        ) -> tidebreak_core::MemoryResult<tidebreak_core::MemoryDigest> {
            let records = self
                .list(
                    owner,
                    MemoryListFilter {
                        scope: Some(scope),
                        statuses: vec![MemoryStatus::Active],
                        kinds: Vec::new(),
                    },
                )
                .await?;
            let markdown = if records.is_empty() {
                String::new()
            } else {
                let mut markdown = String::from(
                    "## Tidebreak memory\n\nThese are dated point-in-time claims from Tidebreak. Treat this conversation as newer evidence.\n",
                );
                for record in &records {
                    markdown.push_str("- ");
                    markdown.push_str(&record.updated_at.format("%Y-%m-%d").to_string());
                    markdown.push_str(" — ");
                    markdown.push_str(&record.title);
                    markdown.push('\n');
                }
                markdown
            };
            Ok(tidebreak_core::MemoryDigest {
                scope,
                markdown: markdown.clone(),
                byte_len: markdown.len(),
                byte_cap: 8192,
                record_count: records.len(),
            })
        }

        async fn revision_history(
            &self,
            _owner: &OwnerId,
            _id: MemoryRecordId,
        ) -> tidebreak_core::MemoryResult<Vec<tidebreak_core::MemoryRevision>> {
            Ok(Vec::new())
        }
    }

    fn sample_record() -> MemoryRecord {
        let at = Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap();
        MemoryRecord {
            id: MemoryRecordId(Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap()),
            scope: MemoryScope::Personal,
            kind: MemoryKind::Fact,
            status: MemoryStatus::Active,
            title: "When cutting a release".to_owned(),
            body: "Run the smoke test first.".to_owned(),
            provenance: MemoryProvenance {
                author: MemoryAuthor::User,
                origin: MemoryOrigin::default(),
                evidence: Vec::new(),
            },
            links: Vec::new(),
            expires_at: None,
            superseded_by: None,
            observation_count: 0,
            revision: 1,
            created_at: at,
            updated_at: at,
        }
    }

    #[tokio::test]
    async fn rematerialize_is_byte_identical() {
        let temp = TempDir::new().unwrap();
        let root = ScratchRoot::open_for_test(temp.path()).unwrap();
        let backend = FixedBackend {
            records: vec![sample_record()],
            failing: false,
        };
        let owner = OwnerId::new("user:alice").unwrap();
        let first = materialize_session_memory(&backend, &owner, None, &root)
            .await
            .unwrap();
        let first_index = std::fs::read(first.join(MEMORY_INDEX)).unwrap();
        let first_record =
            std::fs::read(first.join("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.md")).unwrap();
        let second = materialize_session_memory(&backend, &owner, None, &root)
            .await
            .unwrap();
        assert_eq!(
            first_index,
            std::fs::read(second.join(MEMORY_INDEX)).unwrap()
        );
        assert_eq!(
            first_record,
            std::fs::read(second.join("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.md")).unwrap()
        );
    }

    #[tokio::test]
    async fn a_failing_store_leaves_the_previous_files_in_place() {
        let temp = TempDir::new().unwrap();
        let root = ScratchRoot::open_for_test(temp.path()).unwrap();
        let owner = OwnerId::new("user:alice").unwrap();
        let healthy = FixedBackend {
            records: vec![sample_record()],
            failing: false,
        };
        let dir = materialize_session_memory(&healthy, &owner, None, &root)
            .await
            .unwrap();
        let index = std::fs::read(dir.join(MEMORY_INDEX)).unwrap();
        let record_file = dir.join("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.md");
        assert!(record_file.exists());

        let broken = FixedBackend {
            records: Vec::new(),
            failing: true,
        };
        materialize_session_memory(&broken, &owner, None, &root)
            .await
            .unwrap_err();
        assert_eq!(
            std::fs::read(dir.join(MEMORY_INDEX)).unwrap(),
            index,
            "the index the first turn named survives a store fault"
        );
        assert!(record_file.exists(), "the record files survive too");
    }

    #[tokio::test]
    async fn materialization_stays_outside_the_worktree() {
        let temp = TempDir::new().unwrap();
        let worktree = temp.path().join("repo");
        std::fs::create_dir_all(worktree.join(".git")).unwrap();
        let private = temp.path().join("private");
        std::fs::create_dir_all(&private).unwrap();
        let root = ScratchRoot::open_for_test(&private).unwrap();
        let backend = FixedBackend {
            records: vec![sample_record()],
            failing: false,
        };
        let owner = OwnerId::new("user:alice").unwrap();
        let written = materialize_session_memory(&backend, &owner, None, &root)
            .await
            .unwrap();
        assert!(written.starts_with(&private));
        assert!(!written.starts_with(&worktree));
        assert!(walkdir_empty_of_memory(&worktree));
    }

    fn walkdir_empty_of_memory(root: &Path) -> bool {
        fn walk(path: &Path) -> bool {
            let Ok(entries) = std::fs::read_dir(path) else {
                return true;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name == MEMORY_DIR || name == OsStr::new(MEMORY_INDEX) {
                    return false;
                }
                if entry.path().is_dir() && !walk(&entry.path()) {
                    return false;
                }
            }
            true
        }
        walk(root)
    }
}
