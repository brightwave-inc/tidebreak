//! A durable, embedded [`VectorStore`] backed by LanceDB.
//!
//! LanceDB is a pure-Rust, on-disk vector database (Arrow/Lance columnar format).
//! This is the persistent counterpart to [`crate::InMemoryVectorStore`]: same
//! seam, but the index survives a restart and can grow past what fits in memory,
//! with room for an ANN index and multimodal columns later.
//!
//! Chunks and generation markers live in one table with a fixed-width vector
//! column; scalar columns carry citation data and publication identity. Writes use
//! Lance's transactional merge-insert operation, so replacing or activating a
//! document is published as one dataset version. Cosine distance; LanceDB does a
//! flat (brute-force) scan until an index is built, which is fine for the corpus
//! sizes this targets today.
//!
//! **Build note:** enabled by the non-default `vec-lance` feature. LanceDB pulls a
//! large Arrow/DataFusion tree and needs `protoc` at build time — hence off by
//! default for library consumers. OpenWave's workspace CI installs `protoc`.

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Arc;

use arrow_array::types::Float32Type;
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Int64Array, RecordBatch,
    RecordBatchIterator, StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{DistanceType, Table};
use tokio::sync::Mutex;

use crate::document::{ByteSpan, Chunk, ScoredChunk};
use crate::embed::Embedding;
use crate::error::{Result, RetrievalError};
use crate::id::{ChunkId, DocumentId};
use crate::vector::{
    DocumentGenerationState, GenerationStageOutcome, SearchScope, VectorRecord, VectorStore,
};
use openwave_core::{DocumentGeneration, ProjectId};

/// The single table all chunks and publication markers are stored in.
const TABLE: &str = "chunks";
/// The vector column name.
const VECTOR_COL: &str = "vector";
/// LanceDB's distance column on query results.
const DISTANCE_COL: &str = "_distance";
const UNVERSIONED_CHUNK: &str = "unversioned_chunk";
const STAGED_CHUNK: &str = "staged_chunk";
const ACTIVE_CHUNK: &str = "active_chunk";
const STAGED_MARKER: &str = "staged_marker";
const ACTIVE_MARKER: &str = "active_marker";
const VISIBLE_CHUNKS: &str = "row_kind IN ('unversioned_chunk', 'active_chunk')";

/// A persistent [`VectorStore`] backed by a local LanceDB dataset.
///
/// One instance serializes every mutation so marker reads and their following
/// merge commit form one coordinator operation. An exclusive lock file enforces
/// one writer instance per dataset across handles and processes.
pub struct LanceVectorStore {
    table: Table,
    schema: SchemaRef,
    dims: usize,
    write_lock: Mutex<()>,
    _writer_lock: File,
}

struct StoredRow {
    storage_id: String,
    row_kind: &'static str,
    record: Option<VectorRecord>,
    document_id: DocumentId,
    generation: Option<DocumentGeneration>,
}

impl LanceVectorStore {
    /// Open (or create) a LanceDB-backed store at `uri` for vectors of `dims`.
    ///
    /// `uri` is a local directory path; it's created if missing, and an existing
    /// dataset there is reopened (that's what makes the index durable).
    ///
    /// **Schema guardrail:** if an existing table's complete schema does not match
    /// the expected layout or vector width, the old table is dropped and recreated
    /// empty. The index is derived data (re-embeddable from source documents), so
    /// rebuilding is the safe pre-v1 response to an incompatible layout.
    pub async fn connect(uri: &str, dims: usize) -> Result<Self> {
        std::fs::create_dir_all(uri).map_err(lance_err)?;
        let writer_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(Path::new(uri).join(".openwave-writer.lock"))
            .map_err(lance_err)?;
        writer_lock.try_lock().map_err(|error| {
            RetrievalError::vector_store(format!(
                "another Lance vector writer already owns {uri}: {error}"
            ))
        })?;
        let schema = build_schema(dims);
        let db = lancedb::connect(uri).execute().await.map_err(lance_err)?;
        let names = db.table_names().execute().await.map_err(lance_err)?;

        let existing_schema = if names.iter().any(|n| n == TABLE) {
            let table = db.open_table(TABLE).execute().await.map_err(lance_err)?;
            Some(table.schema().await.map_err(lance_err)? as SchemaRef)
        } else {
            None
        };

        let table = match existing_schema {
            Some(existing) if existing.as_ref() == schema.as_ref() => {
                db.open_table(TABLE).execute().await.map_err(lance_err)?
            }
            Some(_) => {
                // The index is derived and pre-v1, so any incompatible layout is
                // reset rather than carrying an in-place data migration.
                db.drop_table(TABLE, &[]).await.map_err(lance_err)?;
                db.create_empty_table(TABLE, schema.clone())
                    .execute()
                    .await
                    .map_err(lance_err)?
            }
            None => db
                .create_empty_table(TABLE, schema.clone())
                .execute()
                .await
                .map_err(lance_err)?,
        };
        Ok(Self {
            table,
            schema,
            dims,
            write_lock: Mutex::new(()),
            _writer_lock: writer_lock,
        })
    }

    /// The dimensionality this store enforces.
    #[must_use]
    pub fn dimensions(&self) -> usize {
        self.dims
    }

    fn check_dims(&self, embedding: &Embedding) -> Result<()> {
        if embedding.dim() != self.dims {
            return Err(RetrievalError::DimensionMismatch {
                expected: self.dims,
                actual: embedding.dim(),
            });
        }
        Ok(())
    }

    fn validate_document_records(
        &self,
        document_id: DocumentId,
        records: &[VectorRecord],
    ) -> Result<()> {
        let project_id = records.first().map(|record| record.project_id);
        for record in records {
            self.check_dims(&record.embedding)?;
            if record.chunk.document_id != document_id {
                return Err(RetrievalError::vector_store(format!(
                    "replacement record {} belongs to document {}, expected {document_id}",
                    record.chunk.id, record.chunk.document_id
                )));
            }
            if Some(record.project_id) != project_id {
                return Err(RetrievalError::vector_store(format!(
                    "records for document {document_id} span multiple project corpora"
                )));
            }
        }
        Ok(())
    }

    fn validate_upsert_records(&self, records: &[VectorRecord]) -> Result<()> {
        let mut projects = std::collections::HashMap::new();
        for record in records {
            self.check_dims(&record.embedding)?;
            match projects.entry(record.chunk.document_id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(record.project_id);
                }
                std::collections::hash_map::Entry::Occupied(entry)
                    if *entry.get() != record.project_id =>
                {
                    return Err(RetrievalError::vector_store(format!(
                        "records for document {} span multiple project corpora",
                        record.chunk.document_id
                    )));
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
        Ok(())
    }

    async fn live_document_project_id(
        &self,
        document_id: DocumentId,
    ) -> Result<Option<Option<ProjectId>>> {
        let mut stream = self
            .table
            .query()
            .only_if(format!(
                "document_id = '{document_id}' AND row_kind IN ('{UNVERSIONED_CHUNK}', '{ACTIVE_CHUNK}', '{STAGED_CHUNK}')"
            ))
            .select(Select::columns(&["project_id"]))
            .execute()
            .await
            .map_err(lance_err)?;
        let mut found = None;
        while let Some(batch) = stream.try_next().await.map_err(lance_err)? {
            let project_ids = str_col(&batch, "project_id")?;
            for index in 0..batch.num_rows() {
                let project_id = if project_ids.is_null(index) {
                    None
                } else {
                    Some(
                        project_ids
                            .value(index)
                            .parse::<ProjectId>()
                            .map_err(|error| {
                                RetrievalError::vector_store(format!(
                                    "document {document_id} has an invalid project_id: {error}"
                                ))
                            })?,
                    )
                };
                if found.is_some_and(|existing| existing != project_id) {
                    return Err(RetrievalError::vector_store(format!(
                        "stored records for document {document_id} span multiple project corpora"
                    )));
                }
                found = Some(project_id);
            }
        }
        Ok(found)
    }

    async fn validate_live_document_scope(
        &self,
        document_id: DocumentId,
        records: &[VectorRecord],
    ) -> Result<()> {
        let Some(requested) = records.first().map(|record| record.project_id) else {
            return Ok(());
        };
        if self
            .live_document_project_id(document_id)
            .await?
            .is_some_and(|existing| existing != requested)
        {
            return Err(RetrievalError::vector_store(format!(
                "document {document_id} cannot move between project corpora while it has live chunks"
            )));
        }
        Ok(())
    }

    fn rows_to_batch(&self, rows: &[StoredRow]) -> Result<RecordBatch> {
        let storage_ids =
            StringArray::from_iter_values(rows.iter().map(|row| row.storage_id.as_str()));
        let row_kinds = StringArray::from_iter_values(rows.iter().map(|row| row.row_kind));
        let chunk_id_values = rows
            .iter()
            .map(|row| {
                row.record.as_ref().map_or_else(
                    || row.storage_id.clone(),
                    |record| record.chunk.id.to_string(),
                )
            })
            .collect::<Vec<_>>();
        let chunk_ids = StringArray::from_iter_values(chunk_id_values.iter().map(String::as_str));
        let doc_id_values = rows
            .iter()
            .map(|row| row.document_id.to_string())
            .collect::<Vec<_>>();
        let doc_ids = StringArray::from_iter_values(doc_id_values.iter().map(String::as_str));
        let project_id_values = rows
            .iter()
            .map(|row| {
                row.record
                    .as_ref()
                    .and_then(|record| record.project_id)
                    .map(|project_id| project_id.to_string())
            })
            .collect::<Vec<_>>();
        let project_ids = StringArray::from_iter(
            project_id_values
                .iter()
                .map(|project_id| project_id.as_deref()),
        );
        let ordinals = UInt64Array::from_iter_values(rows.iter().map(|row| {
            row.record
                .as_ref()
                .map_or(0, |record| record.chunk.ordinal as u64)
        }));
        let texts = StringArray::from_iter_values(rows.iter().map(|row| {
            row.record
                .as_ref()
                .map_or("", |record| record.chunk.text.as_str())
        }));
        let starts = UInt64Array::from_iter_values(rows.iter().map(|row| {
            row.record
                .as_ref()
                .map_or(0, |record| record.chunk.span.start as u64)
        }));
        let ends = UInt64Array::from_iter_values(rows.iter().map(|row| {
            row.record
                .as_ref()
                .map_or(0, |record| record.chunk.span.end as u64)
        }));
        let revisions = Int64Array::from_iter_values(rows.iter().map(|row| {
            row.generation
                .map_or(0, |generation| generation.content_revision)
        }));
        let revision_token_values = rows
            .iter()
            .map(|row| {
                row.generation.map_or_else(String::new, |generation| {
                    generation.revision_token.to_string()
                })
            })
            .collect::<Vec<_>>();
        let revision_tokens =
            StringArray::from_iter_values(revision_token_values.iter().map(String::as_str));
        let vectors = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            rows.iter().map(|row| {
                Some(match row.record.as_ref() {
                    Some(record) => record
                        .embedding
                        .0
                        .iter()
                        .map(|&value| Some(value))
                        .collect::<Vec<_>>(),
                    None => vec![Some(0.0); self.dims],
                })
            }),
            self.dims as i32,
        );
        let columns: Vec<ArrayRef> = vec![
            Arc::new(storage_ids),
            Arc::new(row_kinds),
            Arc::new(chunk_ids),
            Arc::new(doc_ids),
            Arc::new(project_ids),
            Arc::new(ordinals),
            Arc::new(texts),
            Arc::new(starts),
            Arc::new(ends),
            Arc::new(revisions),
            Arc::new(revision_tokens),
            Arc::new(vectors),
        ];
        RecordBatch::try_new(self.schema.clone(), columns).map_err(lance_err)
    }

    async fn merge_rows(&self, rows: &[StoredRow], delete_filter: Option<String>) -> Result<()> {
        let batch = self.rows_to_batch(rows)?;
        let reader = RecordBatchIterator::new(vec![Ok(batch)], self.schema.clone());
        let mut merge = self.table.merge_insert(&["storage_id"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        if let Some(filter) = delete_filter {
            merge.when_not_matched_by_source_delete(Some(filter));
        }
        merge.execute(Box::new(reader)).await.map_err(lance_err)?;
        Ok(())
    }

    async fn generation_marker(
        &self,
        document_id: DocumentId,
        row_kind: &str,
    ) -> Result<Option<DocumentGeneration>> {
        let mut stream = self
            .table
            .query()
            .only_if(format!(
                "document_id = '{document_id}' AND row_kind = '{row_kind}'"
            ))
            .select(Select::columns(&["content_revision", "revision_token"]))
            .execute()
            .await
            .map_err(lance_err)?;
        let mut found = None;
        while let Some(batch) = stream.try_next().await.map_err(lance_err)? {
            let revisions = i64_col(&batch, "content_revision")?;
            let tokens = str_col(&batch, "revision_token")?;
            for index in 0..batch.num_rows() {
                if found.is_some() {
                    return Err(RetrievalError::vector_store(format!(
                        "document {document_id} has duplicate {row_kind} rows"
                    )));
                }
                let content_revision = revisions.value(index);
                if content_revision < 1 {
                    return Err(RetrievalError::vector_store(format!(
                        "document {document_id} has an invalid generation revision {content_revision}"
                    )));
                }
                found = Some(DocumentGeneration {
                    content_revision,
                    revision_token: tokens.value(index).parse().map_err(|error| {
                        RetrievalError::vector_store(format!(
                            "document {document_id} has an invalid generation token: {error}"
                        ))
                    })?,
                });
            }
        }
        Ok(found)
    }

    async fn generation_rows_exist(&self, document_id: DocumentId) -> Result<bool> {
        self.table
            .count_rows(Some(format!(
                "document_id = '{document_id}' AND row_kind != '{UNVERSIONED_CHUNK}'"
            )))
            .await
            .map(|count| count > 0)
            .map_err(lance_err)
    }

    async fn read_records(&self, filter: String) -> Result<Vec<VectorRecord>> {
        let mut stream = self
            .table
            .query()
            .only_if(filter)
            .select(Select::columns(&[
                "chunk_id",
                "document_id",
                "project_id",
                "ordinal",
                "text",
                "span_start",
                "span_end",
                VECTOR_COL,
            ]))
            .execute()
            .await
            .map_err(lance_err)?;
        let mut records = Vec::new();
        while let Some(batch) = stream.try_next().await.map_err(lance_err)? {
            read_vector_records(&batch, &mut records)?;
        }
        Ok(records)
    }
}

fn unversioned_row(record: VectorRecord) -> StoredRow {
    StoredRow {
        storage_id: format!("unversioned:{}", record.chunk.id),
        row_kind: UNVERSIONED_CHUNK,
        document_id: record.chunk.document_id,
        record: Some(record),
        generation: None,
    }
}

fn generation_chunk_row(
    record: VectorRecord,
    generation: DocumentGeneration,
    row_kind: &'static str,
) -> StoredRow {
    StoredRow {
        storage_id: format!(
            "generation:{}:{}:{}:{}",
            record.chunk.document_id,
            generation.content_revision,
            generation.revision_token,
            record.chunk.id
        ),
        row_kind,
        document_id: record.chunk.document_id,
        record: Some(record),
        generation: Some(generation),
    }
}

fn generation_marker_row(
    document_id: DocumentId,
    generation: DocumentGeneration,
    row_kind: &'static str,
) -> StoredRow {
    let marker = match row_kind {
        STAGED_MARKER => "staged-marker",
        ACTIVE_MARKER => "active-marker",
        _ => unreachable!("only generation marker kinds have marker rows"),
    };
    StoredRow {
        storage_id: format!("{marker}:{document_id}"),
        row_kind,
        record: None,
        document_id,
        generation: Some(generation),
    }
}

fn newest_generation(
    document_id: DocumentId,
    active: Option<DocumentGeneration>,
    staged: Option<DocumentGeneration>,
) -> Result<Option<DocumentGeneration>> {
    if let (Some(active), Some(staged)) = (active, staged) {
        if active.content_revision == staged.content_revision
            && active.revision_token != staged.revision_token
        {
            return Err(RetrievalError::vector_store(format!(
                "document {document_id} has conflicting generation markers at revision {}",
                active.content_revision
            )));
        }
        return Ok(Some(
            if staged.content_revision >= active.content_revision {
                staged
            } else {
                active
            },
        ));
    }
    Ok(staged.or(active))
}

#[async_trait]
impl VectorStore for LanceVectorStore {
    async fn upsert(&self, records: Vec<VectorRecord>) -> Result<()> {
        self.validate_upsert_records(&records)?;
        let records = dedupe_by_chunk_id(records);
        if records.is_empty() {
            return Ok(());
        }
        let _write = self.write_lock.lock().await;
        let mut documents = std::collections::HashMap::new();
        for record in &records {
            documents
                .entry(record.chunk.document_id)
                .or_insert_with(Vec::new)
                .push(record.clone());
        }
        for (document_id, document_records) in documents {
            self.validate_live_document_scope(document_id, &document_records)
                .await?;
            if self.generation_rows_exist(document_id).await? {
                return Err(RetrievalError::vector_store(
                    "legacy upsert cannot modify a generation-managed document",
                ));
            }
        }
        let rows = records.into_iter().map(unversioned_row).collect::<Vec<_>>();
        self.merge_rows(&rows, None).await
    }

    async fn query(
        &self,
        query: &Embedding,
        k: usize,
        scope: SearchScope,
    ) -> Result<Vec<ScoredChunk>> {
        self.check_dims(query)?;
        if k == 0 {
            return Ok(Vec::new());
        }
        // Lance applies this predicate before the vector limit, so a closer row
        // in another corpus cannot consume one of this scope's top-k slots.
        // ProjectId is a parsed UUID newtype, not caller-provided query text.
        let scope_filter = match scope {
            SearchScope::Unscoped => "project_id IS NULL".to_string(),
            SearchScope::Project(project_id) => format!("project_id = '{project_id}'"),
        };
        let mut stream = self
            .table
            .query()
            .nearest_to(query.0.as_slice())
            .map_err(lance_err)?
            .only_if(format!("({VISIBLE_CHUNKS}) AND ({scope_filter})"))
            .column(VECTOR_COL)
            .distance_type(DistanceType::Cosine)
            .limit(k)
            .execute()
            .await
            .map_err(lance_err)?;

        let mut out = Vec::new();
        while let Some(batch) = stream.try_next().await.map_err(lance_err)? {
            read_batch(&batch, &mut out)?;
        }
        Ok(out)
    }

    async fn replace_document(
        &self,
        document_id: DocumentId,
        records: Vec<VectorRecord>,
    ) -> Result<()> {
        self.validate_document_records(document_id, &records)?;
        let records = dedupe_by_chunk_id(records);
        let _write = self.write_lock.lock().await;
        self.validate_live_document_scope(document_id, &records)
            .await?;
        if self.generation_rows_exist(document_id).await? {
            return Err(RetrievalError::vector_store(
                "legacy replacement cannot modify a generation-managed document",
            ));
        }
        let rows = records.into_iter().map(unversioned_row).collect::<Vec<_>>();
        self.merge_rows(&rows, Some(format!("document_id = '{document_id}'")))
            .await
    }

    async fn stage_document_generation(
        &self,
        document_id: DocumentId,
        generation: DocumentGeneration,
        records: Vec<VectorRecord>,
    ) -> Result<GenerationStageOutcome> {
        if generation.content_revision < 1 {
            return Err(RetrievalError::vector_store(
                "document generation revision must be at least one",
            ));
        }
        self.validate_document_records(document_id, &records)?;
        let records = dedupe_by_chunk_id(records);
        let _write = self.write_lock.lock().await;
        if !records.is_empty() {
            self.validate_live_document_scope(document_id, &records)
                .await?;
        }
        let active = self.generation_marker(document_id, ACTIVE_MARKER).await?;
        let staged = self.generation_marker(document_id, STAGED_MARKER).await?;
        if let Some(current) = newest_generation(document_id, active, staged)? {
            match generation.content_revision.cmp(&current.content_revision) {
                std::cmp::Ordering::Less => {
                    return Ok(GenerationStageOutcome::Rejected { current });
                }
                std::cmp::Ordering::Equal => {
                    if generation.revision_token != current.revision_token {
                        return Err(RetrievalError::vector_store(format!(
                            "document {document_id} generation {} has conflicting revision tokens",
                            generation.content_revision
                        )));
                    }
                    return Ok(GenerationStageOutcome::AlreadyPresent);
                }
                std::cmp::Ordering::Greater => {}
            }
        }

        let mut rows = records
            .into_iter()
            .map(|record| generation_chunk_row(record, generation, STAGED_CHUNK))
            .collect::<Vec<_>>();
        rows.push(generation_marker_row(
            document_id,
            generation,
            STAGED_MARKER,
        ));
        self.merge_rows(
            &rows,
            Some(format!(
                "document_id = '{document_id}' AND row_kind IN ('{STAGED_CHUNK}', '{STAGED_MARKER}')"
            )),
        )
        .await?;
        Ok(GenerationStageOutcome::Staged)
    }

    async fn activate_document_generation(
        &self,
        document_id: DocumentId,
        generation: DocumentGeneration,
    ) -> Result<bool> {
        let _write = self.write_lock.lock().await;
        let staged = self.generation_marker(document_id, STAGED_MARKER).await?;
        if let Some(staged) = staged {
            if staged != generation {
                return Ok(false);
            }
            let active = self.generation_marker(document_id, ACTIVE_MARKER).await?;
            if newest_generation(document_id, active, Some(staged))? != Some(staged) {
                return Ok(false);
            }
            let records = self
                .read_records(format!(
                    "document_id = '{document_id}' AND row_kind = '{STAGED_CHUNK}' AND content_revision = {} AND revision_token = '{}'",
                    generation.content_revision, generation.revision_token
                ))
                .await?;
            let mut rows = records
                .into_iter()
                .map(|record| generation_chunk_row(record, generation, ACTIVE_CHUNK))
                .collect::<Vec<_>>();
            rows.push(generation_marker_row(
                document_id,
                generation,
                ACTIVE_MARKER,
            ));
            self.merge_rows(&rows, Some(format!("document_id = '{document_id}'")))
                .await?;
            return Ok(true);
        }
        Ok(self
            .generation_marker(document_id, ACTIVE_MARKER)
            .await?
            .is_some_and(|active| active == generation))
    }

    async fn active_document_generation(
        &self,
        document_id: DocumentId,
    ) -> Result<Option<DocumentGeneration>> {
        self.generation_marker(document_id, ACTIVE_MARKER).await
    }

    async fn newest_document_generation(
        &self,
        document_id: DocumentId,
    ) -> Result<Option<DocumentGenerationState>> {
        let _read = self.write_lock.lock().await;
        let active = self.generation_marker(document_id, ACTIVE_MARKER).await?;
        let staged = self.generation_marker(document_id, STAGED_MARKER).await?;
        let newest = newest_generation(document_id, active, staged)?;
        Ok(match newest {
            Some(generation) if staged == Some(generation) => {
                Some(DocumentGenerationState::Staged(generation))
            }
            Some(generation) => Some(DocumentGenerationState::Active(generation)),
            None => None,
        })
    }

    async fn document_len(&self, document_id: DocumentId) -> Result<Option<usize>> {
        self.table
            .count_rows(Some(format!(
                "document_id = '{document_id}' AND {VISIBLE_CHUNKS}"
            )))
            .await
            .map(Some)
            .map_err(lance_err)
    }

    async fn len(&self) -> Result<usize> {
        self.table
            .count_rows(Some(VISIBLE_CHUNKS.to_string()))
            .await
            .map_err(lance_err)
    }
}

/// Collapse records with the same chunk id to the last occurrence, preserving
/// first-seen order — so a batch carrying a duplicate id stores one row, not two.
fn dedupe_by_chunk_id(records: Vec<VectorRecord>) -> Vec<VectorRecord> {
    let mut index = std::collections::HashMap::new();
    let mut out: Vec<VectorRecord> = Vec::with_capacity(records.len());
    for record in records {
        if let Some(&i) = index.get(&record.chunk.id) {
            out[i] = record;
        } else {
            index.insert(record.chunk.id, out.len());
            out.push(record);
        }
    }
    out
}

/// The table schema: scalar citation columns plus the fixed-width vector column.
fn build_schema(dims: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("storage_id", DataType::Utf8, false),
        Field::new("row_kind", DataType::Utf8, false),
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("document_id", DataType::Utf8, false),
        Field::new("project_id", DataType::Utf8, true),
        Field::new("ordinal", DataType::UInt64, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("span_start", DataType::UInt64, false),
        Field::new("span_end", DataType::UInt64, false),
        Field::new("content_revision", DataType::Int64, false),
        Field::new("revision_token", DataType::Utf8, false),
        Field::new(
            VECTOR_COL,
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dims as i32,
            ),
            false,
        ),
    ]))
}

fn read_vector_records(batch: &RecordBatch, out: &mut Vec<VectorRecord>) -> Result<()> {
    let chunk_ids = str_col(batch, "chunk_id")?;
    let doc_ids = str_col(batch, "document_id")?;
    let project_ids = str_col(batch, "project_id")?;
    let ordinals = u64_col(batch, "ordinal")?;
    let texts = str_col(batch, "text")?;
    let starts = u64_col(batch, "span_start")?;
    let ends = u64_col(batch, "span_end")?;
    let vectors = batch
        .column_by_name(VECTOR_COL)
        .and_then(|column| column.as_any().downcast_ref::<FixedSizeListArray>())
        .ok_or_else(|| RetrievalError::vector_store("missing/mistyped column `vector`"))?;

    for index in 0..batch.num_rows() {
        let chunk_id: ChunkId = chunk_ids
            .value(index)
            .parse()
            .map_err(|error| RetrievalError::vector_store(format!("bad chunk_id: {error}")))?;
        let document_id: DocumentId = doc_ids
            .value(index)
            .parse()
            .map_err(|error| RetrievalError::vector_store(format!("bad document_id: {error}")))?;
        let project_id = if project_ids.is_null(index) {
            None
        } else {
            Some(
                project_ids
                    .value(index)
                    .parse::<ProjectId>()
                    .map_err(|error| {
                        RetrievalError::vector_store(format!("bad project_id: {error}"))
                    })?,
            )
        };
        let values = vectors.value(index);
        let values = values
            .as_any()
            .downcast_ref::<Float32Array>()
            .ok_or_else(|| RetrievalError::vector_store("vector values are not float32"))?;
        out.push(VectorRecord {
            project_id,
            chunk: Chunk {
                id: chunk_id,
                document_id,
                ordinal: ordinals.value(index) as usize,
                text: texts.value(index).to_string(),
                span: ByteSpan::new(starts.value(index) as usize, ends.value(index) as usize),
            },
            embedding: Embedding(values.values().to_vec()),
        });
    }
    Ok(())
}

/// Decode one result batch into [`ScoredChunk`]s, converting cosine distance to
/// similarity (`1 - distance`) so scores match the crate's `[-1, 1]` convention.
fn read_batch(batch: &RecordBatch, out: &mut Vec<ScoredChunk>) -> Result<()> {
    let chunk_ids = str_col(batch, "chunk_id")?;
    let doc_ids = str_col(batch, "document_id")?;
    let ordinals = u64_col(batch, "ordinal")?;
    let texts = str_col(batch, "text")?;
    let starts = u64_col(batch, "span_start")?;
    let ends = u64_col(batch, "span_end")?;
    let distances = batch
        .column_by_name(DISTANCE_COL)
        .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
        .ok_or_else(|| RetrievalError::vector_store("query result missing _distance column"))?;

    for i in 0..batch.num_rows() {
        let chunk_id: ChunkId = chunk_ids
            .value(i)
            .parse()
            .map_err(|e| RetrievalError::vector_store(format!("bad chunk_id: {e}")))?;
        let document_id: DocumentId = doc_ids
            .value(i)
            .parse()
            .map_err(|e| RetrievalError::vector_store(format!("bad document_id: {e}")))?;
        let span = ByteSpan::new(starts.value(i) as usize, ends.value(i) as usize);
        out.push(ScoredChunk {
            chunk: Chunk {
                id: chunk_id,
                document_id,
                ordinal: ordinals.value(i) as usize,
                text: texts.value(i).to_string(),
                span,
            },
            score: 1.0 - distances.value(i),
        });
    }
    Ok(())
}

fn str_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| RetrievalError::vector_store(format!("missing/mistyped column `{name}`")))
}

fn u64_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| RetrievalError::vector_store(format!("missing/mistyped column `{name}`")))
}

fn i64_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Int64Array> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| RetrievalError::vector_store(format!("missing/mistyped column `{name}`")))
}

fn lance_err(err: impl std::fmt::Display) -> RetrievalError {
    RetrievalError::vector_store(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(doc: DocumentId, ordinal: usize, text: &str, vector: Vec<f32>) -> VectorRecord {
        let span = ByteSpan::new(ordinal * 100, ordinal * 100 + text.len());
        VectorRecord {
            project_id: None,
            chunk: Chunk::new(doc, ordinal, span, text),
            embedding: Embedding(vector),
        }
    }

    fn project_record(
        project_id: ProjectId,
        doc: DocumentId,
        ordinal: usize,
        text: &str,
        vector: Vec<f32>,
    ) -> VectorRecord {
        VectorRecord {
            project_id: Some(project_id),
            ..record(doc, ordinal, text, vector)
        }
    }

    fn generation(revision: i64) -> DocumentGeneration {
        DocumentGeneration {
            content_revision: revision,
            revision_token: uuid::Uuid::from_u128(revision as u128),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_enforces_one_writer_per_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();
        let first = LanceVectorStore::connect(uri, 2).await.unwrap();
        let error = match LanceVectorStore::connect(uri, 2).await {
            Ok(_) => panic!("a second writer unexpectedly acquired the dataset"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already owns"));

        drop(first);
        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert!(reopened.is_empty().await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ingest_query_replace_and_persist_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();
        let doc = DocumentId::new();

        {
            let store = LanceVectorStore::connect(uri, 2).await.unwrap();
            assert!(store.is_empty().await.unwrap());
            store
                .upsert(vec![
                    record(doc, 0, "east", vec![1.0, 0.0]),
                    record(doc, 1, "north", vec![0.0, 1.0]),
                ])
                .await
                .unwrap();
            assert_eq!(store.len().await.unwrap(), 2);
            assert_eq!(store.document_len(doc).await.unwrap(), Some(2));
            assert_eq!(
                store.document_len(DocumentId::new()).await.unwrap(),
                Some(0)
            );

            // Nearest to "east" is the east vector, with its span/text intact.
            let hits = store
                .query(&Embedding(vec![1.0, 0.0]), 1, SearchScope::Unscoped)
                .await
                .unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].chunk.text, "east");
            assert_eq!(hits[0].chunk.document_id, doc);
            assert!(hits[0].score > 0.9);

            // A replacement is published as exactly one Lance dataset version.
            let version_before_replace = store.table.version().await.unwrap();
            store
                .replace_document(doc, vec![record(doc, 0, "south", vec![1.0, 0.0])])
                .await
                .unwrap();
            assert_eq!(
                store.table.version().await.unwrap(),
                version_before_replace + 1
            );
            assert_eq!(store.len().await.unwrap(), 1);
            let hits = store
                .query(&Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
                .await
                .unwrap();
            assert_eq!(hits[0].chunk.text, "south");
        }

        // Reopen the same directory: the data is still there — that's durability.
        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert_eq!(reopened.len().await.unwrap(), 1);
        let hits = reopened
            .query(&Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits[0].chunk.text, "south");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_filters_corpus_before_top_k_and_scope_persists_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        let unscoped_doc = DocumentId::new();
        let a_doc = DocumentId::new();
        let b_doc = DocumentId::new();

        {
            let store = LanceVectorStore::connect(uri, 2).await.unwrap();
            store
                .upsert(vec![
                    record(unscoped_doc, 0, "unscoped", vec![0.8, 0.2]),
                    project_record(project_a, a_doc, 0, "project-a", vec![0.9, 0.1]),
                    // Globally closest, but it must not consume project A's k=1.
                    project_record(project_b, b_doc, 0, "project-b", vec![1.0, 0.0]),
                ])
                .await
                .unwrap();

            let query = Embedding(vec![1.0, 0.0]);
            let unscoped = store.query(&query, 1, SearchScope::Unscoped).await.unwrap();
            let a = store
                .query(&query, 1, SearchScope::Project(project_a))
                .await
                .unwrap();
            let b = store
                .query(&query, 1, SearchScope::Project(project_b))
                .await
                .unwrap();
            assert_eq!(unscoped[0].chunk.text, "unscoped");
            assert_eq!(a[0].chunk.text, "project-a");
            assert_eq!(b[0].chunk.text, "project-b");
        }

        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        let query = Embedding(vec![1.0, 0.0]);
        assert_eq!(
            reopened
                .query(&query, 1, SearchScope::Unscoped)
                .await
                .unwrap()[0]
                .chunk
                .text,
            "unscoped"
        );
        assert_eq!(
            reopened
                .query(&query, 1, SearchScope::Project(project_a))
                .await
                .unwrap()[0]
                .chunk
                .text,
            "project-a"
        );
        assert_eq!(
            reopened
                .query(&query, 1, SearchScope::Project(project_b))
                .await
                .unwrap()[0]
                .chunk
                .text,
            "project-b"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn generation_activation_preserves_project_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        let a_doc = DocumentId::new();
        let b_doc = DocumentId::new();
        let first = generation(1);

        store
            .upsert(vec![project_record(
                project_b,
                b_doc,
                0,
                "project-b",
                vec![1.0, 0.0],
            )])
            .await
            .unwrap();
        store
            .stage_document_generation(
                a_doc,
                first,
                vec![project_record(
                    project_a,
                    a_doc,
                    0,
                    "project-a-active",
                    vec![0.9, 0.1],
                )],
            )
            .await
            .unwrap();

        let query = Embedding(vec![1.0, 0.0]);
        assert!(store
            .query(&query, 1, SearchScope::Project(project_a))
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .activate_document_generation(a_doc, first)
            .await
            .unwrap());
        assert_eq!(
            store
                .query(&query, 1, SearchScope::Project(project_a))
                .await
                .unwrap()[0]
                .chunk
                .text,
            "project-a-active"
        );
        assert_eq!(
            store
                .query(&query, 1, SearchScope::Project(project_b))
                .await
                .unwrap()[0]
                .chunk
                .text,
            "project-b"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_paths_dedupe_same_chunk_id_within_a_batch() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let doc = DocumentId::new();
        // Two records with the same derived chunk id (same span) => one row, last wins.
        let a = record(doc, 0, "old", vec![1.0, 0.0]);
        let b = record(doc, 0, "new", vec![0.0, 1.0]);
        assert_eq!(a.chunk.id, b.chunk.id);

        let version = store.table.version().await.unwrap();
        store.upsert(vec![a.clone(), b.clone()]).await.unwrap();
        assert_eq!(store.table.version().await.unwrap(), version + 1);
        assert_eq!(store.len().await.unwrap(), 1);

        store.replace_document(doc, vec![a, b]).await.unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
        let hits = store
            .query(&Embedding(vec![0.0, 1.0]), 5, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "new");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reopening_with_different_dims_rebuilds_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();

        // Fill a 2-dim index.
        {
            let store = LanceVectorStore::connect(uri, 2).await.unwrap();
            store
                .upsert(vec![record(DocumentId::new(), 0, "a", vec![1.0, 0.0])])
                .await
                .unwrap();
            assert_eq!(store.len().await.unwrap(), 1);
        }
        // Reopen with a different width (embedder changed) — old data is dropped,
        // and the store accepts the new width instead of erroring.
        let store = LanceVectorStore::connect(uri, 3).await.unwrap();
        assert_eq!(store.dimensions(), 3);
        assert!(store.is_empty().await.unwrap());
        store
            .upsert(vec![record(DocumentId::new(), 0, "b", vec![1.0, 0.0, 0.0])])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_wrong_dimensionality() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 3)
            .await
            .unwrap();
        let err = store
            .upsert(vec![record(DocumentId::new(), 0, "x", vec![1.0, 0.0])])
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RetrievalError::DimensionMismatch {
                expected: 3,
                actual: 2
            }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_index_and_k_zero_return_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        assert!(store
            .query(&Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
            .await
            .unwrap()
            .is_empty());
        store
            .upsert(vec![record(DocumentId::new(), 0, "a", vec![1.0, 0.0])])
            .await
            .unwrap();
        assert!(store
            .query(&Embedding(vec![1.0, 0.0]), 0, SearchScope::Unscoped)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_replacement_is_one_commit_and_preserves_other_documents() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let removed = DocumentId::new();
        let kept = DocumentId::new();
        store
            .upsert(vec![
                record(removed, 0, "remove me", vec![1.0, 0.0]),
                record(kept, 0, "keep me", vec![0.0, 1.0]),
            ])
            .await
            .unwrap();

        let version = store.table.version().await.unwrap();
        store.replace_document(removed, Vec::new()).await.unwrap();

        assert_eq!(store.table.version().await.unwrap(), version + 1);
        assert_eq!(store.len().await.unwrap(), 1);
        let hits = store
            .query(&Embedding(vec![0.0, 1.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.document_id, kept);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_replacements_leave_one_complete_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let doc = DocumentId::new();
        store
            .upsert(vec![record(doc, 0, "old", vec![1.0, 0.0])])
            .await
            .unwrap();

        let version_a = vec![
            record(doc, 1, "a-one", vec![1.0, 0.0]),
            record(doc, 2, "a-two", vec![1.0, 0.0]),
        ];
        let version_b = vec![
            record(doc, 3, "b-one", vec![1.0, 0.0]),
            record(doc, 4, "b-two", vec![1.0, 0.0]),
        ];

        let (a, b) = tokio::join!(
            store.replace_document(doc, version_a),
            store.replace_document(doc, version_b)
        );
        a.unwrap();
        b.unwrap();

        let texts: std::collections::BTreeSet<_> = store
            .query(&Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap()
            .into_iter()
            .map(|hit| hit.chunk.text)
            .collect();
        let expected_a = ["a-one".to_string(), "a-two".to_string()].into();
        let expected_b = ["b-one".to_string(), "b-two".to_string()].into();
        assert!(texts == expected_a || texts == expected_b, "got {texts:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replacement_rejects_records_from_another_document_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let existing = DocumentId::new();
        let wrong = DocumentId::new();
        store
            .upsert(vec![record(existing, 0, "original", vec![1.0, 0.0])])
            .await
            .unwrap();
        let version = store.table.version().await.unwrap();

        let err = store
            .replace_document(existing, vec![record(wrong, 0, "wrong", vec![0.0, 1.0])])
            .await
            .unwrap_err();

        assert!(err.to_string().contains("belongs to document"));
        assert_eq!(store.table.version().await.unwrap(), version);
        let hits = store
            .query(&Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "original");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn document_writes_reject_mixed_project_metadata_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        let legacy_doc = DocumentId::new();
        let generation_doc = DocumentId::new();
        store
            .upsert(vec![record(legacy_doc, 0, "original", vec![1.0, 0.0])])
            .await
            .unwrap();

        let version = store.table.version().await.unwrap();
        let mixed_legacy = [
            vec![
                record(legacy_doc, 1, "unscoped", vec![1.0, 0.0]),
                project_record(project_a, legacy_doc, 2, "project-a", vec![1.0, 0.0]),
            ],
            vec![
                project_record(project_a, legacy_doc, 1, "project-a", vec![1.0, 0.0]),
                project_record(project_b, legacy_doc, 2, "project-b", vec![1.0, 0.0]),
            ],
        ];
        for records in mixed_legacy {
            let error = store
                .replace_document(legacy_doc, records)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("multiple project corpora"));
            assert_eq!(store.table.version().await.unwrap(), version);
        }
        let hits = store
            .query(&Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "original");

        let mixed_generation = [
            vec![
                record(generation_doc, 0, "unscoped", vec![1.0, 0.0]),
                project_record(project_a, generation_doc, 1, "project-a", vec![1.0, 0.0]),
            ],
            vec![
                project_record(project_a, generation_doc, 0, "project-a", vec![1.0, 0.0]),
                project_record(project_b, generation_doc, 1, "project-b", vec![1.0, 0.0]),
            ],
        ];
        for records in mixed_generation {
            let error = store
                .stage_document_generation(generation_doc, generation(1), records)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("multiple project corpora"));
            assert_eq!(store.table.version().await.unwrap(), version);
            assert_eq!(
                store
                    .newest_document_generation(generation_doc)
                    .await
                    .unwrap(),
                None
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn live_documents_cannot_move_between_project_corpora() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();

        let batch_doc = DocumentId::new();
        let empty_version = store.table.version().await.unwrap();
        let error = store
            .upsert(vec![
                project_record(project_a, batch_doc, 0, "batch-a", vec![1.0, 0.0]),
                project_record(project_b, batch_doc, 1, "batch-b", vec![1.0, 0.0]),
            ])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("multiple project corpora"));
        assert_eq!(store.table.version().await.unwrap(), empty_version);
        assert_eq!(store.document_len(batch_doc).await.unwrap(), Some(0));

        let legacy_doc = DocumentId::new();
        store
            .upsert(vec![project_record(
                project_a,
                legacy_doc,
                0,
                "legacy-a",
                vec![1.0, 0.0],
            )])
            .await
            .unwrap();
        let legacy_version = store.table.version().await.unwrap();
        let error = store
            .upsert(vec![project_record(
                project_b,
                legacy_doc,
                1,
                "sequential-b",
                vec![1.0, 0.0],
            )])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot move"));
        assert_eq!(store.table.version().await.unwrap(), legacy_version);
        let error = store
            .replace_document(
                legacy_doc,
                vec![project_record(
                    project_b,
                    legacy_doc,
                    0,
                    "replacement-b",
                    vec![1.0, 0.0],
                )],
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot move"));
        assert_eq!(store.table.version().await.unwrap(), legacy_version);
        assert_eq!(
            store
                .query(
                    &Embedding(vec![1.0, 0.0]),
                    10,
                    SearchScope::Project(project_a),
                )
                .await
                .unwrap()[0]
                .chunk
                .text,
            "legacy-a"
        );
        assert!(store
            .query(
                &Embedding(vec![1.0, 0.0]),
                10,
                SearchScope::Project(project_b),
            )
            .await
            .unwrap()
            .is_empty());

        let generation_doc = DocumentId::new();
        let first = generation(1);
        store
            .stage_document_generation(
                generation_doc,
                first,
                vec![project_record(
                    project_a,
                    generation_doc,
                    0,
                    "generation-a",
                    vec![1.0, 0.0],
                )],
            )
            .await
            .unwrap();
        assert!(store
            .activate_document_generation(generation_doc, first)
            .await
            .unwrap());
        let active_version = store.table.version().await.unwrap();
        let error = store
            .stage_document_generation(
                generation_doc,
                generation(2),
                vec![project_record(
                    project_b,
                    generation_doc,
                    0,
                    "generation-b",
                    vec![1.0, 0.0],
                )],
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot move"));
        assert_eq!(store.table.version().await.unwrap(), active_version);
        assert_eq!(
            store
                .newest_document_generation(generation_doc)
                .await
                .unwrap(),
            Some(DocumentGenerationState::Active(first))
        );
        let project_a_hits = store
            .query(
                &Embedding(vec![1.0, 0.0]),
                10,
                SearchScope::Project(project_a),
            )
            .await
            .unwrap();
        assert!(project_a_hits.iter().any(|hit| {
            hit.chunk.document_id == generation_doc && hit.chunk.text == "generation-a"
        }));
        assert!(store
            .query(
                &Embedding(vec![1.0, 0.0]),
                10,
                SearchScope::Project(project_b),
            )
            .await
            .unwrap()
            .is_empty());

        // Once an empty generation removes all live chunks, a recreated
        // document may choose a new corpus.
        let tombstone = generation(2);
        store
            .stage_document_generation(generation_doc, tombstone, Vec::new())
            .await
            .unwrap();
        assert!(store
            .activate_document_generation(generation_doc, tombstone)
            .await
            .unwrap());
        assert_eq!(
            store
                .stage_document_generation(
                    generation_doc,
                    generation(3),
                    vec![project_record(
                        project_b,
                        generation_doc,
                        0,
                        "recreated-b",
                        vec![1.0, 0.0],
                    )],
                )
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_resets_the_same_dimension_legacy_schema() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();
        let legacy_schema = Arc::new(Schema::new(vec![
            Field::new("chunk_id", DataType::Utf8, false),
            Field::new("document_id", DataType::Utf8, false),
            Field::new("ordinal", DataType::UInt64, false),
            Field::new("text", DataType::Utf8, false),
            Field::new("span_start", DataType::UInt64, false),
            Field::new("span_end", DataType::UInt64, false),
            Field::new(
                VECTOR_COL,
                DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), 2),
                false,
            ),
        ]));
        let db = lancedb::connect(uri).execute().await.unwrap();
        db.create_empty_table(TABLE, legacy_schema)
            .execute()
            .await
            .unwrap();
        drop(db);

        let store = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert_eq!(
            store.table.schema().await.unwrap().as_ref(),
            build_schema(2).as_ref()
        );
        assert!(store.is_empty().await.unwrap());
        let doc = DocumentId::new();
        let first = generation(1);
        assert_eq!(
            store
                .stage_document_generation(doc, first, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        assert!(store
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert_eq!(
            store.active_document_generation(doc).await.unwrap(),
            Some(first)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn staged_generation_is_durable_invisible_and_atomically_activated() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();
        let doc = DocumentId::new();
        let other = DocumentId::new();
        let first = generation(1);
        let store = LanceVectorStore::connect(uri, 2).await.unwrap();
        store
            .upsert(vec![record(other, 0, "other", vec![0.0, 1.0])])
            .await
            .unwrap();
        let before_stage = store.table.version().await.unwrap();
        assert_eq!(
            store
                .stage_document_generation(
                    doc,
                    first,
                    vec![record(doc, 0, "first", vec![1.0, 0.0])],
                )
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        assert_eq!(store.table.version().await.unwrap(), before_stage + 1);
        assert_eq!(store.len().await.unwrap(), 1);
        assert_eq!(store.document_len(doc).await.unwrap(), Some(0));
        assert_eq!(
            store.newest_document_generation(doc).await.unwrap(),
            Some(DocumentGenerationState::Staged(first))
        );
        let hits = store
            .query(&Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "other");
        drop(store);

        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert_eq!(
            reopened.active_document_generation(doc).await.unwrap(),
            None
        );
        let before_activation = reopened.table.version().await.unwrap();
        assert!(reopened
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert_eq!(
            reopened.table.version().await.unwrap(),
            before_activation + 1
        );
        assert_eq!(reopened.len().await.unwrap(), 2);
        let hits = reopened
            .query(&Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert!(hits.iter().any(|hit| hit.chunk.text == "first"));
        assert!(hits.iter().any(|hit| hit.chunk.text == "other"));
        assert_eq!(
            reopened.active_document_generation(doc).await.unwrap(),
            Some(first)
        );
        assert_eq!(
            reopened.newest_document_generation(doc).await.unwrap(),
            Some(DocumentGenerationState::Active(first))
        );
        let activated_version = reopened.table.version().await.unwrap();
        assert!(reopened
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert_eq!(reopened.table.version().await.unwrap(), activated_version);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn newer_stage_fences_stale_writers_and_empty_activation_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();
        let store = LanceVectorStore::connect(uri, 2).await.unwrap();
        let doc = DocumentId::new();
        let first = generation(1);
        let second = generation(2);
        let tombstone = generation(3);
        store
            .stage_document_generation(doc, first, vec![record(doc, 0, "live", vec![1.0, 0.0])])
            .await
            .unwrap();
        assert!(store
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert_eq!(
            store
                .stage_document_generation(doc, tombstone, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        let version = store.table.version().await.unwrap();
        assert_eq!(
            store
                .stage_document_generation(doc, second, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Rejected { current: tombstone }
        );
        assert_eq!(
            store
                .stage_document_generation(doc, tombstone, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::AlreadyPresent
        );
        let conflicting = DocumentGeneration {
            revision_token: uuid::Uuid::from_u128(300),
            ..tombstone
        };
        assert!(store
            .stage_document_generation(doc, conflicting, Vec::new())
            .await
            .is_err());
        assert_eq!(store.table.version().await.unwrap(), version);
        assert!(!store
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert!(store
            .upsert(vec![record(doc, 0, "legacy", vec![0.0, 1.0])])
            .await
            .is_err());
        assert!(store.replace_document(doc, Vec::new()).await.is_err());
        drop(store);

        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert!(reopened
            .activate_document_generation(doc, tombstone)
            .await
            .unwrap());
        assert!(reopened.is_empty().await.unwrap());
        assert_eq!(reopened.document_len(doc).await.unwrap(), Some(0));
        assert_eq!(
            reopened.active_document_generation(doc).await.unwrap(),
            Some(tombstone)
        );
        drop(reopened);
        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert_eq!(
            reopened.active_document_generation(doc).await.unwrap(),
            Some(tombstone)
        );
        assert_eq!(reopened.table.count_rows(None).await.unwrap(), 1);
        assert_eq!(
            reopened
                .stage_document_generation(doc, second, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Rejected { current: tombstone }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tombstone_allows_a_later_generation_to_change_corpus() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let doc = DocumentId::new();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        for (generation, records) in [
            (
                generation(1),
                vec![project_record(
                    project_a,
                    doc,
                    0,
                    "old corpus",
                    vec![1.0, 0.0],
                )],
            ),
            (generation(2), Vec::new()),
            (
                generation(3),
                vec![project_record(
                    project_b,
                    doc,
                    0,
                    "new corpus",
                    vec![0.0, 1.0],
                )],
            ),
        ] {
            assert_eq!(
                store
                    .stage_document_generation(doc, generation, records)
                    .await
                    .unwrap(),
                GenerationStageOutcome::Staged
            );
            assert!(store
                .activate_document_generation(doc, generation)
                .await
                .unwrap());
        }
        assert_eq!(
            store
                .query(
                    &Embedding(vec![0.0, 1.0]),
                    5,
                    SearchScope::Project(project_a)
                )
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store
                .query(
                    &Embedding(vec![0.0, 1.0]),
                    5,
                    SearchScope::Project(project_b)
                )
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_stages_leave_only_the_highest_generation_activatable() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let doc = DocumentId::new();
        let second = generation(2);
        let third = generation(3);
        let (lower, higher) = tokio::join!(
            store.stage_document_generation(
                doc,
                second,
                vec![record(doc, 0, "second", vec![1.0, 0.0])],
            ),
            store.stage_document_generation(
                doc,
                third,
                vec![record(doc, 0, "third", vec![0.0, 1.0])],
            )
        );
        let lower = lower.unwrap();
        assert!(
            lower == GenerationStageOutcome::Staged
                || lower == GenerationStageOutcome::Rejected { current: third }
        );
        assert_eq!(higher.unwrap(), GenerationStageOutcome::Staged);
        assert!(!store
            .activate_document_generation(doc, second)
            .await
            .unwrap());
        assert!(store
            .activate_document_generation(doc, third)
            .await
            .unwrap());
        let hits = store
            .query(&Embedding(vec![0.0, 1.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "third");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stage_validation_fails_before_any_dataset_commit() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let doc = DocumentId::new();
        let version = store.table.version().await.unwrap();
        assert!(store
            .stage_document_generation(
                doc,
                generation(1),
                vec![record(DocumentId::new(), 0, "wrong", vec![1.0, 0.0])],
            )
            .await
            .is_err());
        assert!(store
            .stage_document_generation(
                doc,
                generation(1),
                vec![record(doc, 0, "wrong dims", vec![1.0])],
            )
            .await
            .is_err());
        assert_eq!(store.table.version().await.unwrap(), version);
        assert_eq!(store.table.count_rows(None).await.unwrap(), 0);
    }
}
