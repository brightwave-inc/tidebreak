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
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } => TOOL_RESULT_OVERHEAD + estimate_tokens(tool_use_id) + estimate_tokens(content),
    }
}

/// Estimate tokens for a full message (role overhead + blocks).
pub fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    ROLE_OVERHEAD + msg.content.iter().map(estimate_block_tokens).sum::<usize>()
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
        return (messages.to_vec(), false);
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

/// Minimum viable token size for a message: role overhead + each block at the
/// floor.
fn message_floor(msg: &ChatMessage, content_floor: usize) -> usize {
    ROLE_OVERHEAD
        + msg
            .content
            .iter()
            .map(|block| {
                let overhead = block_overhead(block);
                let content = estimate_block_tokens(block).saturating_sub(overhead);
                overhead + content.min(content_floor)
            })
            .sum::<usize>()
}

fn block_overhead(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { .. } => TEXT_OVERHEAD,
        ContentBlock::ToolUse { .. } => TOOL_USE_OVERHEAD,
        ContentBlock::ToolResult { .. } => TOOL_RESULT_OVERHEAD,
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
    let has_user_text = entries.iter().enumerate().any(|(i, e)| {
        e.kept
            && messages[i].role == Role::User
            && messages[i]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { .. }))
    });
    if !has_user_text {
        for i in (0..entries.len()).rev() {
            if messages[i].role == Role::User
                && messages[i]
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { .. }))
            {
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
    let available = target_tokens.saturating_sub(ROLE_OVERHEAD);
    let total: usize = msg.content.iter().map(estimate_block_tokens).sum();
    if total == 0 {
        return msg.clone();
    }

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut remaining = available;

    for block in &msg.content {
        let full = estimate_block_tokens(block);
        let share = ((available as u64 * full as u64) / total as u64) as usize;
        let share = share.min(remaining).max(block_overhead(block));

        if share >= full {
            blocks.push(block.clone());
            remaining = remaining.saturating_sub(full);
        } else {
            blocks.push(truncate_block(block, share));
            remaining = remaining.saturating_sub(share);
        }
    }

    ChatMessage {
        role: msg.role,
        content: blocks,
    }
}

fn truncate_block(block: &ContentBlock, target_tokens: usize) -> ContentBlock {
    match block {
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
    }
}

fn truncate_str(s: &str, target_tokens: usize) -> String {
    let char_budget = target_tokens * 3;
    if s.chars().count() <= char_budget {
        return s.to_string();
    }
    let suffix_chars = TRUNCATION_SUFFIX.chars().count();
    let keep = char_budget.saturating_sub(suffix_chars);
    let truncated: String = s.chars().take(keep).collect();
    format!("{truncated}{TRUNCATION_SUFFIX}")
}

// ── Cleanup ────────────────────────────────────────────────────────

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
        if first.role == Role::User
            && first
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { .. }))
        {
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
        }
    }

    // ── Token estimation ───────────────────────────────────────────

    #[test]
    fn estimate_tokens_uses_chars_div_3() {
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hello world"), 4); // ceil(11/3)
    }

    #[test]
    fn estimate_message_tokens_sums_blocks() {
        let msg = ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "hello".into(),
                },
                ContentBlock::Text {
                    text: "world".into(),
                },
            ],
        };
        let tokens = estimate_message_tokens(&msg);
        assert_eq!(
            tokens,
            ROLE_OVERHEAD + (TEXT_OVERHEAD + estimate_tokens("hello")) * 2
        );
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

    #[test]
    fn empty_transcript_is_noop() {
        let (result, reduced) = fit_to_budget(&[], 1000, 500);
        assert!(!reduced);
        assert!(result.is_empty());
    }

    // ── strip_orphaned_tool_blocks ─────────────────────────────────

    #[test]
    fn strips_orphaned_tool_use() {
        let mut msgs = vec![
            user_msg("go"),
            tool_use_msg("tu_1", "read_file", "a"),
            // No matching ToolResult for tu_1.
            user_msg("next"),
        ];
        strip_orphaned_tool_blocks(&mut msgs);
        assert!(!msgs.iter().any(|m| m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))));
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

    #[test]
    fn keeps_paired_tool_blocks() {
        let mut msgs = vec![
            user_msg("go"),
            tool_use_msg("tu_1", "read_file", "a"),
            tool_result_msg("tu_1", "data"),
            assistant_msg("done"),
        ];
        strip_orphaned_tool_blocks(&mut msgs);
        assert!(msgs.iter().any(|m| m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { id, .. } if id == "tu_1"))));
        assert!(msgs.iter().any(|m| m.content.iter().any(
            |b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "tu_1")
        )));
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

    #[test]
    fn truncate_str_noop_when_fits() {
        let s = "hello";
        let result = truncate_str(s, 100);
        assert_eq!(result, s);
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

    #[test]
    fn ensure_starts_with_user_drops_leading_assistant() {
        let mut msgs = vec![
            assistant_msg("stale"),
            user_msg("fresh"),
            assistant_msg("reply"),
        ];
        ensure_starts_with_user(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
    }
}
