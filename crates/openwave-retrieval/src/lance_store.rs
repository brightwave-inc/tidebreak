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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::types::Float32Type;
use arrow_array::{
    Array, ArrayRef, BooleanArray, FixedSizeListArray, Float32Array, Int64Array, ListArray,
    RecordBatch, RecordBatchIterator, StringArray, UInt32Array, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use arrow_select::filter::filter_record_batch;
use arrow_select::take::take;
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::index::{Index, IndexType};
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::rerankers::Reranker;
use lancedb::table::{OptimizeAction, OptimizeOptions};
use lancedb::{DistanceType, Table};
use tokio::sync::RwLock;

use crate::document::{ByteSpan, Chunk, ScoredChunk};
#[cfg(test)]
use crate::document::{SourceLocation, SourceRegion};
use crate::embed::Embedding;
use crate::error::{Result, RetrievalError};
use crate::id::{ChunkId, DocumentId};
use crate::vector::{
    DocumentGenerationState, GenerationStageOutcome, SearchOptions, SearchScope, VectorRecord,
    VectorStore,
};
use openwave_core::{DocumentGeneration, ProjectId};

/// The single table all chunks and publication markers are stored in.
const TABLE: &str = "chunks";
/// The vector column name.
const VECTOR_COL: &str = "vector";
/// LanceDB's distance column on query results.
const DISTANCE_COL: &str = "_distance";
const RELEVANCE_COL: &str = "_relevance_score";
const INDEX_MUTATION_THRESHOLD: usize = 20;
const UNVERSIONED_CHUNK: &str = "unversioned_chunk";
const STAGED_CHUNK: &str = "staged_chunk";
const ACTIVE_CHUNK: &str = "active_chunk";
const STAGED_MARKER: &str = "staged_marker";
const ACTIVE_MARKER: &str = "active_marker";
const VISIBLE_CHUNKS: &str = "row_kind IN ('unversioned_chunk', 'active_chunk')";

/// A persistent [`VectorStore`] backed by a local LanceDB dataset.
///
/// One instance serializes every mutation so marker reads and their following
/// merge commit form one coordinator operation. Queries share a read guard so
/// Lance's lexical and dense branches observe one publication snapshot while
/// still running concurrently with other queries. An exclusive lock file
/// enforces one writer instance per dataset across handles and processes.
pub struct LanceVectorStore {
    table: Table,
    schema: SchemaRef,
    dims: usize,
    publication_lock: RwLock<()>,
    text_index_name: String,
    index_mutations: AtomicUsize,
    _writer_lock: File,
}

/// Conventional RRF(k=60) with a stable chunk-id secondary order.
#[derive(Debug)]
struct DeterministicRrf {
    query: Embedding,
    min_dense_similarity: f32,
}

fn hybrid_row_ids(batch: &RecordBatch) -> lancedb::Result<&UInt64Array> {
    batch
        .column_by_name("_rowid")
        .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| lancedb::Error::InvalidInput {
            message: "hybrid candidate is missing _rowid".to_string(),
        })
}

fn hybrid_branch_order(
    batch: &RecordBatch,
    score_column: &str,
    descending: bool,
) -> lancedb::Result<Vec<usize>> {
    if batch.num_rows() == 0 {
        return Ok(Vec::new());
    }
    let scores = batch
        .column_by_name(score_column)
        .and_then(|column| column.as_any().downcast_ref::<Float32Array>())
        .ok_or_else(|| lancedb::Error::InvalidInput {
            message: format!("hybrid candidate is missing {score_column}"),
        })?;
    let chunk_ids = batch
        .column_by_name("chunk_id")
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| lancedb::Error::InvalidInput {
            message: "hybrid candidate is missing chunk_id".to_string(),
        })?;
    let parsed_chunk_ids = (0..batch.num_rows())
        .map(|index| {
            chunk_ids.value(index).parse::<ChunkId>().map_err(|error| {
                lancedb::Error::InvalidInput {
                    message: format!("hybrid candidate has invalid chunk_id: {error}"),
                }
            })
        })
        .collect::<lancedb::Result<Vec<_>>>()?;
    let mut order = (0..batch.num_rows()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        let score_order = if descending {
            scores.value(*right).total_cmp(&scores.value(*left))
        } else {
            scores.value(*left).total_cmp(&scores.value(*right))
        };
        score_order.then_with(|| parsed_chunk_ids[*left].0.cmp(&parsed_chunk_ids[*right].0))
    });
    Ok(order)
}

fn dense_similarity(batch: &RecordBatch, index: usize, query: &Embedding) -> lancedb::Result<f32> {
    let vectors = batch
        .column_by_name(VECTOR_COL)
        .and_then(|column| column.as_any().downcast_ref::<FixedSizeListArray>())
        .ok_or_else(|| lancedb::Error::InvalidInput {
            message: "hybrid dense candidate is missing vector".to_string(),
        })?;
    let vector = vectors.value(index);
    let vector = vector
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or_else(|| lancedb::Error::InvalidInput {
            message: "hybrid dense candidate has an invalid vector".to_string(),
        })?;
    Ok(query.cosine_similarity(&Embedding(vector.values().to_vec())))
}

#[async_trait]
impl Reranker for DeterministicRrf {
    async fn rerank_hybrid(
        &self,
        _query: &str,
        vector_results: RecordBatch,
        fts_results: RecordBatch,
    ) -> lancedb::Result<RecordBatch> {
        let mut scores = std::collections::BTreeMap::new();
        let vector_order = hybrid_branch_order(&vector_results, DISTANCE_COL, false)?
            .into_iter()
            .filter_map(
                |index| match dense_similarity(&vector_results, index, &self.query) {
                    Ok(similarity) if similarity >= self.min_dense_similarity => Some(Ok(index)),
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<lancedb::Result<Vec<_>>>()?;
        if !vector_order.is_empty() {
            let vector_row_ids = hybrid_row_ids(&vector_results)?;
            for (position, index) in vector_order.into_iter().enumerate() {
                let row_id = vector_row_ids.value(index);
                *scores.entry(row_id).or_insert(0.0) += 1.0 / (61.0 + position as f32);
            }
        }
        let fts_order = hybrid_branch_order(&fts_results, "_score", true)?;
        if !fts_order.is_empty() {
            let fts_row_ids = hybrid_row_ids(&fts_results)?;
            for (position, index) in fts_order.into_iter().enumerate() {
                let row_id = fts_row_ids.value(index);
                *scores.entry(row_id).or_insert(0.0) += 1.0 / (61.0 + position as f32);
            }
        }

        let combined = match (vector_results.num_rows(), fts_results.num_rows()) {
            (0, 0) | (_, 0) => vector_results,
            (0, _) => fts_results,
            _ => self.merge_results(vector_results, fts_results)?,
        };
        let keep = BooleanArray::from_iter(
            hybrid_row_ids(&combined)?
                .values()
                .iter()
                .map(|row_id| Some(scores.contains_key(row_id))),
        );
        let combined = filter_record_batch(&combined, &keep)?;
        if combined.num_rows() == 0 {
            let mut fields = combined.schema().fields().to_vec();
            fields.push(Arc::new(Field::new(
                RELEVANCE_COL,
                DataType::Float32,
                false,
            )));
            let mut columns = combined.columns().to_vec();
            columns.push(Arc::new(Float32Array::from(Vec::<f32>::new())));
            return RecordBatch::try_new(Arc::new(Schema::new(fields)), columns)
                .map_err(Into::into);
        }
        let combined_row_ids = hybrid_row_ids(&combined)?;
        let chunk_ids = combined
            .column_by_name("chunk_id")
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| lancedb::Error::InvalidInput {
                message: "hybrid candidate is missing chunk_id".to_string(),
            })?;
        let parsed_chunk_ids = (0..combined.num_rows())
            .map(|index| {
                chunk_ids.value(index).parse::<ChunkId>().map_err(|error| {
                    lancedb::Error::InvalidInput {
                        message: format!("hybrid candidate has invalid chunk_id: {error}"),
                    }
                })
            })
            .collect::<lancedb::Result<Vec<_>>>()?;
        let relevance = Float32Array::from_iter_values(
            combined_row_ids
                .values()
                .iter()
                .map(|row_id| scores[row_id]),
        );
        let mut order = (0..combined.num_rows()).collect::<Vec<_>>();
        order.sort_by(|left, right| {
            relevance
                .value(*right)
                .total_cmp(&relevance.value(*left))
                .then_with(|| parsed_chunk_ids[*left].0.cmp(&parsed_chunk_ids[*right].0))
        });
        let order = UInt32Array::from_iter_values(order.into_iter().map(|index| index as u32));

        let mut fields = combined.schema().fields().to_vec();
        fields.push(Arc::new(Field::new(
            RELEVANCE_COL,
            DataType::Float32,
            false,
        )));
        let mut columns = combined.columns().to_vec();
        columns.push(Arc::new(relevance));
        let columns = columns
            .iter()
            .map(|column| take(column.as_ref(), &order, None))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).map_err(Into::into)
    }
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
        let text_index = table
            .list_indices()
            .await
            .map_err(lance_err)?
            .into_iter()
            .find(|index| {
                index.index_type == IndexType::FTS && index.columns == ["retrieval_text"]
            });
        let text_index_name = if let Some(index) = text_index {
            index.name
        } else {
            table
                .create_index(&["retrieval_text"], Index::FTS(Default::default()))
                .execute()
                .await
                .map_err(lance_err)?;
            "retrieval_text_idx".to_string()
        };
        if table
            .index_stats(&text_index_name)
            .await
            .map_err(lance_err)?
            .is_some_and(|stats| stats.num_unindexed_rows > 0)
        {
            let mut options = OptimizeOptions::default();
            options.index_names = Some(vec![text_index_name.clone()]);
            table
                .optimize(OptimizeAction::Index(options))
                .await
                .map_err(lance_err)?;
        }
        Ok(Self {
            table,
            schema,
            dims,
            publication_lock: RwLock::new(()),
            text_index_name,
            index_mutations: AtomicUsize::new(0),
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

    fn validate_record(&self, record: &VectorRecord) -> Result<()> {
        self.check_dims(&record.embedding)?;
        record.chunk.validate_source_regions()
    }

    fn validate_document_records(
        &self,
        document_id: DocumentId,
        records: &[VectorRecord],
    ) -> Result<()> {
        let project_id = records.first().map(|record| record.project_id);
        for record in records {
            self.validate_record(record)?;
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
            self.validate_record(record)?;
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
        let retrieval_texts = rows
            .iter()
            .map(|row| {
                row.record.as_ref().map_or_else(String::new, |record| {
                    record.chunk.retrieval_text().into_owned()
                })
            })
            .collect::<Vec<_>>();
        let retrieval_texts =
            StringArray::from_iter_values(retrieval_texts.iter().map(String::as_str));
        let mut heading_paths = ListBuilder::new(StringBuilder::new());
        for row in rows {
            if let Some(record) = &row.record {
                for heading in &record.chunk.heading_path {
                    heading_paths.values().append_value(heading);
                }
            }
            heading_paths.append(true);
        }
        let heading_paths = heading_paths.finish();
        let source_region_values = rows
            .iter()
            .map(|row| {
                serde_json::to_string(
                    &row.record
                        .as_ref()
                        .map_or(&[][..], |record| record.chunk.source_regions.as_slice()),
                )
                .map_err(|error| {
                    RetrievalError::vector_store(format!(
                        "failed to encode source regions: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let source_regions =
            StringArray::from_iter_values(source_region_values.iter().map(String::as_str));
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
            Arc::new(heading_paths),
            Arc::new(source_regions),
            Arc::new(retrieval_texts),
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
        let mutations = self.index_mutations.fetch_add(1, Ordering::Relaxed) + 1;
        if mutations >= INDEX_MUTATION_THRESHOLD {
            let mut options = OptimizeOptions::default();
            options.index_names = Some(vec![self.text_index_name.clone()]);
            if self
                .table
                .optimize(OptimizeAction::Index(options))
                .await
                .is_ok()
            {
                self.index_mutations.store(0, Ordering::Relaxed);
            }
            // Index maintenance is an optimization, not part of publication.
            // On failure the committed mutation remains successful and the
            // counter stays above threshold so the next mutation retries.
        }
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
                "heading_path",
                "source_regions",
                "retrieval_text",
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
        let _write = self.publication_lock.write().await;
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

    async fn query_with_options(
        &self,
        query_text: &str,
        query: &Embedding,
        k: usize,
        options: SearchOptions,
    ) -> Result<Vec<ScoredChunk>> {
        options.validate()?;
        self.check_dims(query)?;
        if k == 0 {
            return Ok(Vec::new());
        }
        let _snapshot = self.publication_lock.read().await;
        // Lance applies this predicate before the vector limit, so a closer row
        // in another corpus cannot consume one of this scope's top-k slots.
        // ProjectId is a parsed UUID newtype, not caller-provided query text.
        let scope_filter = match options.scope {
            SearchScope::Unscoped => "project_id IS NULL".to_string(),
            SearchScope::Project(project_id) => format!("project_id = '{project_id}'"),
        };
        let visibility_filter = format!("({VISIBLE_CHUNKS}) AND ({scope_filter})");
        let hybrid = !query_text.trim().is_empty();
        let mut vector_query = self
            .table
            .query()
            .nearest_to(query.0.as_slice())
            .map_err(lance_err)?
            .only_if(visibility_filter)
            .column(VECTOR_COL)
            .distance_type(DistanceType::Cosine)
            // Branch selection stays bounded at k. A later retrieval-policy
            // layer can add overfetch; fusion orders the candidates delivered here.
            .limit(k);
        if !hybrid {
            // Lance's upper distance bound is exclusive. Advancing by one ULP
            // preserves our inclusive similarity-floor contract at the boundary.
            vector_query = vector_query
                .distance_range(None, Some((1.0 - options.min_dense_similarity).next_up()));
        }
        let mut stream = if hybrid {
            vector_query
                .full_text_search(FullTextSearchQuery::new(query_text.to_string()))
                .rerank(Arc::new(DeterministicRrf {
                    query: query.clone(),
                    min_dense_similarity: options.min_dense_similarity,
                }))
                .execute()
                .await
                .map_err(lance_err)?
        } else {
            vector_query.execute().await.map_err(lance_err)?
        };

        let mut out = Vec::new();
        while let Some(batch) = stream.try_next().await.map_err(lance_err)? {
            if batch.num_rows() == 0 {
                continue;
            }
            if hybrid {
                read_hybrid_batch(&batch, &mut out)?;
            } else {
                read_batch(&batch, &mut out)?;
            }
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
        let _write = self.publication_lock.write().await;
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
        let _write = self.publication_lock.write().await;
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
        let _write = self.publication_lock.write().await;
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
        let _read = self.publication_lock.read().await;
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
        Field::new(
            "heading_path",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            false,
        ),
        Field::new("source_regions", DataType::Utf8, false),
        Field::new("retrieval_text", DataType::Utf8, false),
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
    let heading_paths = list_str_col(batch, "heading_path")?;
    let source_regions = str_col(batch, "source_regions")?;
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
        let text = texts.value(index).to_string();
        let span = ByteSpan::new(starts.value(index) as usize, ends.value(index) as usize);
        out.push(VectorRecord {
            project_id,
            chunk: Chunk {
                id: chunk_id,
                document_id,
                ordinal: ordinals.value(index) as usize,
                text: text.clone(),
                heading_path: list_str_value(heading_paths, index)?,
                source_regions: decode_source_regions(source_regions.value(index), &text, span)?,
                span,
            },
            embedding: Embedding(values.values().to_vec()),
        });
    }
    Ok(())
}

/// Decode one result batch into [`ScoredChunk`]s, converting cosine distance to
/// similarity (`1 - distance`) so scores match the crate's `[-1, 1]` convention.
fn read_batch(batch: &RecordBatch, out: &mut Vec<ScoredChunk>) -> Result<()> {
    read_scored_batch(batch, DISTANCE_COL, |distance| 1.0 - distance, out)
}

fn read_hybrid_batch(batch: &RecordBatch, out: &mut Vec<ScoredChunk>) -> Result<()> {
    read_scored_batch(batch, RELEVANCE_COL, |score| score, out)
}

fn read_scored_batch(
    batch: &RecordBatch,
    score_column: &str,
    convert_score: impl Fn(f32) -> f32,
    out: &mut Vec<ScoredChunk>,
) -> Result<()> {
    let chunk_ids = str_col(batch, "chunk_id")?;
    let doc_ids = str_col(batch, "document_id")?;
    let ordinals = u64_col(batch, "ordinal")?;
    let texts = str_col(batch, "text")?;
    let heading_paths = list_str_col(batch, "heading_path")?;
    let source_regions = str_col(batch, "source_regions")?;
    let starts = u64_col(batch, "span_start")?;
    let ends = u64_col(batch, "span_end")?;
    let scores = batch
        .column_by_name(score_column)
        .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
        .ok_or_else(|| {
            RetrievalError::vector_store(format!(
                "query result missing or mistyped {score_column} column"
            ))
        })?;

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
        let text = texts.value(i).to_string();
        out.push(ScoredChunk {
            chunk: Chunk {
                id: chunk_id,
                document_id,
                ordinal: ordinals.value(i) as usize,
                text: text.clone(),
                heading_path: list_str_value(heading_paths, i)?,
                source_regions: decode_source_regions(source_regions.value(i), &text, span)?,
                span,
            },
            score: convert_score(scores.value(i)),
        });
    }
    Ok(())
}

fn decode_source_regions(
    encoded: &str,
    chunk_text: &str,
    chunk_span: ByteSpan,
) -> Result<Vec<crate::document::SourceRegion>> {
    let regions: Vec<crate::document::SourceRegion> =
        serde_json::from_str(encoded).map_err(|error| {
            RetrievalError::vector_store(format!("invalid stored source regions: {error}"))
        })?;
    let mut previous_end = chunk_span.start;
    for region in &regions {
        if region.span.is_empty()
            || region.span.start < chunk_span.start
            || region.span.end > chunk_span.end
            || region.span.start < previous_end
        {
            return Err(RetrievalError::vector_store(
                "stored source regions are not ordered within the chunk span",
            ));
        }
        let local_start = region.span.start - chunk_span.start;
        let local_end = region.span.end - chunk_span.start;
        if !chunk_text.is_char_boundary(local_start) || !chunk_text.is_char_boundary(local_end) {
            return Err(RetrievalError::vector_store(
                "stored source region offsets are not UTF-8 character boundaries",
            ));
        }
        previous_end = region.span.end;
    }
    Ok(regions)
}

fn str_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| RetrievalError::vector_store(format!("missing/mistyped column `{name}`")))
}

fn list_str_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a ListArray> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<ListArray>())
        .ok_or_else(|| RetrievalError::vector_store(format!("missing/mistyped column `{name}`")))
}

fn list_str_value(column: &ListArray, index: usize) -> Result<Vec<String>> {
    let values = column.value(index);
    let values = values
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| RetrievalError::vector_store("heading_path values are not utf8"))?;
    Ok(values.iter().flatten().map(ToOwned::to_owned).collect())
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
                .query("", &Embedding(vec![1.0, 0.0]), 1, SearchScope::Unscoped)
                .await
                .unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].chunk.text, "east");
            assert_eq!(hits[0].chunk.document_id, doc);
            assert!((hits[0].score - 1.0).abs() < 1e-6);

            // A replacement is published as exactly one Lance dataset version.
            let version_before_replace = store.table.version().await.unwrap();
            let mut south = record(doc, 0, "south", vec![1.0, 0.0]);
            south.chunk.heading_path = vec!["Compass".into(), "Needleshard".into()];
            south.chunk.source_regions = vec![SourceRegion {
                span: south.chunk.span,
                location: SourceLocation::Page {
                    number: std::num::NonZeroU32::new(7).unwrap(),
                },
            }];
            let expected_source_regions = south.chunk.source_regions.clone();
            store.replace_document(doc, vec![south]).await.unwrap();
            assert_eq!(
                store.table.version().await.unwrap(),
                version_before_replace + 1
            );
            assert_eq!(store.len().await.unwrap(), 1);
            let hits = store
                .query("", &Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
                .await
                .unwrap();
            assert_eq!(hits[0].chunk.text, "south");
            assert_eq!(hits[0].chunk.heading_path, ["Compass", "Needleshard"]);
            assert_eq!(hits[0].chunk.source_regions, expected_source_regions);
            let heading_hits = store
                .query(
                    "needleshard",
                    &Embedding(vec![0.0, 1.0]),
                    1,
                    SearchScope::Unscoped,
                )
                .await
                .unwrap();
            assert_eq!(heading_hits[0].chunk.text, "south");
            assert_eq!(
                heading_hits[0].chunk.heading_path,
                ["Compass", "Needleshard"]
            );
        }

        // Reopen the same directory: the data is still there — that's durability.
        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        assert_eq!(reopened.len().await.unwrap(), 1);
        let hits = reopened
            .query("", &Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits[0].chunk.text, "south");
        assert_eq!(hits[0].chunk.heading_path, ["Compass", "Needleshard"]);
        assert_eq!(hits[0].chunk.source_regions.len(), 1);
        assert_eq!(
            hits[0].chunk.source_regions[0].location,
            SourceLocation::Page {
                number: std::num::NonZeroU32::new(7).unwrap()
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dense_cutoff_precedes_native_fusion_but_lexical_matches_are_rescued() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        store
            .upsert(vec![
                record(DocumentId::new(), 0, "needle both branches", vec![1.0, 0.0]),
                record(
                    DocumentId::new(),
                    0,
                    "needle lexical rescue",
                    vec![0.0, 1.0],
                ),
                record(DocumentId::new(), 0, "dense only", vec![0.8, 0.6]),
                record(DocumentId::new(), 0, "below cutoff", vec![-1.0, 0.0]),
            ])
            .await
            .unwrap();

        let hybrid = store
            .query(
                "needle",
                &Embedding(vec![1.0, 0.0]),
                10,
                SearchScope::Unscoped,
            )
            .await
            .unwrap();
        let texts = hybrid
            .iter()
            .map(|hit| hit.chunk.text.as_str())
            .collect::<Vec<_>>();
        assert!(texts.contains(&"needle both branches"));
        assert!(texts.contains(&"needle lexical rescue"));
        assert!(texts.contains(&"dense only"));
        assert!(!texts.contains(&"below cutoff"));
        let lexical = hybrid
            .iter()
            .find(|hit| hit.chunk.text == "needle lexical rescue")
            .unwrap();
        assert!(lexical.score < crate::DEFAULT_MIN_DENSE_SIMILARITY);

        let dense = store
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(
            dense
                .iter()
                .map(|hit| hit.chunk.text.as_str())
                .collect::<Vec<_>>(),
            vec!["needle both branches", "dense only"]
        );
        assert!(dense
            .iter()
            .all(|hit| hit.score >= crate::DEFAULT_MIN_DENSE_SIMILARITY));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dense_distance_bound_preserves_inclusive_similarity_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        store
            .upsert(vec![record(
                DocumentId::new(),
                0,
                "exact boundary",
                vec![1.0, 0.0],
            )])
            .await
            .unwrap();

        let exact = store
            .query_with_options(
                "",
                &Embedding(vec![1.0, 0.0]),
                1,
                SearchOptions::new(SearchScope::Unscoped).with_min_dense_similarity(1.0),
            )
            .await
            .unwrap();
        assert_eq!(exact.len(), 1);

        let error = store
            .query_with_options(
                "",
                &Embedding(vec![1.0, 0.0]),
                0,
                SearchOptions::new(SearchScope::Unscoped).with_min_dense_similarity(f32::NAN),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("within [0, 1]"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lexical_only_rescue_is_scoped_stable_and_uses_one_branch_rrf_scores() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let first = record(
            DocumentId::derive("lexical-only-a"),
            0,
            "lexicalneedle equal text",
            vec![0.0, 1.0],
        );
        let second = record(
            DocumentId::derive("lexical-only-b"),
            0,
            "lexicalneedle equal text",
            vec![0.0, 1.0],
        );
        let project = ProjectId::new();
        let wrong_scope = project_record(
            project,
            DocumentId::derive("lexical-only-project"),
            0,
            "lexicalneedle equal text",
            vec![0.0, 1.0],
        );
        let mut expected = [first.chunk.id, second.chunk.id];
        expected.sort_by_key(|id| id.0);
        store
            .upsert(vec![second, wrong_scope, first])
            .await
            .unwrap();

        let hits = store
            .query(
                "lexicalneedle",
                &Embedding(vec![1.0, 0.0]),
                10,
                SearchScope::Unscoped,
            )
            .await
            .unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk.id, expected[0]);
        assert_eq!(hits[1].chunk.id, expected[1]);
        assert!((hits[0].score - (1.0 / 61.0)).abs() < 1e-6);
        assert!((hits[1].score - (1.0 / 62.0)).abs() < 1e-6);
        assert!(hits
            .iter()
            .all(|hit| hit.chunk.document_id != DocumentId::derive("lexical-only-project")));
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
            let unscoped = store
                .query("", &query, 1, SearchScope::Unscoped)
                .await
                .unwrap();
            let a = store
                .query("", &query, 1, SearchScope::Project(project_a))
                .await
                .unwrap();
            let b = store
                .query("", &query, 1, SearchScope::Project(project_b))
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
                .query("", &query, 1, SearchScope::Unscoped)
                .await
                .unwrap()[0]
                .chunk
                .text,
            "unscoped"
        );
        assert_eq!(
            reopened
                .query("", &query, 1, SearchScope::Project(project_a))
                .await
                .unwrap()[0]
                .chunk
                .text,
            "project-a"
        );
        assert_eq!(
            reopened
                .query("", &query, 1, SearchScope::Project(project_b))
                .await
                .unwrap()[0]
                .chunk
                .text,
            "project-b"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn native_hybrid_search_sees_appends_but_not_staged_rows_and_prefilters_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let project = ProjectId::new();

        // The FTS index is created with the empty table. These rows are appended
        // afterward and must still participate in native hybrid search.
        store
            .upsert(vec![
                record(
                    DocumentId::new(),
                    0,
                    "needleshard exact identifier",
                    vec![0.0, 1.0],
                ),
                record(
                    DocumentId::new(),
                    0,
                    "semantic dense candidate",
                    vec![1.0, 0.0],
                ),
                project_record(
                    project,
                    DocumentId::new(),
                    0,
                    "needleshard wrong corpus",
                    vec![1.0, 0.0],
                ),
            ])
            .await
            .unwrap();

        let query = Embedding(vec![1.0, 0.0]);
        let hits = store
            .query("needleshard", &query, 2, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits
            .iter()
            .any(|hit| hit.chunk.text == "needleshard exact identifier"));
        assert!(hits
            .iter()
            .any(|hit| hit.chunk.text == "semantic dense candidate"));
        assert!(hits
            .iter()
            .all(|hit| hit.chunk.text != "needleshard wrong corpus"));

        let staged_document = DocumentId::new();
        let staged_generation = generation(1);
        store
            .stage_document_generation(
                staged_document,
                staged_generation,
                vec![record(
                    staged_document,
                    0,
                    "stagedneedle remains hidden",
                    vec![1.0, 0.0],
                )],
            )
            .await
            .unwrap();
        let before_activation = store
            .query("stagedneedle", &query, 3, SearchScope::Unscoped)
            .await
            .unwrap();
        assert!(before_activation
            .iter()
            .all(|hit| hit.chunk.text != "stagedneedle remains hidden"));

        assert!(store
            .activate_document_generation(staged_document, staged_generation)
            .await
            .unwrap());
        let after_activation = store
            .query("stagedneedle", &query, 3, SearchScope::Unscoped)
            .await
            .unwrap();
        assert!(after_activation
            .iter()
            .any(|hit| hit.chunk.text == "stagedneedle remains hidden"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn native_hybrid_split_branch_tie_uses_fused_chunk_id_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let dense = record(
            DocumentId::derive("tie-dense"),
            0,
            "dense candidate",
            vec![1.0, 0.0],
        );
        let lexical = record(
            DocumentId::derive("tie-lexical"),
            0,
            "lexicalneedle candidate",
            vec![0.0, 1.0],
        );
        let expected = dense.chunk.id.0.min(lexical.chunk.id.0);
        store.upsert(vec![dense, lexical]).await.unwrap();

        let hits = store
            .query(
                "lexicalneedle",
                &Embedding(vec![1.0, 0.0]),
                1,
                SearchScope::Unscoped,
            )
            .await
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.id.0, expected);
        assert!((hits[0].score - (1.0 / 61.0)).abs() < 1e-6);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn native_hybrid_orders_equal_branch_scores_by_chunk_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let first = record(
            DocumentId::derive("hybrid-equal-a"),
            0,
            "equalneedle same text",
            vec![1.0, 0.0],
        );
        let second = record(
            DocumentId::derive("hybrid-equal-b"),
            0,
            "equalneedle same text",
            vec![1.0, 0.0],
        );
        let mut expected = [first.chunk.id, second.chunk.id];
        expected.sort_by_key(|id| id.0);
        store.upsert(vec![second, first]).await.unwrap();

        let hits = store
            .query(
                "equalneedle",
                &Embedding(vec![1.0, 0.0]),
                2,
                SearchScope::Unscoped,
            )
            .await
            .unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk.id, expected[0]);
        assert_eq!(hits[1].chunk.id, expected[1]);
        assert!((hits[0].score - (2.0 / 61.0)).abs() < 1e-6);
        assert!((hits[1].score - (2.0 / 62.0)).abs() < 1e-6);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reconnect_catches_up_fts_index_and_preserves_hybrid_search() {
        let dir = tempfile::tempdir().unwrap();
        let uri = dir.path().to_str().unwrap();
        {
            let store = LanceVectorStore::connect(uri, 2).await.unwrap();
            store
                .upsert(vec![record(
                    DocumentId::derive("reconnect-hybrid"),
                    0,
                    "reconnectneedle exact match",
                    vec![0.0, 1.0],
                )])
                .await
                .unwrap();
            let stats = store
                .table
                .index_stats(&store.text_index_name)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stats.num_unindexed_rows, 1);
        }

        let reopened = LanceVectorStore::connect(uri, 2).await.unwrap();
        let stats = reopened
            .table
            .index_stats(&reopened.text_index_name)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stats.num_unindexed_rows, 0);
        assert_eq!(stats.num_indexed_rows, 1);
        let hits = reopened
            .query(
                "reconnectneedle",
                &Embedding(vec![1.0, 0.0]),
                1,
                SearchScope::Unscoped,
            )
            .await
            .unwrap();
        assert_eq!(hits[0].chunk.text, "reconnectneedle exact match");
        assert!((hits[0].score - (1.0 / 61.0)).abs() < 1e-6);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fts_index_is_optimized_after_bounded_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();

        for index in 0..INDEX_MUTATION_THRESHOLD {
            store
                .upsert(vec![record(
                    DocumentId::derive(&format!("index-maintenance-{index}")),
                    0,
                    &format!("maintenance row {index}"),
                    vec![1.0, 0.0],
                )])
                .await
                .unwrap();
            if index + 1 == INDEX_MUTATION_THRESHOLD - 1 {
                let stats = store
                    .table
                    .index_stats(&store.text_index_name)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(stats.num_unindexed_rows, INDEX_MUTATION_THRESHOLD - 1);
            }
        }

        let stats = store
            .table
            .index_stats(&store.text_index_name)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stats.num_unindexed_rows, 0);
        assert_eq!(stats.num_indexed_rows, INDEX_MUTATION_THRESHOLD);
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
            .query("", &query, 1, SearchScope::Project(project_a))
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .activate_document_generation(a_doc, first)
            .await
            .unwrap());
        assert_eq!(
            store
                .query("", &query, 1, SearchScope::Project(project_a))
                .await
                .unwrap()[0]
                .chunk
                .text,
            "project-a-active"
        );
        assert_eq!(
            store
                .query("", &query, 1, SearchScope::Project(project_b))
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
            .query("", &Embedding(vec![0.0, 1.0]), 5, SearchScope::Unscoped)
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
    async fn rejects_malformed_source_regions_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        let mut invalid = record(DocumentId::new(), 0, "é", vec![1.0, 0.0]);
        invalid.chunk.source_regions = vec![SourceRegion {
            span: ByteSpan::new(1, 2),
            location: SourceLocation::Page {
                number: std::num::NonZeroU32::new(1).unwrap(),
            },
        }];

        assert!(store.upsert(vec![invalid]).await.is_err());
        assert!(store.is_empty().await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_index_and_k_zero_return_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::connect(dir.path().to_str().unwrap(), 2)
            .await
            .unwrap();
        assert!(store
            .query("", &Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .query(
                "missing",
                &Embedding(vec![1.0, 0.0]),
                5,
                SearchScope::Unscoped,
            )
            .await
            .unwrap()
            .is_empty());
        store
            .upsert(vec![record(DocumentId::new(), 0, "a", vec![1.0, 0.0])])
            .await
            .unwrap();
        assert!(store
            .query("", &Embedding(vec![1.0, 0.0]), 0, SearchScope::Unscoped)
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
            .query("", &Embedding(vec![0.0, 1.0]), 10, SearchScope::Unscoped)
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
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
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
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
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
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
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
                    "",
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
                "",
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
                "",
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
                "",
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
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert!(hits.is_empty());
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
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert!(hits.iter().any(|hit| hit.chunk.text == "first"));
        assert!(hits.iter().all(|hit| hit.chunk.text != "other"));
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
                    "",
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
                    "",
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
            .query("", &Embedding(vec![0.0, 1.0]), 10, SearchScope::Unscoped)
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
