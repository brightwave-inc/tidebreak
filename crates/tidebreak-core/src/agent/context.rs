use std::collections::HashSet;

use chrono::Utc;
use futures::future::{self, Either};
use futures::StreamExt;

use crate::compaction::{
    self, select_compaction_boundary, CompactionSelection, CompactionSourceBoundary,
};
use crate::context;
use crate::error::{AgentError, Result};
use crate::id::{ChatId, MessageId};
use crate::image::{ImageAttachments, ImageData};
use crate::model::{Chat, Role};
use crate::provider::{ChatMessage, ChatRequest, ContentBlock, ProviderEvent, StopReason, Usage};
use crate::semantic_checkpoint::{
    merge_original_requests, original_requests_from_content, ContextCheckpoint,
    ContextCheckpointPayloadV2, SaveContextCheckpointOutcome, CONTEXT_CHECKPOINT_FORMAT_V2,
    MAX_CONTEXT_CHECKPOINT_BYTES,
};
use crate::tool::ToolSpec;
use crate::PermissionMode;

use super::transcript::{
    checkpoint_is_projectable, project_checkpoint, rebuild_transcript_with_boundary,
};
use super::types::{CONTEXT_CHECKPOINT_INSTRUCTION, CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS};
use super::{Agent, LoadedTranscript, TranscriptSourceBoundary, USER_INTERRUPTION_NOTE};

/// Everything before the last message of the request a step is about to send.
///
/// Compaction appends one message to exactly this and changes nothing else, so
/// the provider's prompt cache serves the whole prefix. Every field here
/// therefore has to be the foreground step's own value, not a maintenance
/// variant of it: tools render first on the wire and their definitions gate the
/// entire cache, and `system` gates system+messages.
pub(crate) struct RequestPrefix {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub images: ImageAttachments,
    /// Whether deterministic reduction shortened the history in `messages`.
    pub reduced: bool,
}

/// Inputs for one semantic-compaction attempt.
pub(crate) struct CreateContextCheckpoint<'a> {
    pub chat_id: ChatId,
    pub transcript: &'a [ChatMessage],
    pub source_boundaries: &'a [TranscriptSourceBoundary],
    pub user_texts: &'a [(MessageId, String)],
    pub current: Option<&'a ContextCheckpoint>,
    pub attempted_boundary: &'a mut Option<usize>,
    pub events: &'a super::events::EventSink<'a>,
    /// The request the foreground step was about to send. The checkpoint call
    /// is this plus one trailing instruction message.
    pub prefix: &'a RequestPrefix,
    /// Compact whatever the boundary rules allow, without waiting for the
    /// transcript to cross the policy threshold. Set by a compaction the user
    /// asked for: they can see the meter, and asking is the trigger.
    pub ignore_threshold: bool,
    /// What the person asking wants the summary to hold on to, if they said.
    /// Everything else about the checkpoint — its schema, its boundary, its
    /// budget — is unchanged; this only steers what the summary spends its
    /// room on.
    pub focus: Option<&'a str>,
}

impl Agent {
    /// Compact this chat now, because someone asked for it.
    ///
    /// The threshold is what makes automatic compaction infrequent; a person
    /// who has looked at the meter and asked for it has supplied their own
    /// trigger, so this runs the same pass without it. Everything else is
    /// unchanged, including the reasons it may decline: too little history to
    /// give up, or a protected tail that already fills the target. Both return
    /// `Ok(None)` — nothing was compacted, and nothing is wrong.
    ///
    /// `focus` is what the caller asked the summary to keep. It steers the
    /// summarizer and nothing else: the checkpoint's schema, boundary, and
    /// budget do not depend on it.
    ///
    /// This runs between turns, so the prefix it assembles is the one the
    /// chat's *next* step would send. Whether that hits the cache depends on
    /// how long ago the last turn ended — see the decision record.
    ///
    /// Events go to `events` exactly as they do inside a turn, so a caller that
    /// journals them gets the same `CompactionStarted`/`CompactionFinished`
    /// pair the renderer already understands.
    pub async fn compact_now(
        &self,
        chat: &Chat,
        focus: Option<&str>,
        events: &futures::channel::mpsc::UnboundedSender<crate::event::AgentEvent>,
    ) -> Result<Option<ContextCheckpoint>> {
        let current = self.load_projectable_checkpoint(chat.id).await;
        let loaded = self
            .load_transcript(
                chat.id,
                current
                    .as_ref()
                    .map(|checkpoint| checkpoint.source_message_id),
            )
            .await?;
        let prefix = self
            .build_request_prefix(
                chat,
                &loaded.messages,
                0,
                current.as_ref(),
                loaded.checkpoint_boundary,
            )
            .await?;
        let sink = super::events::EventSink::Legacy(events);
        self.maybe_create_context_checkpoint(CreateContextCheckpoint {
            chat_id: chat.id,
            transcript: &loaded.messages,
            source_boundaries: &loaded.source_boundaries,
            user_texts: &loaded.user_texts,
            current: current.as_ref(),
            attempted_boundary: &mut None,
            events: &sink,
            prefix: &prefix,
            ignore_threshold: true,
            focus,
        })
        .await
    }

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
        Ok(LoadedTranscript {
            messages: provider_messages,
            checkpoint_boundary,
            source_boundaries,
            user_texts,
        })
    }

    /// What this transcript costs the model right now: the projected
    /// checkpoint plus the history after its boundary, or the whole transcript
    /// when nothing is projectable.
    ///
    /// Every compaction number is measured here, on the messages that actually
    /// go on the wire. Counting the covered prefix a checkpoint already stands
    /// in for would leave the trigger permanently hot after the first
    /// compaction — the opposite of the infrequent, hard cadence the policy
    /// describes — and counting tool results at a size no request carries
    /// would put the trigger and [`select_compaction_boundary`]'s target on
    /// different scales.
    pub(crate) fn model_view_tokens(
        &self,
        transcript: &[ChatMessage],
        checkpoint: Option<&ContextCheckpoint>,
        boundary: Option<usize>,
    ) -> usize {
        match (checkpoint, boundary) {
            (Some(checkpoint), Some(boundary))
                if boundary > 0
                    && boundary <= transcript.len()
                    && checkpoint_is_projectable(checkpoint, checkpoint.chat_id) =>
            {
                context::estimate_transcript_tokens(&transcript[boundary..]).saturating_add(
                    context::estimate_message_tokens(&project_checkpoint(checkpoint)),
                )
            }
            _ => context::estimate_transcript_tokens(transcript),
        }
    }

    /// Create the next semantic checkpoint when compaction policy says the
    /// model's view of this chat is over threshold.
    ///
    /// The call is the step's own request with one instruction message appended
    /// — same model, same route, same system prompt, same tools, same fitted
    /// history — so the provider serves the whole prefix from the cache the
    /// previous step wrote instead of billing a second full copy of the
    /// transcript. Nothing may be added before that trailing message, which is
    /// why the call sends no `response_format` and no `tool_choice`: on the
    /// Messages API both are expressed by editing the tool array, and either
    /// would discard the cache this design exists to reuse.
    ///
    /// It is still maintenance in every other respect: its usage is stored on
    /// the checkpoint rather than added to the turn, and structural / parse
    /// failures — including a model that ignores the instruction and calls a
    /// tool — return `None` so deterministic context reduction remains
    /// available. Provider rate-limit and connection failures propagate so the
    /// host does not pretend the transcript compacted.
    ///
    /// Compacting status events fire only after a candidate boundary is fenced
    /// and the call is about to begin — never as a speculative flash.
    pub(crate) async fn maybe_create_context_checkpoint(
        &self,
        args: CreateContextCheckpoint<'_>,
    ) -> Result<Option<ContextCheckpoint>> {
        let CreateContextCheckpoint {
            chat_id,
            transcript,
            source_boundaries,
            user_texts,
            current,
            attempted_boundary,
            events,
            prefix,
            ignore_threshold,
            focus,
        } = args;
        // A request with no history to summarize cannot produce a checkpoint,
        // and appending the instruction to nothing would ask the model to
        // summarize its own instruction.
        if prefix.messages.is_empty() {
            return Ok(None);
        }
        let bounds = self
            .config
            .compaction
            .resolve_token_bounds(self.config.context_window);

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
        if !ignore_threshold
            && self.model_view_tokens(transcript, current, current_provider_boundary)
                <= bounds.threshold
        {
            return Ok(None);
        }
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

        events.send(crate::event::AgentEvent::CompactionStarted);

        let mut messages = prefix.messages.clone();
        messages.push(ChatMessage::text(
            Role::User,
            checkpoint_instruction(focus).into_owned(),
        ));
        let request = ChatRequest {
            provider: self.config.provider.clone(),
            conversation: Some(chat_id),
            model: self.config.model.clone(),
            reasoning_model: self.config.reasoning_model,
            system: self.config.system_prompt.clone(),
            messages,
            tools: prefix.tools.clone(),
            // Not part of the cached prefix: the cap applies to what the model
            // writes, so it can be the checkpoint's own without costing a hit.
            // Clamped to the chat's own cap because a model that declares a
            // lower output ceiling rejects the request outright, and the
            // rejection is swallowed by fail-open — compaction would then never
            // run on that chat with nothing to see. An absent chat cap keeps
            // the full constant: the host's model policy always sets one for a
            // registry-resolved model, so `None` reaches here only from
            // embedders, and shrinking it would reopen the thinking-truncation
            // problem the constant is sized against.
            max_tokens: Some(
                self.config
                    .max_tokens
                    .map_or(CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS, |cap| {
                        cap.min(CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS)
                    }),
            ),
            temperature: self.config.temperature,
            reasoning_effort: self.config.reasoning_effort,
            // The checkpoint rides the conversation cache (decision 0019), so
            // it also rides the conversation's retention: a differing TTL here
            // would not invalidate anything, but keeping them identical keeps
            // the whole prefix on one policy.
            prompt_cache_retention: self.config.prompt_cache_retention,
            // Compaction is maintenance inside the same Tidebreak turn, not a
            // second search budget. Provider limits are request-scoped, so
            // forwarding the turn's allowance here would let maintenance
            // spend it once and the foreground request spend it again.
            vendor_web_search: None,
            images: prefix.images.clone(),
            ..Default::default()
        };
        let request_max_tokens = request.max_tokens;
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
                    reason: StopReason::EndTurn | StopReason::StopSequence,
                } => {
                    completed = true;
                }
                // Truncation is folded into the same fail-open as any other
                // decline, but it is the one cause an operator can fix: a model
                // whose own output ceiling is below what a conforming payload
                // needs stops mid-JSON, parses as nothing, and would otherwise
                // look identical to a model that simply answered badly.
                ProviderEvent::Stop {
                    reason: StopReason::MaxTokens,
                } => {
                    tracing::warn!(
                        model = %self.config.model,
                        max_tokens = ?request_max_tokens,
                        "context checkpoint hit its output cap and was truncated; the payload cannot parse and this chat will not compact until the cap clears a conforming checkpoint"
                    );
                    finish(false);
                    return Ok(None);
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

    /// The tool schemas this turn advertises.
    ///
    /// Tools render first on the wire and their definitions gate the whole
    /// prompt cache, so every request a chat sends has to compute them the same
    /// way — the foreground step, the wrap-up step, and the compaction call all
    /// come through here.
    pub(crate) fn foreground_tool_specs(&self, chat: &Chat) -> Vec<ToolSpec> {
        if !self.config.tools_supported {
            return Vec::new();
        }
        let mut specs = self.tools.specs_for_surface(
            self.agent_orchestration_active(),
            matches!(chat.permission_mode, Some(PermissionMode::Plan)),
        );
        // The host tool and the provider's own search are one capability with
        // one name. Advertising both would offer the model two `web_search`
        // tools — a request most providers reject outright — so the registered
        // one is withheld for exactly the turns the host routed elsewhere or
        // turned off.
        if self.config.web_search != super::types::TurnWebSearch::Host {
            specs.retain(|spec| spec.name != crate::WEB_SEARCH_TOOL);
        }
        specs
    }

    /// Assemble everything a step's request carries except its trailing intent.
    ///
    /// Fitting, tool-result image eviction, and hydration happen in this order
    /// and nowhere else: hydration can evict an image that no longer fits the
    /// outbound bound, so the messages are only final once it has run.
    pub(crate) async fn build_request_prefix(
        &self,
        chat: &Chat,
        transcript: &[ChatMessage],
        reduction_level: u32,
        checkpoint: Option<&ContextCheckpoint>,
        checkpoint_boundary: Option<usize>,
    ) -> Result<RequestPrefix> {
        let (mut messages, reduced) =
            self.fit_transcript(transcript, reduction_level, checkpoint, checkpoint_boundary);
        context::evict_old_tool_result_images(
            &mut messages,
            context::TOOL_RESULT_IMAGE_MESSAGE_WINDOW,
        );
        let images = self.hydrate_images(&mut messages).await?;
        Ok(RequestPrefix {
            messages,
            tools: self.foreground_tool_specs(chat),
            images,
            reduced,
        })
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
            // A checkpoint that cannot share the normal request budget is not
            // safe to project. Retain deterministic reduction instead.
            return (normal_fitted, reduced);
        };
        let (mut fitted, history_reduced) = context::fit_to_budget(history, history_budget, floor);
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
        // Standing in a checkpoint for its own covered prefix is compaction,
        // which the divider and the compacting indicator already report. Only
        // trimming the post-boundary tail is the truncation this flag means,
        // or every step of every compacted chat would claim history was cut.
        (projected_messages, history_reduced)
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

/// The summarizer's instructions, plus what the caller asked it to keep.
///
/// The focus is appended rather than woven in, and it is labelled as the
/// user's request, so the standing instructions — the schema, the fields, the
/// prohibition on markdown — are the ones that survive a focus line that tries
/// to argue with them. Without a focus the message is byte-identical to the one
/// automatic compaction sends.
fn checkpoint_instruction(focus: Option<&str>) -> std::borrow::Cow<'static, str> {
    match focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        None => std::borrow::Cow::Borrowed(CONTEXT_CHECKPOINT_INSTRUCTION),
        Some(focus) => std::borrow::Cow::Owned(format!(
            "{CONTEXT_CHECKPOINT_INSTRUCTION}\n\nThe user asked for this checkpoint and said to \
             keep, above everything else: {focus}\nSpend the room you have on that, within the \
             same schema.",
        )),
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
