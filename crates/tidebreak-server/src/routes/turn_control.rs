//! Route handlers extracted from the parent `routes` module.

use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::Deserialize;

use tidebreak_core::{
    AcceptTurnOutcome, AcceptTurnSteerOutcome, ChatId, DocumentId, RequestTurnCancellationOutcome,
    TurnId, TurnSteer, TurnSteerId,
};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::providers::{self};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

use super::agent_runs::signal_origin_sandbox_runs_after_commit;
use super::image_attachment;
use super::providers_models::resolve_executable_chat_model;

/// Body of `POST /chats/{id}/messages`.
#[derive(Debug, Deserialize)]
pub struct PostMessage {
    /// Stable client-generated identity for acceptance and ambiguous retries.
    pub turn_id: TurnId,
    /// The user's input for this turn.
    pub content: String,
    /// Ids returned by the image attachment publish endpoint, in display order.
    ///
    /// Only identity crosses the wire. The server re-derives every attachment's
    /// format and dimensions from the stored bytes, so a caller cannot describe
    /// an image as something it is not.
    #[serde(default)]
    pub attachments: Vec<uuid::Uuid>,
    /// Chat-owned document ids returned by the file ingest endpoint.
    #[serde(default)]
    pub file_attachments: Vec<DocumentId>,
    /// Skills the user explicitly invoked for this turn, by name.
    ///
    /// Absent or empty is the ordinary send, where the model routes on the
    /// prompt's skill catalog itself. Every name must be a currently enabled
    /// skill; one that is not refuses the turn rather than being dropped.
    #[serde(default)]
    pub invoked_skills: Vec<String>,
    /// Whether any of the submitted text came from voice transcription.
    ///
    /// The server turns this into canonical model-only guidance; callers cannot
    /// submit arbitrary hidden prompt text.
    #[serde(default)]
    pub voice_input_used: bool,
    /// Queue instead of refusing when the chat has a live turn.
    ///
    /// With this set, `ChatBusy` durably parks the validated message as a
    /// [`tidebreak_core::QueuedTurn`]; the promoter runs it as its own turn
    /// once the chat is free. An idle chat sends immediately either way.
    #[serde(default)]
    pub queue: bool,
}

/// Refuse a turn that invokes a skill the install cannot actually run.
///
/// The catalog the user picked from and the catalog this turn will stage are
/// read at different moments, and a skill can be disabled or uninstalled in
/// between. Honouring the rest of the list would send the turn with an
/// instruction to read a manifest that is not there, so an unknown or disabled
/// name refuses the whole submission before any model call — the same posture
/// as a turn carrying images a model cannot see. The refusal names the skill
/// so a client can drop it and resubmit.
async fn require_invocable_skills(state: &AppState, invoked: &[String]) -> Result<(), ServerError> {
    if invoked.is_empty() {
        return Ok(());
    }
    if invoked.len() > tidebreak_core::TurnRun::MAX_INVOKED_SKILLS {
        return Err(ServerError::bad_request_kind(
            "too_many_invoked_skills",
            format!(
                "a turn may invoke at most {} skills",
                tidebreak_core::TurnRun::MAX_INVOKED_SKILLS
            ),
        ));
    }
    let mut distinct = std::collections::HashSet::with_capacity(invoked.len());
    for name in invoked {
        if !distinct.insert(name.as_str()) {
            return Err(ServerError::bad_request_kind(
                "duplicate_invoked_skill",
                format!("skill `{name}` was invoked more than once"),
            ));
        }
    }
    let available = match state.code_execution.as_ref() {
        Some(exec) => exec.skill_catalog().await,
        None => Vec::new(),
    };
    for name in invoked {
        if !available.iter().any(|skill| skill.name == *name) {
            return Err(ServerError::bad_request_kind(
                "invoked_skill_unavailable",
                format!("skill `{name}` is not installed or is not enabled"),
            ));
        }
    }
    Ok(())
}

/// Resolve published attachment ids into authoritative image identity.
///
/// The bytes are inspected again here rather than trusting what the publish
/// response said, because nothing durable connects the two requests and the
/// metadata persisted with the turn must describe the bytes that actually exist.
async fn resolve_message_attachments(
    state: &AppState,
    ids: &[uuid::Uuid],
) -> Result<Vec<tidebreak_core::ImageRef>, ServerError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > tidebreak_core::MAX_MESSAGE_ATTACHMENTS {
        return Err(ServerError::bad_request_kind(
            "too_many_image_attachments",
            format!(
                "a message may carry at most {} image attachments",
                tidebreak_core::MAX_MESSAGE_ATTACHMENTS
            ),
        ));
    }
    let mut images = Vec::with_capacity(ids.len());
    for &id in ids {
        let missing = || {
            ServerError::bad_request_kind(
                "image_attachment_not_found",
                format!("image attachment {id} has not been published"),
            )
        };
        let bytes = state.blobs.get(id).await?.ok_or_else(missing)?;
        let image = image_attachment::inspect_image_bytes(&bytes)?;
        // Attachment ids are content addresses. Bytes that do not hash back to
        // the requested id are some other blob, so the reference is unresolved
        // rather than merely mismatched.
        if image.blob_id != id {
            return Err(missing());
        }
        images.push(image);
    }
    Ok(images)
}

async fn resolve_file_attachments(
    store: &ScopedStore,
    chat_id: ChatId,
    ids: &[DocumentId],
) -> Result<Vec<DocumentId>, ServerError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > tidebreak_core::MAX_MESSAGE_ATTACHMENTS {
        return Err(ServerError::bad_request_kind(
            "too_many_attachments",
            format!(
                "a message may carry at most {} attachments",
                tidebreak_core::MAX_MESSAGE_ATTACHMENTS
            ),
        ));
    }
    let mut distinct = std::collections::HashSet::with_capacity(ids.len());
    for &id in ids {
        if !distinct.insert(id) {
            return Err(ServerError::bad_request_kind(
                "duplicate_file_attachment",
                format!("file attachment {id} was submitted more than once"),
            ));
        }
        let document = store.get_document(id).await?.ok_or_else(|| {
            ServerError::bad_request_kind(
                "file_attachment_not_found",
                format!("file attachment {id} has not been imported"),
            )
        })?;
        let source_blob = document.source_blob.as_ref();
        if document.chat_id != Some(chat_id)
            || document.project_id.is_some()
            || source_blob.is_none()
        {
            return Err(ServerError::bad_request_kind(
                "file_attachment_not_found",
                format!("file attachment {id} has not been imported for chat {chat_id}"),
            ));
        }
        if source_blob.is_some_and(|blob| blob.byte_len > tidebreak_core::MAX_IMAGE_BYTES) {
            return Err(ServerError::bad_request_kind(
                "file_attachment_too_large",
                "files attached to a message must be 16 MB or smaller",
            ));
        }
    }
    Ok(ids.to_vec())
}

/// Refuse a turn whose model cannot see the images it carries.
///
/// Stripping the images would leave the model answering confidently about
/// something it never received, and silently switching models would change the
/// answer's author behind the user's back. Refusing is the only option that
/// leaves the user in control, so the error is machine-readable and the client
/// can offer to change the model or drop the attachments.
async fn require_image_capable_model(state: &AppState, model: &str) -> Result<(), ServerError> {
    if !state.resolver.enforces_model_registry() {
        return Ok(());
    }
    let Some(policy) = providers::resolve_model_policy(&*state.store, model, true).await? else {
        return Err(ServerError::bad_request_kind(
            "unknown_model",
            format!("model `{model}` is not registered for that provider"),
        ));
    };
    require_image_input(&policy)
}

/// The capability decision itself, separated so it can be exercised against a
/// constructed policy rather than only against whatever the registry happens to
/// advertise today.
fn require_image_input(policy: &providers::ResolvedModelPolicy) -> Result<(), ServerError> {
    if policy
        .input_modalities
        .contains(&crate::model_registry::InputModality::Image)
    {
        return Ok(());
    }
    Err(ServerError::conflict_kind(
        "model_image_input_unsupported",
        format!(
            "model `{}` does not accept image input; choose a model that does, or send the message without images",
            policy.display_name
        ),
    ))
}

#[cfg(test)]
mod image_capability_tests {
    use super::*;
    use crate::model_registry::{InputModality, ModelSpec};
    use crate::providers::ProviderKind;

    const TEXT_ONLY: ModelSpec = ModelSpec {
        id: "text-only-model",
        display_name: "Text Only Model",
        provider: ProviderKind::Anthropic,
        verification: crate::model_registry::VerificationTier::Unverified,
        recommended: false,
        context_window: 200_000,
        max_output_tokens: 64_000,
        input_modalities: &[InputModality::Text],
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: &[],
    };

    #[test]
    fn advertised_image_input_is_the_only_thing_that_admits_a_turn_with_images() {
        let native_model = crate::model_registry::find("claude-opus-5")
            .expect("the default curated Anthropic model is registered");
        assert!(
            require_image_input(&providers::ResolvedModelPolicy::curated(native_model)).is_ok()
        );

        let refused = require_image_input(&providers::ResolvedModelPolicy::curated(&TEXT_ONLY))
            .expect_err("a text-only model must refuse a turn carrying images");
        assert_eq!(refused.kind(), "model_image_input_unsupported");
    }
}

/// `POST /chats/{id}/messages` — durably accept a message and queue its turn.
///
/// Returns `202 Accepted` after the input and queued turn commit; a supervised
/// worker claims it asynchronously and journals events for replay/live delivery.
/// Repeating an exact `turn_id` and payload is idempotent. `404` if the chat
/// doesn't exist, `409` if the identity names different input or another turn
/// already owns the chat's single durable live slot.
///
/// Published image attachments may be referenced by id. They commit with the
/// message, and a retry that names different images is an identity conflict
/// rather than a silent acceptance of the first submission's images. A turn
/// carrying images against a model that does not accept image input is refused
/// with `model_image_input_unsupported`.
pub async fn post_message(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(body): Json<PostMessage>,
) -> Result<StatusCode, ServerError> {
    if body.turn_id.0.is_nil() {
        return Err(ServerError::bad_request("turn_id must not be nil"));
    }
    if body.content.trim().is_empty() || body.content.contains('\0') {
        return Err(ServerError::bad_request(
            "message content must be non-empty and contain no NUL characters",
        ));
    }
    let chat = store.require_chat(id).await?;

    // An ambiguous HTTP retry names only its turn and content, not the resolved
    // model snapshot. Reuse the first acceptance's immutable model so a settings
    // change between attempts cannot turn the same request into a conflict.
    let existing_turn = store.get_turn_run(body.turn_id).await?;
    let model = if let Some(existing) = existing_turn.as_ref() {
        if existing.chat_id != id {
            return Err(ServerError::conflict(format!(
                "turn {} was already accepted by another chat",
                body.turn_id
            )));
        }
        existing.model.clone()
    } else {
        resolve_executable_chat_model(&state, &chat).await?
    };
    require_invocable_skills(&state, &body.invoked_skills).await?;
    let images = resolve_message_attachments(&state, &body.attachments).await?;
    let documents = resolve_file_attachments(&store, id, &body.file_attachments).await?;
    if images.len().saturating_add(documents.len()) > tidebreak_core::MAX_MESSAGE_ATTACHMENTS {
        return Err(ServerError::bad_request_kind(
            "too_many_attachments",
            format!(
                "a message may carry at most {} attachments",
                tidebreak_core::MAX_MESSAGE_ATTACHMENTS
            ),
        ));
    }
    if !images.is_empty() && existing_turn.is_none() {
        require_image_capable_model(&state, &model).await?;
    }
    match store
        .accept_turn_with_message_context(
            body.turn_id,
            id,
            &model,
            &body.content,
            &images,
            &documents,
            &body.invoked_skills,
            body.voice_input_used,
        )
        .await?
    {
        AcceptTurnOutcome::Accepted(_) | AcceptTurnOutcome::Existing(_) => {
            state.turn_job_wake.notify_one();
            Ok(StatusCode::ACCEPTED)
        }
        AcceptTurnOutcome::IdentityConflict => {
            // Concurrent identical requests can resolve different mutable model
            // defaults before either commits. Retry against the winner's immutable
            // model so the wire identity remains `(chat, turn_id, content)`.
            let Some(existing) = store.get_turn_run(body.turn_id).await? else {
                return Err(ServerError::conflict(format!(
                    "turn {} was accepted with conflicting request data",
                    body.turn_id
                )));
            };
            if existing.chat_id == id
                && matches!(
                    store
                        .accept_turn_with_message_context(
                            body.turn_id,
                            id,
                            &existing.model,
                            &body.content,
                            &images,
                            &documents,
                            &body.invoked_skills,
                            body.voice_input_used,
                        )
                        .await?,
                    AcceptTurnOutcome::Existing(_)
                )
            {
                state.turn_job_wake.notify_one();
                Ok(StatusCode::ACCEPTED)
            } else {
                Err(ServerError::conflict(format!(
                    "turn {} was already accepted with different input",
                    body.turn_id
                )))
            }
        }
        AcceptTurnOutcome::ChatBusy(active) => {
            if body.queue {
                store
                    .enqueue_queued_turn(&tidebreak_core::QueuedTurn {
                        id: body.turn_id,
                        chat_id: id,
                        content: body.content.clone(),
                        attachments: body.attachments.clone(),
                        file_attachments: body.file_attachments.clone(),
                        invoked_skills: body.invoked_skills.clone(),
                        voice_input_used: body.voice_input_used,
                        // Assigned durably at insert; echoes back in the row.
                        position: 0,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    })
                    .await?;
                return Ok(StatusCode::ACCEPTED);
            }
            Err(ServerError::conflict(format!(
                "chat {id} already has active turn {}",
                active.id
            )))
        }
    }
}

/// Response of `GET /chats/{chat_id}/queued`.
#[derive(Debug, serde::Serialize)]
pub struct QueuedTurnsSnapshot {
    pub queued: Vec<tidebreak_core::QueuedTurn>,
    pub paused: bool,
}

fn queue_paused_setting(chat_id: ChatId) -> String {
    format!("chats.{chat_id}.queue_paused")
}

pub(crate) async fn read_queue_paused(
    store: &dyn tidebreak_core::Store,
    chat_id: ChatId,
) -> tidebreak_core::Result<bool> {
    Ok(store
        .get_setting(&queue_paused_setting(chat_id))
        .await?
        .and_then(|value| serde_json::from_value::<bool>(value).ok())
        .unwrap_or(false))
}

/// `GET /chats/{chat_id}/queued` — the chat's queued messages, FIFO.
pub async fn list_queued_turns(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<QueuedTurnsSnapshot>, ServerError> {
    store.require_chat(id).await?;
    Ok(Json(QueuedTurnsSnapshot {
        queued: state.store.list_queued_turns(id).await?,
        paused: read_queue_paused(&*state.store, id).await?,
    }))
}

/// Body of `PATCH /chats/{chat_id}/queued/{turn_id}`.
#[derive(Debug, Default, serde::Deserialize)]
pub struct QueuedTurnUpdate {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub position: Option<i32>,
}

/// `PATCH /chats/{chat_id}/queued/{turn_id}` — edit or reorder one queued
/// message.
pub async fn patch_queued_turn(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((id, turn_id)): Path<(ChatId, TurnId)>,
    Json(body): Json<QueuedTurnUpdate>,
) -> Result<Json<tidebreak_core::QueuedTurn>, ServerError> {
    store.require_chat(id).await?;
    match state
        .store
        .update_queued_turn(id, turn_id, body.content.as_deref(), body.position)
        .await?
    {
        Some(updated) => Ok(Json(updated)),
        None => Err(ServerError::not_found(format!(
            "queued turn {turn_id} not found"
        ))),
    }
}

/// `DELETE /chats/{chat_id}/queued/{turn_id}` — retract one queued message.
pub async fn delete_queued_turn(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((id, turn_id)): Path<(ChatId, TurnId)>,
) -> Result<StatusCode, ServerError> {
    store.require_chat(id).await?;
    if state.store.delete_queued_turn(id, turn_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ServerError::not_found(format!(
            "queued turn {turn_id} not found"
        )))
    }
}

/// Body of `PUT /chats/{chat_id}/queue-paused`.
#[derive(Debug, serde::Deserialize)]
pub struct QueuePausedBody {
    pub paused: bool,
}

/// `PUT /chats/{chat_id}/queue-paused` — stop or restart automatic promotion
/// for this chat; queued rows stay put while paused.
pub async fn put_queue_paused(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(body): Json<QueuePausedBody>,
) -> Result<StatusCode, ServerError> {
    store.require_chat(id).await?;
    state
        .store
        .set_setting(&queue_paused_setting(id), &serde_json::json!(body.paused))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /chats/{chat_id}/queued/send-now` — clear this chat's pause so the
/// promoter's next sweep starts the oldest queued message.
///
/// Promotion stays with the sweep: the idempotent try-and-delete in
/// [`promote_queued_turns`] owns the exact ordering guarantees, and the sweep
/// runs on a sub-second cadence, so this route only needs to release the
/// gate. A chat that was not paused is unaffected, making the call safe to
/// repeat.
pub async fn post_queue_send_now(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<StatusCode, ServerError> {
    store.require_chat(id).await?;
    state
        .store
        .set_setting(&queue_paused_setting(id), &serde_json::json!(false))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Body of `POST /chats/{id}/steer`.
#[derive(Debug, Deserialize)]
pub struct SteerBody {
    /// Stable client-generated identity for admission and ambiguous retries.
    pub steer_id: TurnSteerId,
    /// Exact durable turn to steer.
    pub turn_id: TurnId,
    /// User text to inject into the running turn.
    pub content: String,
    /// Skills the user named for this instruction alone. A steer neither
    /// inherits the turn's opening list nor spends its budget.
    #[serde(default)]
    pub invoked_skills: Vec<String>,
    /// When true, preempt the provider stream immediately; otherwise the message
    /// waits for the next step boundary.
    #[serde(default)]
    pub interrupt: bool,
    /// Whether the instruction was dictated and transcribed from speech.
    #[serde(default)]
    pub voice_input_used: bool,
}

/// `POST /chats/{id}/steer` — durably enqueue an instruction for an active turn.
///
/// `202 Accepted` only after the exact instruction commits. A local notification
/// can reduce delivery latency, but the claimed worker always applies pending
/// instructions from the database. Repeating the same identity and payload is
/// idempotent. `404` if the chat doesn't exist, `409` for conflicting identity or
/// unavailable turn, and `400` for malformed input.
pub async fn post_steer(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(body): Json<SteerBody>,
) -> Result<StatusCode, ServerError> {
    if body.steer_id.0.is_nil() {
        return Err(ServerError::bad_request("steer_id must not be nil"));
    }
    if body.content.trim().is_empty()
        || body.content.contains('\0')
        || body.content.chars().count() > TurnSteer::MAX_CONTENT_LEN
    {
        return Err(ServerError::bad_request(
            "steer content must be non-empty, contain no NUL characters, and fit the size limit",
        ));
    }
    require_invocable_skills(&state, &body.invoked_skills).await?;
    store.require_chat(id).await?;
    match store
        .accept_turn_steer_with_message_context(
            body.steer_id,
            body.turn_id,
            id,
            &body.content,
            &body.invoked_skills,
            body.interrupt,
            body.voice_input_used,
        )
        .await?
    {
        AcceptTurnSteerOutcome::Accepted(_) | AcceptTurnSteerOutcome::Existing(_) => {
            state
                .active_turns
                .signal_steer(id, body.turn_id, body.interrupt);
            state.turn_job_wake.notify_one();
            Ok(StatusCode::ACCEPTED)
        }
        AcceptTurnSteerOutcome::IdentityConflict => Err(ServerError::conflict(format!(
            "steer {} was already used by different request data",
            body.steer_id
        ))),
        AcceptTurnSteerOutcome::TurnUnavailable => Err(ServerError::conflict(format!(
            "turn {} is not accepting steering instructions",
            body.turn_id
        ))),
    }
}

/// `POST /chats/{id}/cancel` — durably cancel one exact turn for a chat.
///
/// Queued work becomes terminal atomically; running work enters `cancelling`
/// until its exact worker acknowledges quiescence. `202 Accepted` is idempotent
/// for cancelling/cancelled retries. `404` if the chat doesn't exist, `409` if
/// the turn does not belong to the chat or can no longer accept cancellation.
#[derive(Debug, Deserialize)]
pub struct CancelBody {
    /// Exact durable turn to cancel.
    pub turn_id: TurnId,
}

pub async fn post_cancel(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(body): Json<CancelBody>,
) -> Result<StatusCode, ServerError> {
    // Distinguish "unknown chat" (404) from "known chat, nothing running" (409).
    store.require_chat(id).await?;
    if !store
        .get_turn_run(body.turn_id)
        .await?
        .is_some_and(|turn| turn.chat_id == id)
    {
        return Err(ServerError::conflict(format!(
            "turn {} does not belong to chat {id}",
            body.turn_id
        )));
    }
    let resolution = loop {
        if let Some(resolution) = store
            .request_turn_cancellation_and_append_event(body.turn_id, Utc::now())
            .await?
        {
            break resolution;
        }
        // A heartbeat can advance `updated_at` after this request captures its
        // operational timestamp. Retry the same empty command with fresh time;
        // the store serializes it against the heartbeat and terminal decisions.
        if !store
            .get_turn_run(body.turn_id)
            .await?
            .is_some_and(|turn| turn.chat_id == id)
        {
            return Err(ServerError::conflict(format!(
                "turn {} is not cancellable",
                body.turn_id
            )));
        }
        tokio::task::yield_now().await;
    };
    if matches!(
        resolution.outcome,
        RequestTurnCancellationOutcome::AlreadyTerminal(_)
    ) {
        return Err(ServerError::conflict(format!(
            "turn {} already finished before cancellation",
            body.turn_id
        )));
    }
    if let Some(event) = resolution.terminal_event {
        let _ = state.events.sender(id).send(event);
    }
    state.active_turns.cancel(id, body.turn_id);
    // The turn transaction has already committed its child cancellation
    // cascade. Exact local handles now reduce provider shutdown latency without
    // taking part in the durable decision.
    signal_origin_sandbox_runs_after_commit(&state, id, body.turn_id).await;
    state.turn_job_wake.notify_one();
    // A parked-parent cancellation can fence a queued or running sandbox
    // child. Wake its worker promptly; durable claims remain the source of
    // truth if this notification is lost.
    state.agent_run_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// One promotion sweep: for every chat holding queued messages and no live
/// turn, try to accept the oldest row as a real turn and delete it on
/// success.
///
/// Promotion is deliberately try-based rather than fenced: acceptance is
/// idempotent under the queued row's own turn id, so `ChatBusy` leaves the
/// row for the next sweep, a crash between acceptance and deletion re-runs
/// into `Existing`, and a row whose id was somehow reused for different input
/// is dropped rather than retried forever. A row whose attachments or skills
/// no longer resolve is dropped too — its turn could never be accepted.
pub(crate) async fn promote_queued_turns(state: &AppState) -> Result<(), ServerError> {
    for chat_id in state.store.chats_with_queued_turns().await? {
        if read_queue_paused(&*state.store, chat_id).await? {
            continue;
        }
        let Some(chat) = state.store.get_chat(chat_id).await? else {
            continue;
        };
        let Some(next) = state
            .store
            .list_queued_turns(chat_id)
            .await?
            .into_iter()
            .next()
        else {
            continue;
        };
        let dropped = |reason: &str| {
            eprintln!(
                "tidebreak: dropping queued message {} for chat {chat_id}: {reason}",
                next.id
            );
        };
        // A crash after acceptance but before deleting the queued row must
        // replay against the accepted turn's immutable execution selector.
        // Re-resolving here could turn a catalog change into an identity
        // conflict and strand or drop input that already committed.
        let model = match state.store.get_turn_run(next.id).await? {
            Some(existing) if existing.chat_id == chat_id => existing.model,
            Some(_) => {
                dropped("its turn id was already accepted by another chat");
                state.store.delete_queued_turn(chat_id, next.id).await?;
                continue;
            }
            None => match resolve_executable_chat_model(state, &chat).await {
                Ok(model) => model,
                Err(_) => {
                    // The chat's model became unusable; leave the row so fixing
                    // the model releases the queue rather than losing messages.
                    continue;
                }
            },
        };
        if require_invocable_skills(state, &next.invoked_skills)
            .await
            .is_err()
        {
            dropped("an invoked skill is no longer available");
            state.store.delete_queued_turn(chat_id, next.id).await?;
            continue;
        }
        let images = match resolve_message_attachments(state, &next.attachments).await {
            Ok(images) => images,
            Err(_) => {
                dropped("an image attachment no longer resolves");
                state.store.delete_queued_turn(chat_id, next.id).await?;
                continue;
            }
        };
        match state
            .store
            .accept_turn_with_message_context(
                next.id,
                chat_id,
                &model,
                &next.content,
                &images,
                &next.file_attachments,
                &next.invoked_skills,
                next.voice_input_used,
            )
            .await?
        {
            AcceptTurnOutcome::Accepted(_) | AcceptTurnOutcome::Existing(_) => {
                state.store.delete_queued_turn(chat_id, next.id).await?;
                state.turn_job_wake.notify_one();
            }
            AcceptTurnOutcome::ChatBusy(_) => {}
            AcceptTurnOutcome::IdentityConflict => {
                dropped("its turn id was already used with different input");
                state.store.delete_queued_turn(chat_id, next.id).await?;
            }
        }
    }
    Ok(())
}
