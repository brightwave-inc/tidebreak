//! A durable, embedded [`VectorStore`] backed by LanceDB.
//!
//! LanceDB is a pure-Rust, on-disk vector database (Arrow/Lance columnar format).
//! This is the persistent counterpart to [`crate::InMemoryVectorStore`]: same
//! seam, but the index survives a restart and can grow past what fits in memory,
//! with room for an ANN index and multimodal columns later.
//!
//! Chunks live in one table with a fixed-width vector column; scalar columns carry
//! the citation data (ids, ordinal, text, byte span). Writes use Lance's
//! transactional merge-insert operation, so replacing a document is published as
//! one dataset version. Cosine distance; LanceDB does a flat (brute-force) scan
//! until an index is built, which is fine for the corpus sizes this targets today.
//!
//! **Build note:** enabled by the non-default `vec-lance` feature. LanceDB pulls a
//! large Arrow/DataFusion tree and needs `protoc` at build time — hence off by
//! default for library consumers. OpenWave's workspace CI installs `protoc`.

use std::sync::Arc;

use arrow_array::types::Float32Type;
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator,
    StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{DistanceType, Table};

use crate::document::{ByteSpan, Chunk, ScoredChunk};
use crate::embed::Embedding;
use crate::error::{Result, RetrievalError};
use crate::id::{ChunkId, DocumentId};
use crate::vector::{VectorRecord, VectorStore};

/// The single table all chunks are stored in.
const TABLE: &str = "chunks";
/// The vector column name.
const VECTOR_COL: &str = "vector";
/// LanceDB's distance column on query results.
const DISTANCE_COL: &str = "_distance";
/// Lance dataset configuration marker for the one-time legacy duplicate repair.
const UNIQUE_CHUNK_IDS_V1: &str = "openwave.unique_chunk_ids_v1";
/// Lance's version-relative row-id meta column, used only within one repair scan.
const ROW_ID_COL: &str = "_rowid";
/// Bound repair predicates so a badly duplicated legacy dataset does not produce
/// one pathological SQL expression.
const REPAIR_DELETE_BATCH: usize = 500;

/// A persistent [`VectorStore`] backed by a local LanceDB dataset.
pub struct LanceVectorStore {
    table: Table,
    schema: SchemaRef,
    dims: usize,
}

impl LanceVectorStore {
    /// Open (or create) a LanceDB-backed store at `uri` for vectors of `dims`.
    ///
    /// `uri` is a local directory path; it's created if missing, and an existing
    /// dataset there is reopened (that's what makes the index durable).
    ///
    /// **Embedder-change guardrail:** if an existing table's vector width doesn't
    /// match `dims` (e.g. the configured embedder changed — offline 256-dim vs
    /// OpenAI 1536-dim), the old table is dropped and recreated empty. The index is
    /// derived data (re-embeddable from source documents), so rebuilding is the
    /// safe response to a dimensionality change rather than erroring every write.
    pub async fn connect(uri: &str, dims: usize) -> Result<Self> {
        let schema = build_schema(dims);
        let db = lancedb::connect(uri).execute().await.map_err(lance_err)?;
        let names = db.table_names().execute().await.map_err(lance_err)?;

        let existing_dims = if names.iter().any(|n| n == TABLE) {
            let table = db.open_table(TABLE).execute().await.map_err(lance_err)?;
            vector_dim(&table.schema().await.map_err(lance_err)?)
        } else {
            None
        };

        let table = match existing_dims {
            Some(existing) if existing == dims => {
                db.open_table(TABLE).execute().await.map_err(lance_err)?
            }
            Some(_) => {
                // Incompatible width: drop and start fresh.
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
        ensure_unique_chunk_ids(&table).await?;
        Ok(Self {
            table,
            schema,
            dims,
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

    /// Build one Arrow `RecordBatch` from the records, matching the table schema.
    fn to_batch(&self, records: &[VectorRecord]) -> Result<RecordBatch> {
        let chunk_ids =
            StringArray::from_iter_values(records.iter().map(|r| r.chunk.id.to_string()));
        let doc_ids =
            StringArray::from_iter_values(records.iter().map(|r| r.chunk.document_id.to_string()));
        let ordinals =
            UInt64Array::from_iter_values(records.iter().map(|r| r.chunk.ordinal as u64));
        let texts = StringArray::from_iter_values(records.iter().map(|r| r.chunk.text.as_str()));
        let starts =
            UInt64Array::from_iter_values(records.iter().map(|r| r.chunk.span.start as u64));
        let ends = UInt64Array::from_iter_values(records.iter().map(|r| r.chunk.span.end as u64));
        let vectors = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            records
                .iter()
                .map(|r| Some(r.embedding.0.iter().map(|&f| Some(f)).collect::<Vec<_>>())),
            self.dims as i32,
        );
        let columns: Vec<ArrayRef> = vec![
            Arc::new(chunk_ids),
            Arc::new(doc_ids),
            Arc::new(ordinals),
            Arc::new(texts),
            Arc::new(starts),
            Arc::new(ends),
            Arc::new(vectors),
        ];
        RecordBatch::try_new(self.schema.clone(), columns).map_err(lance_err)
    }
}

/// Repair duplicate chunk ids that an older delete-then-append writer could leave
/// behind after concurrent writes.
///
/// Lance merge-insert treats multiple target matches as undefined, so the new
/// transactional writer must establish uniqueness before using `chunk_id` as its
/// merge key. This scans only ids and row ids once per existing dataset, keeps the
/// newest physical row for each id, deletes older duplicates in bounded commits,
/// and records a dataset marker so normal startups do not rescan the corpus.
async fn ensure_unique_chunk_ids(table: &Table) -> Result<()> {
    let native = table
        .as_native()
        .ok_or_else(|| RetrievalError::vector_store("Lance table is not a local dataset"))?;
    let manifest = native.manifest().await.map_err(lance_err)?;
    if manifest.config.get(UNIQUE_CHUNK_IDS_V1).map(String::as_str) == Some("1") {
        return Ok(());
    }

    let mut stream = table
        .query()
        .select(Select::columns(&["chunk_id"]))
        .with_row_id()
        .execute()
        .await
        .map_err(lance_err)?;
    let mut newest_by_id = std::collections::HashMap::<String, u64>::new();
    let mut stale_row_ids = Vec::new();
    while let Some(batch) = stream.try_next().await.map_err(lance_err)? {
        let chunk_ids = str_col(&batch, "chunk_id")?;
        let row_ids = u64_col(&batch, ROW_ID_COL)?;
        for i in 0..batch.num_rows() {
            let id = chunk_ids.value(i).to_string();
            let row_id = row_ids.value(i);
            if let Some(kept) = newest_by_id.get_mut(&id) {
                // Query order is deliberately irrelevant: the highest physical
                // row id is the last append from the legacy writer. Concurrent
                // repair scans therefore choose the same survivor.
                if row_id > *kept {
                    stale_row_ids.push(*kept);
                    *kept = row_id;
                } else {
                    stale_row_ids.push(row_id);
                }
            } else {
                newest_by_id.insert(id, row_id);
            }
        }
    }

    for batch in stale_row_ids.chunks(REPAIR_DELETE_BATCH) {
        let ids = batch
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        table
            .delete(&format!("{ROW_ID_COL} IN ({ids})"))
            .await
            .map_err(lance_err)?;
    }
    native
        .update_config([(UNIQUE_CHUNK_IDS_V1.to_string(), "1".to_string())])
        .await
        .map_err(lance_err)
}

#[async_trait]
impl VectorStore for LanceVectorStore {
    async fn upsert(&self, records: Vec<VectorRecord>) -> Result<()> {
        for record in &records {
            self.check_dims(&record.embedding)?;
        }
        // Collapse duplicate chunk ids within the batch (last wins) so this backend
        // matches InMemoryVectorStore's by-id dedupe. Merge-insert updates existing
        // rows and inserts new rows in one Lance dataset version.
        let records = dedupe_by_chunk_id(records);
        if records.is_empty() {
            return Ok(());
        }
        let batch = self.to_batch(&records)?;
        let reader = RecordBatchIterator::new(vec![Ok(batch)], self.schema.clone());
        let mut merge = self.table.merge_insert(&["chunk_id"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        merge.execute(Box::new(reader)).await.map_err(lance_err)?;
        Ok(())
    }

    async fn query(&self, query: &Embedding, k: usize) -> Result<Vec<ScoredChunk>> {
        self.check_dims(query)?;
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut stream = self
            .table
            .query()
            .nearest_to(query.0.as_slice())
            .map_err(lance_err)?
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
        for record in &records {
            self.check_dims(&record.embedding)?;
            if record.chunk.document_id != document_id {
                return Err(RetrievalError::vector_store(format!(
                    "replacement record {} belongs to document {}, expected {document_id}",
                    record.chunk.id, record.chunk.document_id
                )));
            }
        }
        let records = dedupe_by_chunk_id(records);
        let batch = self.to_batch(&records)?;
        let reader = RecordBatchIterator::new(vec![Ok(batch)], self.schema.clone());

        // One merge transaction replaces matching chunk ids, inserts new chunks,
        // and deletes stale chunks belonging to this document. Readers see either
        // the previous dataset version or the committed replacement, never the
        // delete half of a two-step write. The target filter prevents rows from
        // other documents being deleted merely because they are absent from this
        // replacement's source batch.
        let mut merge = self.table.merge_insert(&["chunk_id"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all()
            .when_not_matched_by_source_delete(Some(format!("document_id = '{document_id}'")));
        merge.execute(Box::new(reader)).await.map_err(lance_err)?;
        Ok(())
    }

    async fn len(&self) -> Result<usize> {
        self.table.count_rows(None).await.map_err(lance_err)
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

/// The vector column's fixed width in a table schema, if present.
fn vector_dim(schema: &SchemaRef) -> Option<usize> {
    schema
        .field_with_name(VECTOR_COL)
        .ok()
        .and_then(|field| match field.data_type() {
            DataType::FixedSizeList(_, size) => Some(*size as usize),
            _ => None,
        })
}

/// The table schema: scalar citation columns plus the fixed-width vector column.
fn build_schema(dims: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("document_id", DataType::Utf8, false),
        Field::new("ordinal", DataType::UInt64, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("span_start", DataType::UInt64, false),
        Field::new("span_end", DataType::UInt64, false),
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

fn lance_err(err: impl std::fmt::Display) -> RetrievalError {
    RetrievalError::vector_store(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(doc: DocumentId, ordinal: usize, text: &str, vector: Vec<f32>) -> VectorRecord {
        let span = ByteSpan::new(ordinal * 100, ordinal * 100 + text.len());
        VectorRecord {
            chunk: Chunk::new(doc, ordinal, span, text),
            embedding: Embedding(vector),
        }
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

            // Nearest to "east" is the east vector, with its span/text intact.
            let hits = store.query(&Embedding(vec![1.0, 0.0]), 1).await.unwrap();
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
            let hits = store.query(&Embedding(vec![1.0, 0.0]), 5).await.unwrap();
            assert_eq!(hits[0].chunk.text, "south");
        }

        // Reopen the same directory: the data is still there — that's durability.
        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert_eq!(reopened.len().await.unwrap(), 1);
        let hits = reopened.query(&Embedding(vec![1.0, 0.0]), 5).await.unwrap();
        assert_eq!(hits[0].chunk.text, "south");
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
        let hits = store.query(&Embedding(vec![0.0, 1.0]), 5).await.unwrap();
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
            .query(&Embedding(vec![1.0, 0.0]), 5)
            .await
            .unwrap()
            .is_empty());
        store
            .upsert(vec![record(DocumentId::new(), 0, "a", vec![1.0, 0.0])])
            .await
            .unwrap();
        assert!(store
            .query(&Embedding(vec![1.0, 0.0]), 0)
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
        let hits = store.query(&Embedding(vec![0.0, 1.0]), 10).await.unwrap();
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
            .query(&Embedding(vec![1.0, 0.0]), 10)
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
        let hits = store.query(&Embedding(vec![1.0, 0.0]), 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "original");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_repairs_duplicate_ids_from_the_legacy_writer_once() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();
        let schema = build_schema(2);
        let db = lancedb::connect(uri).execute().await.unwrap();
        let table = db
            .create_empty_table(TABLE, schema.clone())
            .execute()
            .await
            .unwrap();
        let legacy = LanceVectorStore {
            table: table.clone(),
            schema,
            dims: 2,
        };
        let doc = DocumentId::new();
        let old = record(doc, 0, "old", vec![1.0, 0.0]);
        let new = record(doc, 0, "new", vec![0.0, 1.0]);
        assert_eq!(old.chunk.id, new.chunk.id);
        table
            .add(legacy.to_batch(&[old]).unwrap())
            .execute()
            .await
            .unwrap();
        table
            .add(legacy.to_batch(&[new]).unwrap())
            .execute()
            .await
            .unwrap();
        assert_eq!(table.count_rows(None).await.unwrap(), 2);
        drop(legacy);
        drop(table);
        drop(db);

        let repaired = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert_eq!(repaired.len().await.unwrap(), 1);
        let hits = repaired
            .query(&Embedding(vec![0.0, 1.0]), 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "new");
        let version = repaired.table.version().await.unwrap();

        // The migration marker prevents a second scan/write on reopen.
        drop(repaired);
        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert_eq!(reopened.table.version().await.unwrap(), version);

        // With target uniqueness restored, merge-insert keeps it unique.
        reopened
            .replace_document(doc, vec![record(doc, 0, "final", vec![1.0, 0.0])])
            .await
            .unwrap();
        assert_eq!(reopened.len().await.unwrap(), 1);
        let hits = reopened
            .query(&Embedding(vec![1.0, 0.0]), 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "final");
    }
}
