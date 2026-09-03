//! The foreground agent's explicit memory verb.
//!
//! Three verbs, one tool: `propose` drafts a durable record for the user to
//! review, `search` runs the backend's lexical search, and `read` loads one
//! record. It needs the memory backend and the store, so it lives here rather
//! than in the core tool module — the execution context carries the
//! conversation and the call, not a backend handle.
//!
//! A `propose` never lands with authority. The record is written as
//! `proposed` with model authorship and evidence pointing at this
//! conversation's durable transcript, so the storage layer's review lifecycle
//! (decision 0067) is the consent gate; there is no approval card to click
//! through here.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tidebreak_core::{
    memory_tool_spec, parse_memory_tool_arguments, ApprovalClass, MemoryAuthor, MemoryBackend,
    MemoryError, MemoryEvidence, MemoryOrigin, MemoryProvenance, MemoryRecord, MemoryRecordId,
    MemoryScope, MemorySearchRequest, MemoryStatus, MemoryToolVerb, Result, Role, Store, Tool,
    ToolCtx, ToolErrorCategory, ToolOutput, ToolSpec, MEMORY_TOOL_SEARCH_LIMIT,
};

/// Search, read, and propose against the owner's durable memory.
pub(crate) struct MemoryTool {
    memory: Arc<dyn MemoryBackend>,
    store: Arc<dyn Store>,
}

impl MemoryTool {
    pub(crate) fn new(memory: Arc<dyn MemoryBackend>, store: Arc<dyn Store>) -> Self {
        Self { memory, store }
    }
}

/// Map a backend refusal onto the category the model can act on.
///
/// The cap errors keep the backend's own coaching text: it already names the
/// outs (consolidate, archive, shorten) and inventing a second phrasing here
/// would drift from the one the review surfaces show.
fn memory_failure(error: MemoryError) -> ToolOutput {
    let category = match &error {
        MemoryError::InvalidRecord(_)
        | MemoryError::EvidenceNotFound(_)
        | MemoryError::ScopeNotFound
        | MemoryError::NotFound
        | MemoryError::AlreadyExists
        | MemoryError::ActiveRecordCapExceeded { .. }
        | MemoryError::DigestCapExceeded { .. } => ToolErrorCategory::InvalidArguments,
        MemoryError::Unsupported(_) => ToolErrorCategory::ConfigurationRequired,
        _ => ToolErrorCategory::ToolFailed,
    };
    ToolOutput::failed(category, format!("memory call failed: {error}"))
}

#[async_trait]
impl Tool for MemoryTool {
    fn spec(&self) -> ToolSpec {
        memory_tool_spec()
    }

    /// `ReadOnly` even though `propose` writes a row, for the same reason
    /// `update_task_plan` is: the class governs consent, and this call
    /// reaches only the owner's own review queue — a `proposed` record
    /// carries no authority until the user activates it, so the review
    /// lifecycle is the gate an approval card would duplicate. The registry
    /// still excludes the tool from the plan-mode surface by name, exactly
    /// like the task plan: a plan turn must not commit rows.
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let arguments = match parse_memory_tool_arguments(&args) {
            Ok(arguments) => arguments,
            Err(correction) => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    correction,
                ));
            }
        };
        // The surface already withholds the verb from an incognito chat;
        // check again here so an unadvertised call cannot reach memory from
        // a conversation the user opted out.
        match self.store.get_chat(ctx.chat_id).await {
            Ok(Some(chat)) if chat.memory_incognito => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::UserDeclined,
                    "this conversation keeps memory off; do not retry",
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::ToolFailed,
                    format!("memory is unavailable right now: {error}"),
                ));
            }
        }
        // The global switch composes no digest and withholds the verb; an
        // unadvertised call must not read or propose past it either.
        let enabled = self
            .store
            .get_setting(crate::routes::MEMORY_ENABLED_SETTING)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if !enabled {
            return Ok(ToolOutput::failed(
                ToolErrorCategory::UserDeclined,
                "memory is switched off in settings; do not retry",
            ));
        }
        // Memory rows are owner-scoped at every backend operation. Fail
        // closed when the store cannot name the person this conversation
        // belongs to, rather than defaulting anyone onto the local scope.
        let owner = match self.store.chat_owner(ctx.chat_id).await {
            Ok(Some(owner)) => owner,
            Ok(None) => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::ConfigurationRequired,
                    "memory is unavailable here: this conversation's owner could not be resolved",
                ));
            }
            Err(error) => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::ToolFailed,
                    format!("memory is unavailable right now: {error}"),
                ));
            }
        };
        match arguments.verb {
            MemoryToolVerb::Search => {
                let query = arguments.query.unwrap_or_default();
                let hits = match self
                    .memory
                    .search(
                        &owner,
                        MemorySearchRequest {
                            query,
                            scope: None,
                            statuses: vec![MemoryStatus::Active],
                            limit: MEMORY_TOOL_SEARCH_LIMIT,
                        },
                    )
                    .await
                {
                    Ok(hits) => hits,
                    Err(error) => return Ok(memory_failure(error)),
                };
                if hits.is_empty() {
                    return Ok(ToolOutput::text("No stored memory matches that query."));
                }
                let mut lines = Vec::with_capacity(hits.len());
                for hit in &hits {
                    lines.push(format!(
                        "- {} — {} (id {}): {}",
                        hit.updated_at.format("%Y-%m-%d"),
                        hit.title,
                        hit.record_id,
                        hit.matching_line
                    ));
                }
                Ok(ToolOutput::text(lines.join("\n"))
                    .with_data(serde_json::to_value(&hits).unwrap_or_default()))
            }
            MemoryToolVerb::Read => {
                let raw = arguments.record_id.unwrap_or_default();
                let Ok(id) = raw.parse::<MemoryRecordId>() else {
                    return Ok(ToolOutput::failed(
                        ToolErrorCategory::InvalidArguments,
                        "record_id is not a memory record id; use the id a search hit named",
                    ));
                };
                match self.memory.get(&owner, id).await {
                    Ok(Some(record)) => Ok(ToolOutput::text(format!(
                        "# {}\nkind: {} — status: {} — updated {}\n\n{}",
                        record.title,
                        record.kind.as_str(),
                        record.status.as_str(),
                        record.updated_at.format("%Y-%m-%d"),
                        record.body
                    ))),
                    Ok(None) => Ok(ToolOutput::failed(
                        ToolErrorCategory::NotFound,
                        "no memory record has that id",
                    )),
                    Err(error) => Ok(memory_failure(error)),
                }
            }
            MemoryToolVerb::Propose => {
                // The storage layer refuses a model-authored record without
                // resolvable evidence, so the proposal is pinned to the
                // newest durable user message of this very conversation —
                // the material the model is proposing from. Its turn is the
                // running turn, which is what attributes the proposal to a
                // transcript row.
                let evidence = match self.store.list_messages(ctx.chat_id).await {
                    Ok(messages) => messages
                        .iter()
                        .rev()
                        .find(|message| message.role == Role::User)
                        .map(|message| {
                            (
                                MemoryEvidence::Message {
                                    message_id: message.id,
                                },
                                message.turn_id,
                            )
                        }),
                    Err(error) => {
                        return Ok(ToolOutput::failed(
                            ToolErrorCategory::ToolFailed,
                            format!("the proposal could not be recorded: {error}"),
                        ));
                    }
                };
                let Some((evidence, turn_id)) = evidence else {
                    return Ok(ToolOutput::failed(
                        ToolErrorCategory::ToolFailed,
                        "a memory proposal needs a conversation with at least one user message",
                    ));
                };
                let now = chrono::Utc::now();
                let record = MemoryRecord {
                    id: MemoryRecordId::new(),
                    scope: MemoryScope::Personal,
                    kind: arguments.kind.expect("propose arguments carry a kind"),
                    status: MemoryStatus::Proposed,
                    title: arguments.title.unwrap_or_default().trim().to_owned(),
                    body: arguments.body.unwrap_or_default().trim().to_owned(),
                    provenance: MemoryProvenance {
                        author: MemoryAuthor::Model,
                        origin: MemoryOrigin {
                            chat_id: Some(ctx.chat_id),
                            turn_id: Some(turn_id),
                            ..MemoryOrigin::default()
                        },
                        evidence: vec![evidence],
                    },
                    links: Vec::new(),
                    expires_at: None,
                    superseded_by: None,
                    observation_count: 0,
                    revision: 1,
                    created_at: now,
                    updated_at: now,
                };
                match self.memory.put(&owner, record).await {
                    Ok(receipt) => Ok(ToolOutput::text(format!(
                        "Proposed memory {} for the user to review. It is a draft: it carries no \
                         authority unless the user activates it.",
                        receipt.record.id
                    ))),
                    Err(error) => Ok(memory_failure(error)),
                }
            }
        }
    }
}
