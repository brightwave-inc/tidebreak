//! Context reduction — fit a transcript to a model's context window.
//!
//! When a conversation outgrows the model's input budget the agent can't send
//! the full history. This module trims the transcript deterministically:
//! messages are shrunk to floor-sized stubs, surplus budget is restored
//! newest-first, and orphaned tool-use/result pairs are stripped. No LLM
//! summarization — the reduction is fast, predictable, and favours recent
//! context.

use std::collections::HashSet;

use crate::model::Role;
use crate::provider::{ChatMessage, ContentBlock};

const TEXT_OVERHEAD: usize = 7;
const TOOL_USE_OVERHEAD: usize = 30;
const TOOL_RESULT_OVERHEAD: usize = 20;
const IMAGE_OVERHEAD: usize = 10;
const ROLE_OVERHEAD: usize = 4;
const MAX_SINGLE_BLOCK_TOKENS: usize = 25_000;
const TRUNCATION_SUFFIX: &str = "\n… [truncated]";

// ── Token estimation ───────────────────────────────────────────────

/// Estimate tokens for a string: `ceil(chars / 3)`.
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(3)
}

/// Estimate tokens for one content block (overhead + content).
pub fn estimate_block_tokens(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text } => TEXT_OVERHEAD + estimate_tokens(text),
        ContentBlock::ToolUse { id, name, input } => {
            TOOL_USE_OVERHEAD
                + estimate_tokens(id)
                + estimate_tokens(name)
                + estimate_tokens(&input.to_string())
        }
        ContentBlock::Image { image } => IMAGE_OVERHEAD + image.estimated_tokens(),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } => TOOL_RESULT_OVERHEAD + estimate_tokens(tool_use_id) + estimate_tokens(content),
        // Call and result both ride in one block, so it costs about what a
        // tool-use plus its result would.
        ContentBlock::ProviderExecutedToolCall {
            name,
            input,
            output,
            ..
        } => {
            TOOL_USE_OVERHEAD
                + TOOL_RESULT_OVERHEAD
                + estimate_tokens(name)
                + estimate_tokens(&input.to_string())
                + estimate_tokens(&output.to_string())
        }
    }
}

/// Whether this message can anchor the start of a request.
///
/// The Messages API requires a conversation to open with a `user` message that
/// carries real content, so a message holding only tool results cannot lead.
/// Text and images both qualify: an image-only turn ("what is in this
/// screenshot?" with the question in the image) is a legitimate opening, and
/// treating it as unanchored would silently discard the user's actual request.
fn is_user_anchor(msg: &ChatMessage) -> bool {
    msg.role == Role::User
        && msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. } | ContentBlock::Image { .. }))
}

/// Replace an image block with a short text stand-in.
///
/// Dropping an image silently would leave the model answering a question about
/// something it cannot see, with no way to tell that from a question about
/// nothing. The stand-in keeps the turn coherent and lets the model say it no
/// longer has the image instead of guessing.
///
/// This is also what makes the adapter contract unambiguous: every
/// [`ContentBlock::Image`] that survives reduction is expected to have
/// hydrated bytes, so an adapter finding none is a real error rather than an
/// intended eviction.
#[must_use]
pub fn evict_image_block(block: &ContentBlock) -> ContentBlock {
    match block {
        ContentBlock::Image { image } => ContentBlock::Text {
            text: format!(
                "[image omitted from context: {} {}×{}]",
                image.media_type, image.width, image.height
            ),
        },
        other => other.clone(),
    }
}

/// Replace every image block in `messages` with its text stand-in.
///
/// A coarse degradation lever for when a request must shed pixels wholesale —
/// for example after a provider refuses an oversized body.
pub fn evict_all_images(messages: &mut [ChatMessage]) {
    for message in messages.iter_mut() {
        for block in message.content.iter_mut() {
            if matches!(block, ContentBlock::Image { .. }) {
                *block = evict_image_block(block);
            }
        }
    }
}

/// Most image attachments hydrated into a single outbound request.
///
/// A long conversation accumulates image blocks without bound, and every one
/// that keeps its pixels is re-uploaded on every subsequent turn. Capping the
/// count is what stops the outbound body from growing with chat length. Eight
/// spans a normal back-and-forth about a handful of screenshots while leaving
/// the older ones as text stand-ins.
pub const MAX_HYDRATED_IMAGES: usize = 8;

/// Most image bytes hydrated into a single outbound request.
///
/// The count cap alone is not a byte bound — eight attachments at the per-image
/// ceiling would be 128 MB. Providers cap the whole request (Anthropic at 32 MB)
/// and image bytes are base64-encoded on the wire, inflating by 4/3, so 20 MiB
/// of pixels is roughly 27 MB encoded and stays under that with room for the
/// text of the transcript.
pub const MAX_HYDRATED_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Tool-result previews keep their pixels across this many recent provider
/// messages. Older results retain a readable placeholder but no outbound bytes.
pub const TOOL_RESULT_IMAGE_MESSAGE_WINDOW: usize = 10;

/// Evict preview images from tool-result messages outside the recent window.
pub fn evict_old_tool_result_images(messages: &mut [ChatMessage], keep_messages: usize) {
    let cutoff = messages.len().saturating_sub(keep_messages);
    for message in messages.iter_mut().take(cutoff) {
        let is_tool_result = message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }));
        if !is_tool_result {
            continue;
        }
        for block in &mut message.content {
            if matches!(block, ContentBlock::Image { .. }) {
                *block = ContentBlock::Text {
                    text: "[preview image omitted from older tool result]".into(),
                };
            }
        }
    }
}

/// Keep pixels on only the newest `keep_last_n` image blocks, evicting older
/// ones to text stand-ins.
///
/// Bounds outbound body growth over a long chat. Newest-first because recent
/// images are the ones a turn is usually about.
///
/// Evicting rewrites the bytes of an already-sent message, which invalidates a
/// provider prompt cache from that position onward. Callers should therefore
/// keep `keep_last_n` generous enough to span a typical back-and-forth about
/// one image and tighten it only when the context budget actually demands it.
pub fn evict_images_beyond(messages: &mut [ChatMessage], keep_last_n: usize) {
    let mut seen = 0usize;
    for message in messages.iter_mut().rev() {
        for block in message.content.iter_mut().rev() {
            if !matches!(block, ContentBlock::Image { .. }) {
                continue;
            }
            if seen < keep_last_n {
                seen += 1;
                continue;
            }
            *block = evict_image_block(block);
        }
    }
}

/// Whether a block can be shrunk to fit a budget.
///
/// Text and tool payloads compress by dropping characters. An image cannot:
/// the provider needs whole bytes, so the only choices are send it or drop the
/// message carrying it. Treating an image as compressible would let a message
/// claim a small floor and then serialize at full cost, which is how an
/// image-heavy transcript slips past the budget gate and overflows the context
/// window.
const fn is_compressible(block: &ContentBlock) -> bool {
    !matches!(block, ContentBlock::Image { .. })
}

/// Estimate tokens for a message's opaque reasoning side channel.
///
/// Reasoning accumulates as ordinary history on keep-all models, so it must
/// be counted wherever the content is. The blocks are provider JSON, so the
/// estimate runs over their serialized form.
fn estimate_reasoning_tokens(msg: &ChatMessage) -> usize {
    msg.reasoning
        .blocks()
        .iter()
        .map(|block| estimate_tokens(&block.to_string()))
        .sum()
}

/// Estimate tokens for a full message (role overhead + reasoning + blocks).
pub fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    ROLE_OVERHEAD
        + estimate_reasoning_tokens(msg)
        + msg.content.iter().map(estimate_block_tokens).sum::<usize>()
}

/// Estimate tokens for an entire transcript.
pub fn estimate_transcript_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

// ── Context reduction ──────────────────────────────────────────────

/// Fit a transcript into a token `budget`.
///
/// `content_floor` is the minimum content tokens per block after shrinking
/// (e.g. 500 at reduction level 0, 100 at level 3).
///
/// Algorithm (floor + newest-first restoration):
/// 1. Compute each message's full and floor token estimates.
/// 2. If the sum of floors exceeds the budget, drop oldest messages
///    (newest-first selection, keeping tool-use/result pairs together).
/// 3. Distribute remaining budget newest-first — each message reclaims up to
///    its original size, capped at [`MAX_SINGLE_BLOCK_TOKENS`].
/// 4. Truncate content blocks that exceed their allocation.
/// 5. Strip orphaned tool-use/result blocks and merge adjacent same-role
///    messages.
///
/// Returns `(fitted_messages, was_reduced)`.
pub fn fit_to_budget(
    messages: &[ChatMessage],
    budget: usize,
    content_floor: usize,
) -> (Vec<ChatMessage>, bool) {
    if messages.is_empty() || estimate_transcript_tokens(messages) <= budget {
        // Fitting is not the only thing a transcript needs. A turn that died
        // with tool calls still pending leaves a ToolUse with no ToolResult,
        // which providers reject outright — and because that transcript is
        // usually well under budget, the reduction path below never runs and
        // never repairs it. Repair here too, so one interrupted turn cannot
        // wedge every later turn in the chat. This is a structural fix rather
        // than a reduction, so it does not report `was_reduced`.
        let mut fitted = messages.to_vec();
        if has_orphaned_tool_blocks(&fitted) {
            strip_orphaned_tool_blocks(&mut fitted);
            merge_adjacent_roles(&mut fitted);
            ensure_starts_with_user(&mut fitted);
        }
        return (fitted, false);
    }

    let mut entries: Vec<Entry> = messages
        .iter()
        .map(|msg| {
            let full = estimate_message_tokens(msg);
            let floor = message_floor(msg, content_floor);
            Entry {
                full,
                floor,
                allocated: floor,
                kept: true,
            }
        })
        .collect();

    // Phase 1: if floors exceed budget, select newest messages that fit.
    let floor_total: usize = entries.iter().map(|e| e.floor).sum();
    if floor_total > budget {
        select_newest_to_fit(&mut entries, budget, messages);
    }

    // Phase 2: distribute surplus newest-first.
    let current: usize = entries.iter().filter(|e| e.kept).map(|e| e.allocated).sum();
    let mut surplus = budget.saturating_sub(current);
    for entry in entries.iter_mut().rev() {
        if !entry.kept || surplus == 0 {
            continue;
        }
        let headroom = entry
            .full
            .saturating_sub(entry.allocated)
            .min(MAX_SINGLE_BLOCK_TOKENS);
        let reclaim = headroom.min(surplus);
        entry.allocated += reclaim;
        surplus -= reclaim;
    }

    // Phase 3: build output with truncation.
    let mut result: Vec<ChatMessage> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if !entry.kept {
            continue;
        }
        if entry.allocated >= entry.full {
            result.push(messages[i].clone());
        } else {
            result.push(truncate_message(&messages[i], entry.allocated));
        }
    }

    // Phase 4: clean up.
    strip_orphaned_tool_blocks(&mut result);
    merge_adjacent_roles(&mut result);
    ensure_starts_with_user(&mut result);

    (result, true)
}

struct Entry {
    full: usize,
    floor: usize,
    allocated: usize,
    kept: bool,
}

/// Minimum viable token size for a message: role overhead + reasoning in full
/// + each block at the floor.
///
/// Reasoning is charged like an image: replay validity is all-or-nothing per
/// message, so a floor that promised to shrink it would be one the serializer
/// cannot keep.
fn message_floor(msg: &ChatMessage, content_floor: usize) -> usize {
    ROLE_OVERHEAD
        + estimate_reasoning_tokens(msg)
        + msg
            .content
            .iter()
            .map(|block| {
                if !is_compressible(block) {
                    // Charged in full: shrinking is not an option for this
                    // block, so a smaller floor would be a promise the
                    // serializer cannot keep.
                    return estimate_block_tokens(block);
                }
                let overhead = block_overhead(block);
                let content = estimate_block_tokens(block).saturating_sub(overhead);
                overhead + content.min(content_floor)
            })
            .sum::<usize>()
}

fn block_overhead(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { .. } => TEXT_OVERHEAD,
        ContentBlock::Image { .. } => IMAGE_OVERHEAD,
        ContentBlock::ToolUse { .. } => TOOL_USE_OVERHEAD,
        ContentBlock::ToolResult { .. } => TOOL_RESULT_OVERHEAD,
        ContentBlock::ProviderExecutedToolCall { .. } => TOOL_USE_OVERHEAD + TOOL_RESULT_OVERHEAD,
    }
}

/// Walk backwards from newest, keeping messages whose floors fit. Marks
/// messages as `kept = false` when they don't fit. Tool-use/result pairs are
/// kept or dropped together.
fn select_newest_to_fit(entries: &mut [Entry], budget: usize, messages: &[ChatMessage]) {
    // Build pairing maps: tool_use id → message index, tool_result id → index.
    let mut use_to_msg: Vec<(String, usize)> = Vec::new();
    let mut result_to_msg: Vec<(String, usize)> = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => use_to_msg.push((id.clone(), i)),
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    result_to_msg.push((tool_use_id.clone(), i));
                }
                _ => {}
            }
        }
    }

    // Mark all as dropped, then select from newest.
    for entry in entries.iter_mut() {
        entry.kept = false;
    }

    let mut remaining = budget;
    // Walk backwards, greedily selecting.
    for i in (0..entries.len()).rev() {
        if entries[i].kept {
            continue; // already selected as part of a tool pair
        }
        let cost = pair_cost(i, entries, messages, &use_to_msg, &result_to_msg);
        if cost <= remaining {
            remaining -= mark_pair_kept(i, entries, messages, &use_to_msg, &result_to_msg);
        }
    }

    // Guarantee at least one user text message (the Messages API requires
    // conversations to start with `user`). If none was selected, force-keep
    // the most recent one — slightly exceeding the budget is better than an
    // empty or invalid transcript.
    let has_user_anchor = entries
        .iter()
        .enumerate()
        .any(|(i, e)| e.kept && is_user_anchor(&messages[i]));
    if !has_user_anchor {
        for i in (0..entries.len()).rev() {
            if is_user_anchor(&messages[i]) {
                mark_pair_kept(i, entries, messages, &use_to_msg, &result_to_msg);
                break;
            }
        }
    }
}

/// Cost of keeping message `i` and its paired tool messages (if any).
fn pair_cost(
    i: usize,
    entries: &[Entry],
    messages: &[ChatMessage],
    use_to_msg: &[(String, usize)],
    result_to_msg: &[(String, usize)],
) -> usize {
    let mut cost = entries[i].floor;
    for block in &messages[i].content {
        match block {
            ContentBlock::ToolUse { id, .. } => {
                if let Some(&(_, ri)) = result_to_msg.iter().find(|(tid, _)| tid == id) {
                    if !entries[ri].kept {
                        cost += entries[ri].floor;
                    }
                }
            }
            ContentBlock::ToolResult { tool_use_id, .. } => {
                if let Some(&(_, ui)) = use_to_msg.iter().find(|(tid, _)| tid == tool_use_id) {
                    if !entries[ui].kept {
                        cost += entries[ui].floor;
                    }
                }
            }
            _ => {}
        }
    }
    cost
}

/// Mark message `i` and its tool pair as kept. Returns total floor cost.
fn mark_pair_kept(
    i: usize,
    entries: &mut [Entry],
    messages: &[ChatMessage],
    use_to_msg: &[(String, usize)],
    result_to_msg: &[(String, usize)],
) -> usize {
    entries[i].kept = true;
    let mut cost = entries[i].floor;
    for block in &messages[i].content {
        match block {
            ContentBlock::ToolUse { id, .. } => {
                if let Some(&(_, ri)) = result_to_msg.iter().find(|(tid, _)| tid == id) {
                    if !entries[ri].kept {
                        entries[ri].kept = true;
                        cost += entries[ri].floor;
                    }
                }
            }
            ContentBlock::ToolResult { tool_use_id, .. } => {
                if let Some(&(_, ui)) = use_to_msg.iter().find(|(tid, _)| tid == tool_use_id) {
                    if !entries[ui].kept {
                        entries[ui].kept = true;
                        cost += entries[ui].floor;
                    }
                }
            }
            _ => {}
        }
    }
    cost
}

// ── Truncation ─────────────────────────────────────────────────────

fn truncate_message(msg: &ChatMessage, target_tokens: usize) -> ChatMessage {
    // Reasoning is never truncated or partially dropped — a shrunken thinking
    // block would fail the provider's replay validation, so the side channel
    // keeps its full cost and only the content blocks split what remains.
    let available = target_tokens
        .saturating_sub(ROLE_OVERHEAD)
        .saturating_sub(estimate_reasoning_tokens(msg));
    if msg.content.is_empty() {
        return msg.clone();
    }

    // Every block costs at least its fixed overhead (it can't shrink below the
    // JSON envelope + ids), so reserve overheads first, then split whatever
    // remains across blocks in proportion to their content size. Callers keep
    // `target_tokens >= message_floor`, so `available >= overhead_total` and the
    // shares sum to at most `available` — the message never exceeds its budget.
    let overhead_total: usize = msg.content.iter().map(block_overhead).sum();
    let content_total: usize = msg
        .content
        .iter()
        .map(|b| estimate_block_tokens(b).saturating_sub(block_overhead(b)))
        .sum();
    let content_budget = available.saturating_sub(overhead_total);

    let blocks: Vec<ContentBlock> = msg
        .content
        .iter()
        .map(|block| {
            let overhead = block_overhead(block);
            let block_content = estimate_block_tokens(block).saturating_sub(overhead);
            let content_share = if content_total == 0 {
                0
            } else {
                (content_budget as u64 * block_content as u64 / content_total as u64) as usize
            };
            let share = overhead + content_share;
            if share >= estimate_block_tokens(block) {
                block.clone()
            } else {
                truncate_block(block, share)
            }
        })
        .collect();

    ChatMessage {
        role: msg.role,
        content: blocks,
        // The signed reasoning prefix is untouched by content truncation, so
        // it survives whole with the message.
        reasoning: msg.reasoning.clone(),
    }
}

fn truncate_block(block: &ContentBlock, target_tokens: usize) -> ContentBlock {
    match block {
        // Incompressible: the floor already charges the full cost, so this is
        // only ever reached with a budget the image already fits.
        ContentBlock::Image { .. } => block.clone(),
        ContentBlock::Text { text } => {
            let budget = target_tokens.saturating_sub(TEXT_OVERHEAD);
            ContentBlock::Text {
                text: truncate_str(text, budget),
            }
        }
        ContentBlock::ToolUse { id, name, input } => {
            let fixed = TOOL_USE_OVERHEAD + estimate_tokens(id) + estimate_tokens(name);
            let budget = target_tokens.saturating_sub(fixed);
            let input_str = input.to_string();
            let truncated = truncate_str(&input_str, budget);
            ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: serde_json::json!({ "truncated_args": truncated }),
            }
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let fixed = TOOL_RESULT_OVERHEAD + estimate_tokens(tool_use_id);
            let budget = target_tokens.saturating_sub(fixed);
            ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: truncate_str(content, budget),
                is_error: *is_error,
            }
        }
        // The result is the compressible half — the call's own arguments are
        // small and identify what was run.
        ContentBlock::ProviderExecutedToolCall {
            name,
            input,
            output,
            is_error,
        } => {
            let fixed = TOOL_USE_OVERHEAD
                + TOOL_RESULT_OVERHEAD
                + estimate_tokens(name)
                + estimate_tokens(&input.to_string());
            let budget = target_tokens.saturating_sub(fixed);
            let output_str = output.to_string();
            let truncated = truncate_str(&output_str, budget);
            ContentBlock::ProviderExecutedToolCall {
                name: name.clone(),
                input: input.clone(),
                output: serde_json::json!({ "truncated_output": truncated }),
                is_error: *is_error,
            }
        }
    }
}

fn truncate_str(s: &str, target_tokens: usize) -> String {
    let char_budget = target_tokens * 3;
    if s.chars().count() <= char_budget {
        return s.to_string();
    }
    let suffix_chars = TRUNCATION_SUFFIX.chars().count();
    // When the budget can't even hold the notice, hard-cut to the budget — the
    // notice itself must never push the block back over `target_tokens`.
    if char_budget <= suffix_chars {
        return s.chars().take(char_budget).collect();
    }
    let keep = char_budget - suffix_chars;
    let truncated: String = s.chars().take(keep).collect();
    format!("{truncated}{TRUNCATION_SUFFIX}")
}

// ── Cleanup ────────────────────────────────────────────────────────

/// Report whether any ToolUse lacks a matching ToolResult, or vice versa.
#[must_use]
pub fn has_orphaned_tool_blocks(messages: &[ChatMessage]) -> bool {
    let mut use_ids: HashSet<&str> = HashSet::new();
    let mut result_ids: HashSet<&str> = HashSet::new();

    for msg in messages {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    use_ids.insert(id.as_str());
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    result_ids.insert(tool_use_id.as_str());
                }
                _ => {}
            }
        }
    }

    use_ids.symmetric_difference(&result_ids).next().is_some()
}

/// Remove ToolUse blocks with no matching ToolResult and vice versa.
pub fn strip_orphaned_tool_blocks(messages: &mut Vec<ChatMessage>) {
    let mut use_ids: HashSet<String> = HashSet::new();
    let mut result_ids: HashSet<String> = HashSet::new();

    for msg in messages.iter() {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    use_ids.insert(id.clone());
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    result_ids.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }

    for msg in messages.iter_mut() {
        msg.content.retain(|block| match block {
            ContentBlock::ToolUse { id, .. } => result_ids.contains(id),
            ContentBlock::ToolResult { tool_use_id, .. } => use_ids.contains(tool_use_id),
            _ => true,
        });
    }

    messages.retain(|msg| !msg.content.is_empty());
}

/// Merge adjacent messages with the same role.
fn merge_adjacent_roles(messages: &mut Vec<ChatMessage>) {
    let mut i = 0;
    while i + 1 < messages.len() {
        if messages[i].role == messages[i + 1].role {
            let next = messages.remove(i + 1);
            // The absorbed message's reasoning cannot ride along: appending
            // its content after the surviving message's blocks would move its
            // thinking prefix away from the front, and partial or reordered
            // replay is worse than none. The surviving message keeps its own
            // reasoning, which still prefixes its own content.
            messages[i].content.extend(next.content);
        } else {
            i += 1;
        }
    }
}

/// Drop leading messages until the first is a user message with non-tool-result
/// content. The Messages API requires the conversation to start with `user`.
fn ensure_starts_with_user(messages: &mut Vec<ChatMessage>) {
    while let Some(first) = messages.first() {
        if is_user_anchor(first) {
            break;
        }
        messages.remove(0);
    }
}

// ── Budget helpers (used by the agent loop) ────────────────────────

/// The maximum reduction level (0 = normal, 3 = nuclear).
pub const MAX_REDUCTION_LEVEL: u32 = 3;

/// Compute the message-array token budget for a given reduction level.
///
/// Subtracts estimated system-prompt and tool-spec tokens from a fraction of
/// the context window. The fraction shrinks with each level (75% → 63% → 50%
/// → 38%).
pub fn compute_message_budget(
    context_window: usize,
    reduction_level: u32,
    system_prompt: Option<&str>,
    tool_specs: &[crate::tool::ToolSpec],
) -> usize {
    let fraction = match reduction_level {
        0 => 75,
        1 => 63,
        2 => 50,
        _ => 38,
    };
    let effective = context_window * fraction / 100;
    let system_tokens = system_prompt.map(estimate_tokens).unwrap_or(0);
    let tools_tokens: usize = tool_specs
        .iter()
        .map(|t| {
            30 + estimate_tokens(&t.name)
                + estimate_tokens(&t.description)
                + estimate_tokens(&t.input_schema.to_string())
        })
        .sum();
    effective
        .saturating_sub(system_tokens)
        .saturating_sub(tools_tokens)
}

/// Content-floor tokens for a given reduction level.
pub fn content_floor_for_level(level: u32) -> usize {
    match level {
        0 => 500,
        1 => 300,
        2 => 200,
        _ => 100,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{MessageReasoning, ReasoningOrigin};
    use serde_json::json;

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage::text(Role::User, text)
    }

    fn assistant_msg(text: &str) -> ChatMessage {
        ChatMessage::text(Role::Assistant, text)
    }

    fn tool_use_msg(id: &str, name: &str, args: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input: json!({ "arg": args }),
            }],
            reasoning: MessageReasoning::default(),
        }
    }

    fn tool_result_msg(tool_use_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error: false,
            }],
            reasoning: MessageReasoning::default(),
        }
    }

    // ── fit_to_budget ──────────────────────────────────────────────

    #[test]
    fn no_reduction_when_within_budget() {
        let msgs = vec![user_msg("hello"), assistant_msg("hi")];
        let budget = estimate_transcript_tokens(&msgs) + 100;
        let (result, reduced) = fit_to_budget(&msgs, budget, 500);
        assert!(!reduced);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn reasoning_rides_its_message_whole_through_reduction() {
        // Reasoning accumulates as ordinary history on keep-all models, so it
        // is counted — and reduction either keeps the message with every
        // block intact or drops the message entirely. Shrinking or partially
        // dropping blocks would break the provider's replay validation.
        let reasoning = vec![
            json!({"type": "thinking", "thinking": "p".repeat(300), "signature": "sig"}),
            json!({"type": "redacted_thinking", "data": "blob"}),
        ];
        let origin = ReasoningOrigin {
            provider: None,
            model: "m".into(),
        };
        let mut thinking_step = assistant_msg(&"calling a tool ".repeat(50));
        thinking_step.reasoning = MessageReasoning::captured(origin, reasoning.clone());

        let mut silent_step = thinking_step.clone();
        silent_step.reasoning = MessageReasoning::default();
        assert!(
            estimate_message_tokens(&thinking_step) > estimate_message_tokens(&silent_step),
            "reasoning tokens must be counted"
        );

        // A budget that keeps the message but shrinks its text leaves every
        // reasoning block byte-identical.
        let messages = vec![user_msg("start"), thinking_step.clone()];
        let full = estimate_transcript_tokens(&messages);
        let floor_with_user =
            estimate_message_tokens(&messages[0]) + message_floor(&thinking_step, 100);
        let (fitted, reduced) = fit_to_budget(&messages, (full + floor_with_user) / 2, 100);
        assert!(reduced);
        let fitted_step = fitted
            .iter()
            .find(|message| message.role == Role::Assistant)
            .expect("the step fits under this budget");
        assert_eq!(
            fitted_step.reasoning.blocks(),
            reasoning,
            "truncation never touches the blocks"
        );
        assert!(
            estimate_message_tokens(fitted_step) < estimate_message_tokens(&thinking_step),
            "the text took the shrink"
        );

        // A budget that cannot pay the floor — reasoning is charged in full,
        // like an image — drops the whole message rather than some blocks.
        let drop_budget = estimate_message_tokens(&messages[0]) + 10;
        let (fitted, _) = fit_to_budget(&messages, drop_budget, 100);
        assert!(
            fitted.iter().all(|message| message.reasoning.is_empty()),
            "no partial reasoning survives: {fitted:?}"
        );
        assert!(
            fitted.iter().all(|message| message.role != Role::Assistant),
            "the step went with its reasoning: {fitted:?}"
        );
    }

    #[test]
    fn reduces_when_over_budget() {
        let big = "x".repeat(3000);
        let msgs = vec![
            user_msg("start"),
            assistant_msg(&big),
            user_msg("follow up"),
            assistant_msg("recent answer"),
        ];
        let full = estimate_transcript_tokens(&msgs);
        let budget = full / 2;
        let (result, reduced) = fit_to_budget(&msgs, budget, 100);
        assert!(reduced);
        assert!(estimate_transcript_tokens(&result) <= budget);
    }

    #[test]
    fn multi_block_message_never_exceeds_its_allocation() {
        // A message with many small blocks used to overshoot its target: each
        // block was force-allocated its overhead *after* the remaining-budget
        // cap, so the shares summed above the allocation. Truncating to a tight
        // target must produce a message at or under that target.
        let blocks: Vec<ContentBlock> = (0..10)
            .map(|i| ContentBlock::Text {
                text: format!("block {i} ").repeat(20),
            })
            .collect();
        let msg = ChatMessage {
            role: Role::User,
            content: blocks,
            reasoning: MessageReasoning::default(),
        };
        // Callers never allocate below the message's irreducible overhead floor
        // (role + each block's fixed envelope); test at and above it.
        let overhead_floor: usize =
            ROLE_OVERHEAD + msg.content.iter().map(block_overhead).sum::<usize>();
        for target in [overhead_floor, overhead_floor + 40, 200, 400] {
            let truncated = truncate_message(&msg, target);
            assert!(
                estimate_message_tokens(&truncated) <= target,
                "target {target}: got {}",
                estimate_message_tokens(&truncated)
            );
        }
    }

    #[test]
    fn transcript_with_many_block_messages_fits_budget() {
        // End-to-end: a transcript of multi-block messages must fit the budget
        // after reduction, so the proactive path doesn't fall through to the
        // provider's PromptTooLong retry.
        let make = |role: Role, n: usize| ChatMessage {
            role,
            content: (0..n)
                .map(|i| ContentBlock::Text {
                    text: format!("chunk {i} ").repeat(30),
                })
                .collect(),
            reasoning: MessageReasoning::default(),
        };
        let msgs = vec![
            user_msg("kick off the conversation here"),
            make(Role::Assistant, 8),
            make(Role::User, 6),
            make(Role::Assistant, 10),
        ];
        let budget = estimate_transcript_tokens(&msgs) / 3;
        let (result, reduced) = fit_to_budget(&msgs, budget, 50);
        assert!(reduced);
        assert!(
            estimate_transcript_tokens(&result) <= budget,
            "fitted {} > budget {budget}",
            estimate_transcript_tokens(&result)
        );
    }

    #[test]
    fn newest_messages_get_more_budget() {
        let big = "x".repeat(3000);
        let msgs = vec![
            user_msg("old"),
            assistant_msg(&big),
            user_msg("recent"),
            assistant_msg("answer"),
        ];
        let full = estimate_transcript_tokens(&msgs);
        // Budget enough for the newest messages at full + oldest shrunk.
        let budget = full * 2 / 3;
        let (result, reduced) = fit_to_budget(&msgs, budget, 50);
        assert!(reduced);
        // The recent answer should be intact (or close to it).
        let last = result.last().unwrap();
        if let ContentBlock::Text { text } = &last.content[0] {
            assert_eq!(text, "answer");
        }
    }

    #[test]
    fn tool_pairs_kept_together() {
        let msgs = vec![
            user_msg("go"),
            tool_use_msg("tu_1", "read_file", "a"),
            tool_result_msg("tu_1", &"data ".repeat(500)),
            user_msg("now what"),
            assistant_msg("done"),
        ];
        let budget = estimate_transcript_tokens(&msgs) / 3;
        let (result, reduced) = fit_to_budget(&msgs, budget, 50);
        assert!(reduced);
        // If tool_use is present, its result must be too (and vice versa).
        let has_use = result.iter().any(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { id, .. } if id == "tu_1"))
        });
        let has_result = result.iter().any(|m| {
            m.content.iter().any(
                |b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "tu_1"),
            )
        });
        assert_eq!(
            has_use, has_result,
            "tool pair must be kept or dropped together"
        );
    }

    #[test]
    fn result_starts_with_user_text() {
        let msgs = vec![
            user_msg("hi"),
            assistant_msg("hey"),
            user_msg("question"),
            assistant_msg("answer"),
        ];
        let budget = estimate_transcript_tokens(&msgs) / 2;
        let (result, _) = fit_to_budget(&msgs, budget, 50);
        assert!(!result.is_empty());
        let first = &result[0];
        assert_eq!(first.role, Role::User);
        assert!(first
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. })));
    }

    // ── strip_orphaned_tool_blocks ─────────────────────────────────

    /// A turn that died with a tool call unresolved leaves a ToolUse with no
    /// ToolResult. That transcript is usually well under budget, so the
    /// reduction path never runs — yet providers reject it outright, which
    /// wedged every later turn in the chat. Repair it even when it fits.
    #[test]
    fn repairs_orphaned_tool_use_even_when_the_transcript_already_fits() {
        let msgs = vec![
            user_msg("go"),
            tool_use_msg("tu_1", "read_file", "a"),
            // The turn died here: no matching ToolResult was ever committed.
            user_msg("next"),
        ];
        let (fitted, was_reduced) = fit_to_budget(&msgs, 100_000, 500);
        assert!(
            !fitted.iter().any(|m| m
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }))),
            "the orphaned tool use should be stripped: {fitted:?}"
        );
        // Structural repair is not a context reduction — reporting one here
        // would tell the client its context had been truncated.
        assert!(!was_reduced);
    }

    #[test]
    fn under_budget_transcripts_are_otherwise_untouched() {
        let msgs = vec![
            user_msg("go"),
            tool_use_msg("tu_1", "read_file", "a"),
            tool_result_msg("tu_1", "data"),
            assistant_msg("done"),
        ];
        let (fitted, was_reduced) = fit_to_budget(&msgs, 100_000, 500);
        assert_eq!(fitted, msgs);
        assert!(!was_reduced);
    }

    #[test]
    fn strips_orphaned_tool_result() {
        let mut msgs = vec![
            user_msg("go"),
            // No matching ToolUse for tu_1.
            tool_result_msg("tu_1", "data"),
            assistant_msg("done"),
        ];
        strip_orphaned_tool_blocks(&mut msgs);
        assert!(!msgs.iter().any(|m| m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))));
    }

    // ── Budget helpers ─────────────────────────────────────────────

    #[test]
    fn budget_shrinks_with_level() {
        let b0 = compute_message_budget(200_000, 0, None, &[]);
        let b1 = compute_message_budget(200_000, 1, None, &[]);
        let b2 = compute_message_budget(200_000, 2, None, &[]);
        let b3 = compute_message_budget(200_000, 3, None, &[]);
        assert!(b0 > b1);
        assert!(b1 > b2);
        assert!(b2 > b3);
    }

    #[test]
    fn budget_subtracts_system_and_tools() {
        let bare = compute_message_budget(200_000, 0, None, &[]);
        let with_system = compute_message_budget(200_000, 0, Some("be brief and helpful"), &[]);
        assert!(with_system < bare);
    }

    #[test]
    fn content_floor_decreases_with_level() {
        assert!(content_floor_for_level(0) > content_floor_for_level(1));
        assert!(content_floor_for_level(1) > content_floor_for_level(2));
        assert!(content_floor_for_level(2) > content_floor_for_level(3));
    }

    // ── Truncation helpers ─────────────────────────────────────────

    #[test]
    fn truncate_str_appends_notice() {
        let s = "a".repeat(3000);
        let result = truncate_str(&s, 100);
        assert!(result.contains(TRUNCATION_SUFFIX));
        assert!(result.len() < s.len());
    }

    // ── merge / ensure_starts_with_user ────────────────────────────

    #[test]
    fn merge_adjacent_same_role() {
        let mut msgs = vec![user_msg("a"), user_msg("b"), assistant_msg("c")];
        merge_adjacent_roles(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].content.len(), 2);
    }

    // ── Image blocks ───────────────────────────────────────────────

    fn image_block(width: u32, height: u32) -> ContentBlock {
        ContentBlock::Image {
            image: crate::image::ImageRef {
                blob_id: uuid::Uuid::from_u128(u128::from(width) << 32 | u128::from(height)),
                media_type: crate::image::ImageMediaType::Png,
                width,
                height,
                byte_len: 4_096,
            },
        }
    }

    fn image_only_user_msg(width: u32, height: u32) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: vec![image_block(width, height)],
            reasoning: MessageReasoning::default(),
        }
    }

    #[test]
    fn an_image_is_charged_tokens_rather_than_riding_along_free() {
        // A 1024×1024 image is four 512-px tiles.
        let tokens = estimate_block_tokens(&image_block(1_024, 1_024));
        assert_eq!(tokens, IMAGE_OVERHEAD + 4 * 1_600);
        // Comfortably more than a short text block, which is the whole point:
        // budgeting must not treat pixels as nearly free.
        assert!(tokens > estimate_block_tokens(&ContentBlock::Text { text: "hi".into() }));
    }

    #[test]
    fn an_image_message_floor_is_its_full_cost_not_a_stub() {
        let msg = image_only_user_msg(1_024, 1_024);
        let full = estimate_message_tokens(&msg);
        // Even at an aggressively small content floor, an image cannot shrink,
        // so its floor must still equal the full cost. Reporting a smaller
        // floor is what lets an image-heavy transcript slip past the budget
        // gate and overflow the context window.
        assert_eq!(message_floor(&msg, 10), full);
    }

    #[test]
    fn truncating_an_image_block_leaves_it_byte_identical() {
        let block = image_block(800, 600);
        assert_eq!(truncate_block(&block, 1), block);
    }

    #[test]
    fn an_image_only_user_turn_can_anchor_the_transcript() {
        // "What is in this screenshot?" where the question is the image.
        let mut msgs = vec![image_only_user_msg(800, 600), assistant_msg("a chart")];
        ensure_starts_with_user(&mut msgs);
        assert_eq!(msgs.len(), 2, "the image-only turn must not be discarded");
        assert!(matches!(msgs[0].content[0], ContentBlock::Image { .. }));
    }

    #[test]
    fn reduction_keeps_an_image_only_turn_instead_of_emptying_the_transcript() {
        let msgs = vec![image_only_user_msg(2_048, 2_048)];
        // A budget far below the image's cost: the transcript cannot be made
        // to fit, but returning nothing would be worse than overshooting.
        let (fitted, _) = fit_to_budget(&msgs, 10, 0);
        assert!(
            !fitted.is_empty(),
            "reduction must not discard the only user turn"
        );
    }

    #[test]
    fn evicting_an_image_leaves_a_stand_in_the_model_can_read() {
        let evicted = evict_image_block(&image_block(800, 600));
        let ContentBlock::Text { text } = &evicted else {
            panic!("expected a text stand-in, got {evicted:?}");
        };
        assert!(text.contains("image/png"), "{text}");
        assert!(text.contains("800"), "{text}");
        assert!(text.contains("600"), "{text}");
        // Cheap enough that eviction actually reclaims budget.
        assert!(estimate_block_tokens(&evicted) < estimate_block_tokens(&image_block(800, 600)));
    }

    #[test]
    fn eviction_keeps_the_newest_images_and_stands_in_for_the_rest() {
        let mut msgs = vec![
            image_only_user_msg(100, 100),
            assistant_msg("first"),
            image_only_user_msg(200, 200),
            assistant_msg("second"),
            image_only_user_msg(300, 300),
        ];
        evict_images_beyond(&mut msgs, 2);

        // Oldest loses its pixels; the two newest keep theirs.
        assert!(matches!(msgs[0].content[0], ContentBlock::Text { .. }));
        assert!(matches!(msgs[2].content[0], ContentBlock::Image { .. }));
        assert!(matches!(msgs[4].content[0], ContentBlock::Image { .. }));

        evict_all_images(&mut msgs);
        assert!(
            msgs.iter()
                .flat_map(|m| &m.content)
                .all(|b| !matches!(b, ContentBlock::Image { .. })),
            "evict_all_images must leave no image blocks"
        );
    }

    #[test]
    fn old_tool_result_previews_lose_pixels_by_message_recency() {
        let mut messages = vec![ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "old".into(),
                    content: "old result".into(),
                    is_error: false,
                },
                image_block(400, 300),
            ],
            reasoning: MessageReasoning::default(),
        }];
        messages.extend((0..10).map(|index| assistant_msg(&format!("message {index}"))));
        messages.push(ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "new".into(),
                    content: "new result".into(),
                    is_error: false,
                },
                image_block(800, 600),
            ],
            reasoning: MessageReasoning::default(),
        });

        evict_old_tool_result_images(&mut messages, TOOL_RESULT_IMAGE_MESSAGE_WINDOW);

        assert!(matches!(messages[0].content[1], ContentBlock::Text { .. }));
        assert!(matches!(
            messages.last().unwrap().content[1],
            ContentBlock::Image { .. }
        ));
    }
}
