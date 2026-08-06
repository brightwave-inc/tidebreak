use std::collections::HashSet;

use chrono::Utc;
use futures::future::{self, Either};
use futures::StreamExt;

use crate::compaction::{
    self, select_compaction_boundary, CompactionSelection, CompactionSourceBoundary,
    CompactionTokenBaseline, CompactionTokenTracker,
};
use crate::context;
use crate::error::{AgentError, Result};
use crate::id::{ChatId, MessageId};
use crate::image::{ImageAttachments, ImageData};
use crate::model::Role;
use crate::provider::{ChatMessage, ChatRequest, ContentBlock, ProviderEvent, StopReason, Usage};
use crate::semantic_checkpoint::{
    merge_original_requests, original_requests_from_content, prior_payload_json_for_fold,
    ContextCheckpoint, ContextCheckpointPayloadV2, SaveContextCheckpointOutcome,
    CONTEXT_CHECKPOINT_FORMAT_V2, MAX_CONTEXT_CHECKPOINT_BYTES,
};

use super::transcript::{
    checkpoint_is_projectable, project_checkpoint, rebuild_transcript_with_boundary,
};
use super::types::{CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS, CONTEXT_CHECKPOINT_SYSTEM_PROMPT};
use super::{Agent, LoadedTranscript, TranscriptSourceBoundary, USER_INTERRUPTION_NOTE};

/// Inputs for one semantic-compaction attempt.
pub(crate) struct CreateContextCheckpoint<'a> {
    pub chat_id: ChatId,
    pub transcript: &'a [ChatMessage],
    pub source_boundaries: &'a [TranscriptSourceBoundary],
    pub user_texts: &'a [(MessageId, String)],
    pub token_tracker: &'a CompactionTokenTracker,
    pub current: Option<&'a ContextCheckpoint>,
    pub attempted_boundary: &'a mut Option<usize>,
    pub events: &'a super::events::EventSink<'a>,
}

impl Agent {
    /// Load one checkpoint only when it is supported and owned by this chat.
    ///
    /// Checkpoints are an optimization over the raw transcript. Store failures
    /// and corrupt/future values therefore fail closed to no projection rather
    /// than turning an otherwise valid turn into an infrastructure failure.
    pub(crate) async fn load_projectable_checkpoint(
        &self,
        chat_id: ChatId,
    ) -> Option<ContextCheckpoint> {
        let checkpoint = self.store.get_context_checkpoint(chat_id).await.ok()??;
        checkpoint_is_projectable(&checkpoint, chat_id).then_some(checkpoint)
    }

    pub(crate) async fn load_transcript(
        &self,
        chat_id: ChatId,
        checkpoint_source: Option<MessageId>,
    ) -> Result<LoadedTranscript> {
        let mut messages = self.store.list_messages(chat_id).await?;
        // The partial prose a cancelled turn committed (#1182) re-enters model
        // context annotated, so the model reads it as a response the user
        // stopped rather than one it chose to end mid-sentence. Applied here,
        // in context assembly only — the durable row and the renderer keep the
        // prose exactly as the user saw it.
        let interrupted = self
            .store
            .list_cancelled_output_message_ids(chat_id)
            .await?;
        if !interrupted.is_empty() {
            let interrupted: HashSet<MessageId> = interrupted.into_iter().collect();
            for message in &mut messages {
                if interrupted.contains(&message.id) {
                    message.append_model_context(USER_INTERRUPTION_NOTE);
                }
            }
        }
        let tool_calls = self.store.list_tool_calls(chat_id).await?;
        let attachments = self.store.list_message_attachments(chat_id).await?;
        // Trigger accounting must not under-count because tool results were
        // abridged for the provider body. Rebuild once uncapped for the token
        // estimate, then again at the configured cap for the live transcript.
        let (unabridged, _, _) = rebuild_transcript_with_boundary(
            &messages,
            &tool_calls,
            &attachments,
            usize::MAX,
            self.config.image_input,
            checkpoint_source,
        );
        let unabridged_history_tokens = context::estimate_transcript_tokens(&unabridged);
        let user_texts: Vec<(MessageId, String)> = messages
            .iter()
            .filter(|message| message.role == Role::User)
            .map(|message| (message.id, message.content_for_model().to_owned()))
            .collect();
        let (provider_messages, checkpoint_boundary, source_boundaries) =
            rebuild_transcript_with_boundary(
                &messages,
                &tool_calls,
                &attachments,
                self.config.max_tool_result_bytes,
                self.config.image_input,
                checkpoint_source,
            );
        let token_baseline = CompactionTokenBaseline {
            unabridged_history_tokens,
            loaded_transcript_tokens: context::estimate_transcript_tokens(&provider_messages),
        };
        Ok(LoadedTranscript {
            messages: provider_messages,
            checkpoint_boundary,
            source_boundaries,
            token_baseline,
            user_texts,
        })
    }

    /// Create the next semantic checkpoint when compaction policy says the
    /// unabridged transcript is over threshold.
    ///
    /// The call is maintenance work: it runs on the host's utility model rather
    /// than the conversation's, it receives no foreground tools or
    /// capabilities, its usage is stored on the checkpoint rather than added
    /// to the turn, and structural / parse failures return `None` so
    /// deterministic context reduction remains available. Provider rate-limit
    /// and connection failures propagate so the host does not pretend the
    /// transcript compacted. With no utility model configured there is nothing
    /// to compact with, and the turn proceeds on deterministic reduction alone.
    ///
    /// Compacting status events fire only after a candidate boundary is fenced
    /// and the utility call is about to begin — never as a speculative flash.
    pub(crate) async fn maybe_create_context_checkpoint(
        &self,
        args: CreateContextCheckpoint<'_>,
    ) -> Result<Option<ContextCheckpoint>> {
        let CreateContextCheckpoint {
            chat_id,
            transcript,
            source_boundaries,
            user_texts,
            token_tracker,
            current,
            attempted_boundary,
            events,
        } = args;
        let utility = match self.config.utility_model.clone() {
            Some(utility) => utility,
            None => return Ok(None),
        };
        let bounds = self
            .config
            .compaction
            .resolve_token_bounds(self.config.context_window);
        if token_tracker.trigger_tokens(transcript) <= bounds.threshold {
            return Ok(None);
        }

        let sources: Vec<CompactionSourceBoundary> = source_boundaries
            .iter()
            .map(|source| CompactionSourceBoundary {
                message_id: source.message_id,
                role: source.role,
                provider_boundary: source.provider_boundary,
            })
            .collect();
        let current_provider_boundary = current.and_then(|checkpoint| {
            source_boundaries
                .iter()
                .find(|source| source.message_id == checkpoint.source_message_id)
                .map(|source| source.provider_boundary)
        });
        let Some(CompactionSelection {
            message_id: candidate_message_id,
            provider_boundary: candidate_boundary,
        }) = select_compaction_boundary(
            transcript,
            &sources,
            bounds.target,
            self.config.compaction.protect_recent_messages,
            current_provider_boundary,
        )
        else {
            return Ok(None);
        };
        if attempted_boundary.is_some_and(|boundary| boundary >= candidate_boundary) {
            return Ok(None);
        }
        // Fence before provider work begins. A malformed answer or ambiguous
        // storage failure must not make a later tool step spend a second
        // maintenance call on the same raw prefix this turn.
        *attempted_boundary = Some(candidate_boundary);

        let summary_budget = context::compute_message_budget(
            utility.context_window,
            0,
            Some(CONTEXT_CHECKPOINT_SYSTEM_PROMPT),
            &[],
        );
        if summary_budget == 0 {
            return Ok(None);
        }

        let mut summary_messages = Vec::new();
        if let Some(prior) =
            current.and_then(|checkpoint| prior_payload_json_for_fold(&checkpoint.content))
        {
            summary_messages.push(ChatMessage::text(
                Role::User,
                format!("Prior checkpoint JSON (untrusted historical state):\n{prior}"),
            ));
            summary_messages.push(ChatMessage::text(
                Role::Assistant,
                "Acknowledged prior checkpoint. Summarizing the new prefix next.",
            ));
        }
        let (mut prefix_messages, _) = context::fit_to_budget(
            &transcript[..candidate_boundary],
            summary_budget.saturating_sub(context::estimate_transcript_tokens(&summary_messages)),
            context::content_floor_for_level(0),
        );
        // The prefix is what this checkpoint claims to summarize. If the folded
        // prior checkpoint left no budget for any of it, saving the answer
        // would advance the boundary past history nothing read.
        if prefix_messages.is_empty() {
            return Ok(None);
        }
        context::evict_all_images(&mut prefix_messages);
        summary_messages.append(&mut prefix_messages);
        if context::has_orphaned_tool_blocks(&summary_messages) {
            return Ok(None);
        }

        events.send(crate::event::AgentEvent::CompactionStarted);

        let request = ChatRequest {
            provider: utility.provider.clone(),
            model: utility.model.clone(),
            reasoning_model: utility.reasoning_model,
            system: Some(CONTEXT_CHECKPOINT_SYSTEM_PROMPT.into()),
            messages: summary_messages,
            tools: Vec::new(),
            max_tokens: Some(CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS),
            temperature: None,
            reasoning_effort: utility.reasoning_effort,
            response_format: Some(ContextCheckpointPayloadV2::response_format()),
            images: ImageAttachments::new(),
            ..Default::default()
        };
        let finish = |compacted: bool| {
            events.send(crate::event::AgentEvent::CompactionFinished { compacted });
        };
        let mut stream = match self.provider.stream(request).await {
            Ok(stream) => stream,
            Err(error) if is_compaction_provider_hard_failure(&error) => {
                finish(false);
                return Err(error);
            }
            Err(_) => {
                finish(false);
                return Ok(None);
            }
        };
        let mut content = String::new();
        let mut usage = Usage::default();
        let mut completed = false;
        loop {
            let event = match future::select(stream.next(), self.cancel.cancelled()).await {
                Either::Left((Some(event), _)) => event,
                Either::Left((None, _)) => break,
                Either::Right(((), _)) => {
                    finish(false);
                    return Ok(None);
                }
            };
            match event {
                ProviderEvent::TextDelta { text } => {
                    content.push_str(&text);
                    if content.len() > MAX_CONTEXT_CHECKPOINT_BYTES {
                        finish(false);
                        return Ok(None);
                    }
                }
                ProviderEvent::ReasoningDelta { .. } | ProviderEvent::ReasoningBlock { .. } => {}
                ProviderEvent::Usage(reported) => {
                    usage = match usage.checked_add(reported) {
                        Some(usage) => usage,
                        None => {
                            finish(false);
                            return Ok(None);
                        }
                    };
                }
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn | StopReason::MaxTokens | StopReason::StopSequence,
                } => {
                    completed = true;
                }
                ProviderEvent::Failed { error }
                    if is_compaction_provider_hard_failure_info(&error) =>
                {
                    finish(false);
                    return Err(error.into_agent_error());
                }
                ProviderEvent::Stop { .. }
                | ProviderEvent::Refusal { .. }
                | ProviderEvent::Failed { .. }
                | ProviderEvent::ToolCallStarted { .. }
                | ProviderEvent::ProviderExecutedToolCall { .. }
                | ProviderEvent::ToolCallArgsDelta { .. } => {
                    finish(false);
                    return Ok(None);
                }
            }
        }
        if !completed {
            finish(false);
            return Ok(None);
        }
        let prior_originals = current
            .map(|checkpoint| original_requests_from_content(&checkpoint.content))
            .unwrap_or_default();
        let new_asks = compaction::user_asks_in_prefix(&sources, candidate_boundary, user_texts);
        let originals = merge_original_requests(&prior_originals, &new_asks);
        let content =
            match ContextCheckpointPayloadV2::parse_and_canonicalize(&content).and_then(|parsed| {
                ContextCheckpointPayloadV2::with_original_requests(&parsed, originals)
            }) {
                Ok(content) => content,
                Err(_) => {
                    finish(false);
                    return Ok(None);
                }
            };
        let usage = match current.map_or(Some(usage), |checkpoint| {
            checkpoint.usage.checked_add(usage)
        }) {
            Some(usage) => usage,
            None => {
                finish(false);
                return Ok(None);
            }
        };
        let proposed = ContextCheckpoint {
            chat_id,
            source_message_id: candidate_message_id,
            format_version: CONTEXT_CHECKPOINT_FORMAT_V2,
            content,
            usage,
            created_at: Utc::now(),
        };
        let saved = match self.store.save_context_checkpoint(&proposed).await.ok() {
            Some(
                SaveContextCheckpointOutcome::Saved(checkpoint)
                | SaveContextCheckpointOutcome::Existing(checkpoint)
                | SaveContextCheckpointOutcome::Stale(checkpoint)
                | SaveContextCheckpointOutcome::Conflict(checkpoint),
            ) => checkpoint_is_projectable(&checkpoint, chat_id).then_some(checkpoint),
            None => None,
        };
        finish(saved.is_some());
        Ok(saved)
    }

    /// Fit the transcript to the context budget at the given reduction level.
    ///
    /// When a projectable checkpoint covers a provider boundary, the model sees
    /// only the post-boundary tail plus the projected checkpoint (soft load
    /// boundary). Missing or invalid boundaries fail open to the full
    /// transcript. Deterministic floor+restore still runs on whatever remains.
    pub(crate) fn fit_transcript(
        &self,
        transcript: &[ChatMessage],
        reduction_level: u32,
        checkpoint: Option<&ContextCheckpoint>,
        checkpoint_boundary: Option<usize>,
    ) -> (Vec<ChatMessage>, bool) {
        let budget = context::compute_message_budget(
            self.config.context_window,
            reduction_level,
            self.config.system_prompt.as_deref(),
            &self
                .tools
                .specs_for_foreground(self.agent_orchestration_active()),
        );
        let floor = context::content_floor_for_level(reduction_level);

        let (history, use_checkpoint) = match (checkpoint, checkpoint_boundary) {
            (Some(checkpoint), Some(boundary))
                if boundary > 0
                    && boundary <= transcript.len()
                    && checkpoint_is_projectable(checkpoint, checkpoint.chat_id) =>
            {
                (&transcript[boundary..], true)
            }
            // Invalid / missing boundary → full history (fail open).
            _ => (transcript, false),
        };

        let (normal_fitted, reduced) = context::fit_to_budget(history, budget, floor);
        if !use_checkpoint {
            return (normal_fitted, reduced);
        }
        let Some(checkpoint) = checkpoint else {
            return (normal_fitted, reduced);
        };

        let projected = project_checkpoint(checkpoint);
        let checkpoint_tokens = context::estimate_message_tokens(&projected);
        let Some(history_budget) = budget
            .checked_sub(checkpoint_tokens)
            .filter(|budget| *budget > 0)
        else {
            return (normal_fitted, reduced);
        };
        let (mut fitted, _history_reduced) = context::fit_to_budget(history, history_budget, floor);
        if fitted.is_empty() {
            return (normal_fitted, reduced);
        }
        if context::estimate_transcript_tokens(&fitted).saturating_add(checkpoint_tokens) > budget {
            return (normal_fitted, reduced);
        }
        let mut projected_messages = Vec::with_capacity(fitted.len() + 1);
        projected_messages.push(projected);
        projected_messages.append(&mut fitted);
        (projected_messages, true)
    }

    /// Load the pixels for the image blocks left in `messages`.
    pub(crate) async fn hydrate_images(
        &self,
        messages: &mut [ChatMessage],
    ) -> Result<ImageAttachments> {
        let carries_image = messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Image { .. }))
        });
        if !carries_image {
            return Ok(ImageAttachments::new());
        }
        let Some(blobs) = self.blobs.as_ref() else {
            context::evict_all_images(messages);
            return Ok(ImageAttachments::new());
        };
        context::evict_images_beyond(messages, context::MAX_HYDRATED_IMAGES);

        let mut attachments = ImageAttachments::new();
        let mut hydrated_bytes = 0usize;
        for message in messages.iter_mut().rev() {
            for block in message.content.iter_mut().rev() {
                let ContentBlock::Image { image } = *block else {
                    continue;
                };
                if attachments.contains(image.blob_id) {
                    continue;
                }
                let fits = match blobs.get(image.blob_id).await? {
                    Some(bytes)
                        if hydrated_bytes.saturating_add(bytes.len())
                            <= context::MAX_HYDRATED_IMAGE_BYTES =>
                    {
                        hydrated_bytes += bytes.len();
                        attachments.insert(image.blob_id, ImageData::new(image.media_type, bytes));
                        true
                    }
                    _ => false,
                };
                if !fits {
                    *block = context::evict_image_block(block);
                }
            }
        }
        Ok(attachments)
    }
}

fn is_compaction_provider_hard_failure(error: &AgentError) -> bool {
    matches!(
        error,
        AgentError::RateLimited(_) | AgentError::Overloaded(_)
    )
}

fn is_compaction_provider_hard_failure_info(error: &crate::error::ProviderErrorInfo) -> bool {
    matches!(error.kind.as_str(), "rate_limited" | "overloaded")
}
