//! Conversation outputs (deliverables) over HTTP.
//!
//! These routes are the whole of the output surface: the catalog, one output's
//! bounded text preview, one revision's complete bytes, version history,
//! restore, user edits, and soft delete with its undo. They were Tauri commands
//! reading `tidebreak-core`'s `Store` directly, which made every one of them
//! unreachable without the desktop shell; moving them here is what makes an
//! output readable and exportable headlessly (decision record 7).
//!
//! Two things deliberately did not move. **Export-to-path stays client-side**:
//! the route serves bytes and the caller decides where they land, which is the
//! only shape that works for both a native save dialog and a CLI writing to an
//! argument. And **restore stays append-only** — restoring an earlier version
//! publishes a new head revision carrying that version's content, so nothing is
//! rewound and no revision loses its bytes.
//!
//! The payload shapes are the ones the renderer already validates, camelCase
//! and all: this is a transport move, and a field rename here would be a second
//! change wearing the first one's clothes.

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use tidebreak_core::{
    deliverable_media_type, media_type_is_text, revision_byte_ceiling, AgentError,
    AssistantCitationId, ChatId, ChatTranscriptSnapshot, CitationLocator, DocumentId, OutputId,
    OutputRecord, OutputRevision, OutputRevisionId, ResultEntryKind, ToolCallRecord,
    ToolCallStatus, ToolResultPreview, TurnId, WEB_SEARCH_TOOL,
};

use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::output_files::{
    open_chat_scratch, read_output_revision_bytes, require_live_output, require_output_revision,
};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

/// Most outputs a catalog answer carries. One more than this is fetched so the
/// answer can say it was truncated rather than silently ending.
const MAX_DELIVERABLES: usize = 100;
/// Characters of text a preview carries. Bigger outputs are read through the
/// content route.
const MAX_PREVIEW_CHARACTERS: usize = 100_000;
/// Most durable evidence references projected onto one revision. Document
/// citations take precedence over web-search rows when a turn retrieved more.
const MAX_OUTPUT_REVISION_SOURCES: usize = 20;

/// Body ceiling for a user edit. The stored ceiling for a text output is 512
/// KiB; the request carries that content JSON-escaped, so the transport limit
/// is twice it — worst-case escaping — and the store still refuses anything
/// over the real ceiling.
pub const MAX_OUTPUT_REVISION_BODY_BYTES: usize = 2 * tidebreak_core::MAX_DELIVERABLE_BYTES;

/// One row of the outputs catalog.
///
/// The output records are also read back by the CLI through [`crate::wire`],
/// so they reject unknown keys the way the renderer's guards do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliverableSummary {
    pub output_id: OutputId,
    pub filename: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub revision_count: u32,
    pub updated_at: chrono::DateTime<Utc>,
    /// Background run that produced the current revision, when the output was
    /// submitted by a background agent rather than a foreground turn. A display
    /// key, not authority.
    pub producing_run_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliverablesCatalog {
    pub deliverables: Vec<DeliverableSummary>,
    /// Whether the conversation has more outputs than one answer carries.
    pub truncated: bool,
}

/// A bounded text preview of one exact revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliverablePreview {
    pub output_id: OutputId,
    pub filename: String,
    pub media_type: String,
    /// Lets a detail view gate its version-history affordance without a second
    /// catalog fetch.
    pub revision_count: u32,
    /// The revision this preview (or empty binary placeholder) was built from,
    /// so a viewer can address the same immutable bytes.
    pub revision_id: OutputRevisionId,
    pub content: String,
    pub truncated: bool,
}

/// One row of an output's version history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputRevisionInfo {
    pub revision_id: OutputRevisionId,
    pub ordinal: u32,
    pub size_bytes: u64,
    pub created_at: chrono::DateTime<Utc>,
    pub produced_by: OutputRevisionProducer,
    pub is_current: bool,
    /// Durable evidence retrieved by the foreground turn that produced this
    /// revision. User and background-run revisions have no turn, so they carry
    /// an empty list rather than borrowing evidence from another producer.
    pub sources: Vec<OutputRevisionSource>,
}

/// Who produced a revision.
///
/// Spelled in camelCase on the wire because the output routes answer in the
/// shape the desktop renderer already validates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputRevisionProducer {
    /// A foreground turn.
    Agent,
    /// A background run.
    BackgroundAgent,
    /// An edit or a restore.
    User,
}

impl OutputRevisionProducer {
    /// The wire spelling, for a client that prints the producer without a
    /// serde round trip. Pinned to the serde form by a test in [`crate::wire`].
    pub fn as_str(self) -> &'static str {
        match self {
            OutputRevisionProducer::Agent => "agent",
            OutputRevisionProducer::BackgroundAgent => "backgroundAgent",
            OutputRevisionProducer::User => "user",
        }
    }
}

/// One durable evidence reference belonging to a revision's producing turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum OutputRevisionSource {
    Document {
        citation_id: AssistantCitationId,
        document_id: DocumentId,
        locator: CitationLocator,
    },
    Web {
        url: String,
        label: String,
        domain: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputRevisionsCatalog {
    pub output_id: OutputId,
    pub revisions: Vec<OutputRevisionInfo>,
}

/// Publish a user's edit of a text output as a new user-authored revision.
///
/// `expected_revision_id` is the revision the editor was opened on. It is a
/// precondition, not a hint: if anything published a newer revision in the
/// meantime the save is refused and the caller is told which revision is
/// current, so the person reloads and reconciles instead of silently discarding
/// work they never saw.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveOutputRevisionBody {
    expected_revision_id: OutputRevisionId,
    content: String,
}

/// The outcome of one save. A conflict is an expected product state with its own
/// reconcile path, not a failure, so it travels as a value rather than an error.
#[derive(Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "status"
)]
pub enum SaveOutputRevisionResult {
    /// The edit published a new revision; the payload previews it.
    Saved(DeliverablePreview),
    /// Another revision became current after the editor opened.
    Conflict {
        /// The revision that is current now — what the caller should reload.
        current_revision_id: OutputRevisionId,
    },
}

/// Optional revision selector. Omitted means the output's current revision.
#[derive(Debug, Deserialize)]
pub struct RevisionQuery {
    #[serde(default)]
    revision_id: Option<OutputRevisionId>,
}

/// `GET /chats/{chat_id}/outputs` — the conversation's live outputs.
pub async fn list_chat_outputs(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(chat_id): Path<ChatId>,
) -> Result<Json<DeliverablesCatalog>, ServerError> {
    store.require_chat(chat_id).await?;
    let mut outputs = state
        .store
        .list_outputs(chat_id, (MAX_DELIVERABLES + 1) as u64)
        .await?;
    let truncated = outputs.len() > MAX_DELIVERABLES;
    outputs.truncate(MAX_DELIVERABLES);
    let mut deliverables = Vec::with_capacity(outputs.len());
    for output in outputs {
        let revision = state
            .store
            .get_output_revision(output.current_revision)
            .await?
            .ok_or_else(|| ServerError::internal("an output has no current revision"))?;
        deliverables.push(summary_from_record(&output, &revision).map_err(ServerError::internal)?);
    }
    Ok(Json(DeliverablesCatalog {
        deliverables,
        truncated,
    }))
}

/// `GET /chats/{chat_id}/outputs/{output_id}` — the bounded text preview of the
/// current revision.
pub async fn get_chat_output(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, output_id)): Path<(ChatId, OutputId)>,
) -> Result<Json<DeliverablePreview>, ServerError> {
    store.require_chat(chat_id).await?;
    let (output, revision) = require_live_output(&state.store, chat_id, output_id)
        .await
        .map_err(ServerError::not_found)?;
    Ok(Json(
        revision_preview(&state, chat_id, &output, &revision).await?,
    ))
}

/// `GET /chats/{chat_id}/outputs/{output_id}/revisions/{revision_id}` — the
/// bounded text preview of one exact, possibly superseded, revision.
pub async fn get_chat_output_revision(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, output_id, revision_id)): Path<(ChatId, OutputId, OutputRevisionId)>,
) -> Result<Json<DeliverablePreview>, ServerError> {
    store.require_chat(chat_id).await?;
    let (output, revision) = require_output_revision(&state.store, chat_id, output_id, revision_id)
        .await
        .map_err(ServerError::not_found)?;
    Ok(Json(
        revision_preview(&state, chat_id, &output, &revision).await?,
    ))
}

/// `GET /chats/{chat_id}/outputs/{output_id}/content[?revision_id=…]` — one
/// revision's complete bytes.
///
/// Unlike the preview this is neither capped nor limited to text media types:
/// it is what an inline viewer draws and what an export writes to disk. The
/// bytes are served raw so a client streams them to a file rather than decoding
/// a base64 field.
pub async fn get_chat_output_content(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, output_id)): Path<(ChatId, OutputId)>,
    Query(query): Query<RevisionQuery>,
) -> Result<Response, ServerError> {
    store.require_chat(chat_id).await?;
    let (output, revision) = match query.revision_id {
        None => require_live_output(&state.store, chat_id, output_id).await,
        Some(revision_id) => {
            require_output_revision(&state.store, chat_id, output_id, revision_id).await
        }
    }
    .map_err(ServerError::not_found)?;
    let bytes = revision_bytes(&state, chat_id, &output, &revision).await?;
    // The stored media type is agent-controlled text, so it is echoed only in
    // the two headers a client reads programmatically and never used to invite
    // a browser to render the bytes in place.
    let media_type = HeaderValue::from_str(&output.media_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    Ok((
        [
            (header::CONTENT_TYPE, media_type),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment"),
            ),
            (
                header::HeaderName::from_static("x-tidebreak-revision-id"),
                HeaderValue::from_str(&revision.id.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// `GET /chats/{chat_id}/outputs/{output_id}/revisions` — the version history,
/// as the store orders it.
pub async fn list_chat_output_revisions(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, output_id)): Path<(ChatId, OutputId)>,
) -> Result<Json<OutputRevisionsCatalog>, ServerError> {
    store.require_chat(chat_id).await?;
    let (output, _) = require_live_output(&state.store, chat_id, output_id)
        .await
        .map_err(ServerError::not_found)?;
    let revisions = state.store.list_output_revisions(output.id).await?;
    let sources_by_turn = if revisions.iter().any(|revision| revision.turn_id.is_some()) {
        let (transcript, tool_calls) = tokio::try_join!(
            store.get_chat_transcript(chat_id),
            store.list_tool_calls(chat_id),
        )?;
        output_revision_sources(transcript.as_ref(), &tool_calls)
    } else {
        HashMap::new()
    };
    Ok(Json(OutputRevisionsCatalog {
        output_id: output.id,
        revisions: revisions
            .into_iter()
            .map(|revision| OutputRevisionInfo {
                revision_id: revision.id,
                ordinal: revision.ordinal,
                size_bytes: revision.byte_len,
                created_at: revision.created_at,
                produced_by: if revision.producing_run_id.is_some() {
                    OutputRevisionProducer::BackgroundAgent
                } else if revision.turn_id.is_some() {
                    OutputRevisionProducer::Agent
                } else {
                    OutputRevisionProducer::User
                },
                is_current: revision.id == output.current_revision,
                sources: revision
                    .turn_id
                    .and_then(|turn_id| sources_by_turn.get(&turn_id).cloned())
                    .unwrap_or_default(),
            })
            .collect(),
    }))
}

#[derive(Default)]
struct RevisionSources {
    values: Vec<OutputRevisionSource>,
    documents: HashSet<(DocumentId, CitationLocator)>,
    web_urls: HashSet<String>,
}

/// Group the conversation's durable evidence by the exact turn that retrieved
/// it. This is a read-time projection: citations and tool previews remain the
/// authorities, so a revision can never invent or retain a second copy of a
/// source that diverges from the transcript.
fn output_revision_sources(
    transcript: Option<&ChatTranscriptSnapshot>,
    tool_calls: &[ToolCallRecord],
) -> HashMap<TurnId, Vec<OutputRevisionSource>> {
    let mut by_turn: HashMap<TurnId, RevisionSources> = HashMap::new();

    if let Some(transcript) = transcript {
        let message_turns = transcript
            .messages
            .iter()
            .map(|message| (message.id, message.turn_id))
            .collect::<HashMap<_, _>>();
        for snapshot in &transcript.citations {
            let Some(turn_id) = message_turns.get(&snapshot.message_id).copied() else {
                continue;
            };
            let sources = by_turn.entry(turn_id).or_default();
            let citation = &snapshot.citation;
            let key = (citation.document_id, citation.locator.clone());
            if sources.values.len() < MAX_OUTPUT_REVISION_SOURCES && sources.documents.insert(key) {
                sources.values.push(OutputRevisionSource::Document {
                    citation_id: citation.id,
                    document_id: citation.document_id,
                    locator: citation.locator.clone(),
                });
            }
        }
    }

    // Document citations are inserted first on purpose. Search results fill
    // the remaining bounded slots in durable call/result order.
    for call in tool_calls {
        if call.name != WEB_SEARCH_TOOL || call.status != ToolCallStatus::Completed {
            continue;
        }
        let Some(ToolResultPreview::Entries { entries, .. }) = call.result_preview.as_ref() else {
            continue;
        };
        let sources = by_turn.entry(call.turn_id).or_default();
        for entry in entries {
            if sources.values.len() >= MAX_OUTPUT_REVISION_SOURCES {
                break;
            }
            if entry.kind != ResultEntryKind::Link {
                continue;
            }
            let Some(url) = entry.url.as_deref() else {
                continue;
            };
            let Some(domain) = web_domain(url) else {
                continue;
            };
            if !sources.web_urls.insert(url.to_owned()) {
                continue;
            }
            sources.values.push(OutputRevisionSource::Web {
                url: url.to_owned(),
                label: if entry.label.is_empty() {
                    url.to_owned()
                } else {
                    entry.label.clone()
                },
                domain,
            });
        }
    }

    by_turn
        .into_iter()
        .map(|(turn_id, sources)| (turn_id, sources.values))
        .collect()
}

fn web_domain(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;
    Some(host.strip_prefix("www.").unwrap_or(host).to_owned())
}

/// `POST /chats/{chat_id}/outputs/{output_id}/revisions/{revision_id}/restore`
/// — restore an earlier version by appending a new revision carrying its
/// content. Append-only: nothing is rewound or lost.
pub async fn restore_chat_output_revision(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, output_id, revision_id)): Path<(ChatId, OutputId, OutputRevisionId)>,
) -> Result<Json<DeliverableSummary>, ServerError> {
    store.require_chat(chat_id).await?;
    let scratch = chat_scratch(&state, chat_id).await?;
    tidebreak_core::restore_output_to_revision(
        &*state.store,
        &scratch,
        chat_id,
        output_id,
        revision_id,
        Utc::now(),
    )
    .await?;
    current_summary(&state, chat_id, output_id).await
}

/// `POST /chats/{chat_id}/outputs/{output_id}/revisions` — publish an edited
/// text output as a new user-authored revision.
///
/// Append-only, exactly like a restore: the previous revision keeps its id and
/// its bytes, and this publishes a new head marked as produced by the user.
pub async fn save_chat_output_revision(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, output_id)): Path<(ChatId, OutputId)>,
    Json(body): Json<SaveOutputRevisionBody>,
) -> Result<Json<SaveOutputRevisionResult>, ServerError> {
    store.require_chat(chat_id).await?;
    let scratch = chat_scratch(&state, chat_id).await?;
    match tidebreak_core::save_user_output_revision(
        &*state.store,
        &scratch,
        chat_id,
        output_id,
        body.expected_revision_id,
        &body.content,
        Utc::now(),
    )
    .await
    {
        Ok(_) => {
            let (output, revision) = require_live_output(&state.store, chat_id, output_id)
                .await
                .map_err(ServerError::not_found)?;
            Ok(Json(SaveOutputRevisionResult::Saved(
                revision_preview(&state, chat_id, &output, &revision).await?,
            )))
        }
        Err(AgentError::OutputRevisionConflict {
            current_revision, ..
        }) => Ok(Json(SaveOutputRevisionResult::Conflict {
            current_revision_id: current_revision,
        })),
        Err(error) => Err(error.into()),
    }
}

/// `DELETE /chats/{chat_id}/outputs/{output_id}` — soft-delete an output. The
/// explicit inverse is the restore route below, which the catalog offers as
/// Undo.
pub async fn delete_chat_output(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, output_id)): Path<(ChatId, OutputId)>,
) -> Result<Json<DeliverableSummary>, ServerError> {
    store.require_chat(chat_id).await?;
    let (output, revision) = require_live_output(&state.store, chat_id, output_id)
        .await
        .map_err(ServerError::not_found)?;
    let summary = summary_from_record(&output, &revision).map_err(ServerError::internal)?;
    state.store.delete_output(output.id, Utc::now()).await?;
    Ok(Json(summary))
}

/// `POST /chats/{chat_id}/outputs/{output_id}/restore` — undo a soft delete.
pub async fn restore_chat_output(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, output_id)): Path<(ChatId, OutputId)>,
) -> Result<Json<DeliverableSummary>, ServerError> {
    store.require_chat(chat_id).await?;
    // A restore targets a soft-deleted output, so it cannot go through the
    // live-only lookup. Bind it to the exact conversation before clearing the
    // retraction.
    let output = state
        .store
        .get_output(output_id)
        .await?
        .filter(|output| output.chat_id == chat_id)
        .ok_or_else(|| ServerError::not_found("Output not found in this conversation"))?;
    state.store.restore_output(output.id, Utc::now()).await?;
    current_summary(&state, chat_id, output_id).await
}

async fn current_summary(
    state: &AppState,
    chat_id: ChatId,
    output_id: OutputId,
) -> Result<Json<DeliverableSummary>, ServerError> {
    let (output, revision) = require_live_output(&state.store, chat_id, output_id)
        .await
        .map_err(ServerError::not_found)?;
    Ok(Json(
        summary_from_record(&output, &revision).map_err(ServerError::internal)?,
    ))
}

/// Open the conversation's private scratch directory for an append-only write.
async fn chat_scratch(state: &AppState, chat_id: ChatId) -> Result<cap_std::fs::Dir, ServerError> {
    let scratch_root = state.config.data_dir.join("scratch");
    tokio::task::spawn_blocking(move || open_chat_scratch(&scratch_root, chat_id))
        .await
        .map_err(|_| ServerError::internal("could not open private output storage"))?
        .map_err(ServerError::not_found)
}

async fn revision_bytes(
    state: &AppState,
    chat_id: ChatId,
    output: &OutputRecord,
    revision: &OutputRevision,
) -> Result<Vec<u8>, ServerError> {
    let scratch_root = state.config.data_dir.join("scratch");
    let output = output.clone();
    let revision = revision.clone();
    tokio::task::spawn_blocking(move || {
        read_output_revision_bytes(&scratch_root, chat_id, &output, &revision)
    })
    .await
    .map_err(|_| ServerError::internal("could not read that output"))?
    .map_err(ServerError::not_found)
}

/// Build the bounded preview of one exact revision's content.
async fn revision_preview(
    state: &AppState,
    chat_id: ChatId,
    output: &OutputRecord,
    revision: &OutputRevision,
) -> Result<DeliverablePreview, ServerError> {
    // Binary artifacts have no inline text preview; a client keys its viewer
    // (or export-only placeholder) off the media type and uses `revision_id` to
    // fetch the full bytes when a viewer can draw them.
    if !media_type_is_text(&output.media_type) {
        return Ok(DeliverablePreview {
            output_id: output.id,
            filename: output.filename.clone(),
            media_type: output.media_type.clone(),
            revision_count: output.revision_count,
            revision_id: revision.id,
            content: String::new(),
            truncated: false,
        });
    }
    let bytes = revision_bytes(state, chat_id, output, revision).await?;
    preview_from_bytes(output, revision, bytes)
        .map_err(|message| ServerError::unprocessable_kind("output_not_text", message))
}

fn preview_from_bytes(
    output: &OutputRecord,
    revision: &OutputRevision,
    bytes: Vec<u8>,
) -> Result<DeliverablePreview, String> {
    let text =
        String::from_utf8(bytes).map_err(|_| "Output is not a supported text file".to_owned())?;
    if text.contains('\0')
        || deliverable_media_type(&output.filename) != Some(output.media_type.as_str())
    {
        return Err("Output is not a supported text file".to_owned());
    }
    let mut characters = text.chars();
    let content: String = characters.by_ref().take(MAX_PREVIEW_CHARACTERS).collect();
    let truncated = characters.next().is_some();
    Ok(DeliverablePreview {
        output_id: output.id,
        filename: output.filename.clone(),
        media_type: output.media_type.clone(),
        revision_count: output.revision_count,
        revision_id: revision.id,
        content,
        truncated,
    })
}

fn summary_from_record(
    output: &OutputRecord,
    revision: &OutputRevision,
) -> Result<DeliverableSummary, String> {
    // Text outputs derive their media type from the filename; binary artifacts
    // carry an explicit media type with an arbitrary extension, so only text is
    // held to the filename-derived type. Each kind keeps its own size ceiling.
    if output.deleted_at.is_some()
        || output.current_revision != revision.id
        || output.id != revision.output_id
        || output.revision_count == 0
        || revision.byte_len > revision_byte_ceiling(&output.media_type) as u64
        || (media_type_is_text(&output.media_type)
            && deliverable_media_type(&output.filename) != Some(output.media_type.as_str()))
    {
        return Err("Could not load this conversation's outputs".to_owned());
    }
    Ok(DeliverableSummary {
        output_id: output.id,
        filename: output.filename.clone(),
        media_type: output.media_type.clone(),
        size_bytes: revision.byte_len,
        revision_count: output.revision_count,
        updated_at: output.updated_at,
        producing_run_id: revision.producing_run_id.map(|run_id| *run_id.as_uuid()),
    })
}
