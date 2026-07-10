//! A durable, embedded [`VectorStore`] backed by LanceDB.
//!
//! LanceDB is a pure-Rust, on-disk vector database (Arrow/Lance columnar format).
//! This is the persistent counterpart to [`crate::InMemoryVectorStore`]: same
//! seam, but the index survives a restart and can grow past what fits in memory,
//! with room for an ANN index and multimodal columns later.
//!
//! Chunks live in one table with a fixed-width vector column; scalar columns carry
//! the citation data (ids, ordinal, text, byte span). Cosine distance; LanceDB does
//! a flat (brute-force) scan until an index is built, which is fine for the corpus
//! sizes this targets today.
//!
//! **Build note:** enabled by the non-default `vec-lance` feature. LanceDB pulls a
//! large Arrow/DataFusion tree and needs `protoc` at build time — hence off by
//! default (the CI runner has no `protoc`). Build/test locally with `PROTOC` set.

use std::sync::Arc;

use arrow_array::types::Float32Type;
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
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
    /// dataset there is reopened (that's what makes the index durable). The `dims`
    /// must match the dataset's existing vector column when reopening.
    pub async fn connect(uri: &str, dims: usize) -> Result<Self> {
        let schema = build_schema(dims);
        let db = lancedb::connect(uri).execute().await.map_err(lance_err)?;
        let names = db.table_names().execute().await.map_err(lance_err)?;
        let table = if names.iter().any(|n| n == TABLE) {
            db.open_table(TABLE).execute().await.map_err(lance_err)?
        } else {
            db.create_empty_table(TABLE, schema.clone())
                .execute()
                .await
                .map_err(lance_err)?
        };
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

    /// Append records to the table (no dedupe; callers delete first as needed).
    async fn append(&self, records: &[VectorRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let batch = self.to_batch(records)?;
        self.table.add(batch).execute().await.map_err(lance_err)?;
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

#[async_trait]
impl VectorStore for LanceVectorStore {
    async fn upsert(&self, records: Vec<VectorRecord>) -> Result<()> {
        for record in &records {
            self.check_dims(&record.embedding)?;
        }
        // Delete any existing rows with these chunk ids, then append — the
        // delete-then-insert upsert. Chunk ids are UUIDs (hex + hyphens only), so
        // interpolating them into the SQL predicate is injection-safe.
        let ids: Vec<String> = records
            .iter()
            .map(|r| format!("'{}'", r.chunk.id))
            .collect();
        if !ids.is_empty() {
            self.table
                .delete(&format!("chunk_id IN ({})", ids.join(", ")))
                .await
                .map_err(lance_err)?;
        }
        self.append(&records).await
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
        }
        // Delete the document's rows, then append the new ones. LanceDB commits
        // each op as a dataset version; unlike the in-memory store this isn't a
        // single atomic write, so concurrent replaces of the *same* document
        // should be serialized by the caller (the server ingests per request).
        self.table
            .delete(&format!("document_id = '{document_id}'"))
            .await
            .map_err(lance_err)?;
        self.append(&records).await
    }

    async fn len(&self) -> Result<usize> {
        self.table.count_rows(None).await.map_err(lance_err)
    }
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

            // replace_document swaps the document's rows atomically-enough.
            store
                .replace_document(doc, vec![record(doc, 0, "south", vec![1.0, 0.0])])
                .await
                .unwrap();
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
}
