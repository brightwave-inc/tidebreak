use std::collections::HashSet;

use chrono::Utc;
use futures::future::{self, Either};
use futures::StreamExt;

use crate::context;
use crate::error::Result;
use crate::id::{ChatId, MessageId};
use crate::image::{ImageAttachments, ImageData};
use crate::model::Role;
use crate::provider::{ChatMessage, ChatRequest, ContentBlock, ProviderEvent, StopReason, Usage};
use crate::semantic_checkpoint::{
    ContextCheckpoint, ContextCheckpointPayloadV1, SaveContextCheckpointOutcome,
    CONTEXT_CHECKPOINT_FORMAT_V1, MAX_CONTEXT_CHECKPOINT_BYTES,
};

use super::transcript::{
    checkpoint_is_projectable, covered_prefix_was_reduced, project_checkpoint,
    rebuild_transcript_with_boundary,
};
use super::types::{CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS, CONTEXT_CHECKPOINT_SYSTEM_PROMPT};
use super::{Agent, LoadedTranscript, TranscriptSourceBoundary, USER_INTERRUPTION_NOTE};

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
        let (messages, checkpoint_boundary, source_boundaries) = rebuild_transcript_with_boundary(
            &messages,
            &tool_calls,
            &attachments,
            self.config.max_tool_result_bytes,
            self.config.image_input,
            checkpoint_source,
        );
        Ok(LoadedTranscript {
            messages,
            checkpoint_boundary,
            source_boundaries,
        })
    }

    /// Create the next semantic checkpoint immediately before a model-specific
    /// fit would discard its eligible raw prefix.
    ///
    /// The call is maintenance work: it runs on the host's utility model rather
    /// than the conversation's, it receives no foreground tools or
    /// capabilities, its usage is stored on the checkpoint rather than added
    /// to the turn, and every failure returns `None` so deterministic context
    /// reduction remains available. With no utility model configured there is
    /// nothing to compact with, and the turn proceeds on deterministic
    /// reduction alone rather than spending the user's conversation model here.
    pub(crate) async fn maybe_create_context_checkpoint(
        &self,
        chat_id: ChatId,
        transcript: &[ChatMessage],
        source_boundaries: &[TranscriptSourceBoundary],
        current: Option<&ContextCheckpoint>,
        reduction_level: u32,
        attempted_boundary: &mut Option<usize>,
    ) -> Option<ContextCheckpoint> {
        let utility = self.config.utility_model.clone()?;
        let foreground_budget = context::compute_message_budget(
            self.config.context_window,
            reduction_level,
            self.config.system_prompt.as_deref(),
            &self
                .tools
                .specs_for_foreground(self.agent_orchestration_active()),
        );
        let floor = context::content_floor_for_level(reduction_level);
        let (normal_fitted, reduced) = context::fit_to_budget(transcript, foreground_budget, floor);
        if !reduced {
            return None;
        }

        // Keep the newest complete user/assistant sequence in raw form. The
        // current user input follows the newest assistant, so the second-newest
        // durable assistant is the latest eligible inclusive boundary.
        let candidate = source_boundaries
            .iter()
            .rev()
            .filter(|source| source.role == Role::Assistant)
            .nth(1)?;
        if candidate.provider_boundary == 0
            || candidate.provider_boundary > transcript.len()
            || !covered_prefix_was_reduced(transcript, &normal_fitted, candidate.provider_boundary)
        {
            return None;
        }
        if current.is_some_and(|checkpoint| {
            source_boundaries
                .iter()
                .find(|source| source.message_id == checkpoint.source_message_id)
                .is_some_and(|source| source.provider_boundary >= candidate.provider_boundary)
        }) {
            return None;
        }
        if attempted_boundary.is_some_and(|boundary| boundary >= candidate.provider_boundary) {
            return None;
        }
        // Fence before provider work begins. A malformed answer or ambiguous
        // storage failure must not make a later tool step spend a second
        // maintenance call on the same raw prefix.
        *attempted_boundary = Some(candidate.provider_boundary);

        // Budgeted against the utility model's own window, which is typically
        // smaller than the conversation model's.
        let summary_budget = context::compute_message_budget(
            utility.context_window,
            0,
            Some(CONTEXT_CHECKPOINT_SYSTEM_PROMPT),
            &[],
        );
        if summary_budget == 0 {
            return None;
        }
        let (mut summary_messages, _) = context::fit_to_budget(
            &transcript[..candidate.provider_boundary],
            summary_budget,
            context::content_floor_for_level(0),
        );
        if summary_messages.is_empty() {
            return None;
        }
        // Source bytes are not part of semantic memory. The checkpoint call
        // sees stable image identities/metadata stand-ins only.
        context::evict_all_images(&mut summary_messages);
        if context::has_orphaned_tool_blocks(&summary_messages) {
            return None;
        }

        let request = ChatRequest {
            provider: utility.provider.clone(),
            model: utility.model.clone(),
            reasoning_model: utility.reasoning_model,
            system: Some(CONTEXT_CHECKPOINT_SYSTEM_PROMPT.into()),
            messages: summary_messages,
            tools: Vec::new(),
            max_tokens: Some(CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS),
            // Some reasoning models reject sampling controls entirely. The
            // strict schema/validator provides determinism without narrowing
            // the set of models that can create a checkpoint.
            temperature: None,
            reasoning_effort: utility.reasoning_effort,
            // Constrain the answer to the payload schema. Without this the
            // model's shape is a request, the parse below is a coin toss, and a
            // lost toss abandons this prefix for the rest of the conversation —
            // the boundary is fenced above before the call is made.
            response_format: Some(ContextCheckpointPayloadV1::response_format()),
            images: ImageAttachments::new(),
            ..Default::default()
        };
        let mut stream = self.provider.stream(request).await.ok()?;
        let mut content = String::new();
        let mut usage = Usage::default();
        let mut completed = false;
        loop {
            let event = match future::select(stream.next(), self.cancel.cancelled()).await {
                Either::Left((Some(event), _)) => event,
                Either::Left((None, _)) => break,
                Either::Right(((), _)) => return None,
            };
            match event {
                ProviderEvent::TextDelta { text } => {
                    content.push_str(&text);
                    if content.len() > MAX_CONTEXT_CHECKPOINT_BYTES {
                        return None;
                    }
                }
                ProviderEvent::ReasoningDelta { .. } | ProviderEvent::ReasoningBlock { .. } => {}
                ProviderEvent::Usage(reported) => {
                    usage = usage.checked_add(reported)?;
                }
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn | StopReason::MaxTokens | StopReason::StopSequence,
                } => {
                    completed = true;
                }
                ProviderEvent::Stop { .. }
                | ProviderEvent::Refusal { .. }
                | ProviderEvent::Failed { .. }
                | ProviderEvent::ToolCallStarted { .. }
                | ProviderEvent::ProviderExecutedToolCall { .. }
                | ProviderEvent::ToolCallArgsDelta { .. } => return None,
            }
        }
        if !completed {
            return None;
        }
        let content = ContextCheckpointPayloadV1::parse_and_canonicalize(&content).ok()?;
        let usage = current.map_or(Some(usage), |checkpoint| {
            checkpoint.usage.checked_add(usage)
        })?;
        let proposed = ContextCheckpoint {
            chat_id,
            source_message_id: candidate.message_id,
            format_version: CONTEXT_CHECKPOINT_FORMAT_V1,
            content,
            usage,
            created_at: Utc::now(),
        };
        match self.store.save_context_checkpoint(&proposed).await.ok()? {
            SaveContextCheckpointOutcome::Saved(checkpoint)
            | SaveContextCheckpointOutcome::Existing(checkpoint)
            | SaveContextCheckpointOutcome::Stale(checkpoint)
            | SaveContextCheckpointOutcome::Conflict(checkpoint) => {
                checkpoint_is_projectable(&checkpoint, chat_id).then_some(checkpoint)
            }
        }
    }

    /// Fit the transcript to the context budget at the given reduction level.
    /// Returns the fitted transcript and whether it was shortened.
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
        let (normal_fitted, reduced) = context::fit_to_budget(transcript, budget, floor);

        // Do not spend prompt budget on a summary while its covered raw
        // history still survives intact. The comparison is against the first
        // fit, before reserving checkpoint tokens, so a checkpoint never
        // causes the very reduction that justifies projecting it.
        let Some(checkpoint) = checkpoint else {
            return (normal_fitted, reduced);
        };
        let Some(boundary) = checkpoint_boundary else {
            return (normal_fitted, reduced);
        };
        if !reduced || !covered_prefix_was_reduced(transcript, &normal_fitted, boundary) {
            return (normal_fitted, reduced);
        }

        let projected = project_checkpoint(checkpoint);
        let checkpoint_tokens = context::estimate_message_tokens(&projected);
        let Some(history_budget) = budget
            .checked_sub(checkpoint_tokens)
            .filter(|budget| *budget > 0)
        else {
            // A checkpoint that cannot share the normal request budget is not
            // safe to project. Retain deterministic reduction instead.
            return (normal_fitted, reduced);
        };
        let (mut fitted, _) = context::fit_to_budget(transcript, history_budget, floor);
        if fitted.is_empty() {
            // The normal fitting algorithm guarantees a user anchor when one
            // can be retained. Do not let a large checkpoint displace all
            // recent request context merely to include stale history.
            return (normal_fitted, reduced);
        }
        if context::estimate_transcript_tokens(&fitted).saturating_add(checkpoint_tokens) > budget {
            // `fit_to_budget` may deliberately retain one oversized user
            // anchor rather than produce an invalid empty request. In that
            // exceptional case the checkpoint cannot also fit, so leave the
            // established deterministic request untouched.
            return (normal_fitted, reduced);
        }
        let mut projected_messages = Vec::with_capacity(fitted.len() + 1);
        projected_messages.push(projected);
        projected_messages.append(&mut fitted);
        (projected_messages, true)
    }

    /// Load the pixels for the image blocks left in `messages`.
    ///
    /// Blocks and bytes are deliberately separate: the transcript carries
    /// identity, and this is the one place bytes join a request. Two bounds
    /// apply, both newest-first, because a long conversation would otherwise
    /// re-upload every image it has ever accumulated on every turn: at most
    /// [`context::MAX_HYDRATED_IMAGES`] attachments and at most
    /// [`context::MAX_HYDRATED_IMAGE_BYTES`] of pixels.
    ///
    /// Anything not hydrated — over a bound, or whose bytes are simply gone —
    /// is rewritten as a text stand-in in `messages` before the request is
    /// built. That keeps the invariant adapters rely on: a surviving
    /// [`ContentBlock::Image`] always has bytes, so an adapter that finds none
    /// is looking at a real fault rather than an intended drop.
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
                // `ImageRef` is `Copy`, so take it by value and release the
                // borrow before the block may be rewritten below.
                let ContentBlock::Image { image } = *block else {
                    continue;
                };
                // The same attachment can appear in several messages; its bytes
                // are uploaded once and counted once.
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
