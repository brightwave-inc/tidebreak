//! Branch a conversation into a new one that starts with a copy of its
//! history.
//!
//! A branch owns everything it shows. The original can be renamed, moved,
//! deleted, or continued, and the branch keeps working, because nothing in it
//! points back except the link on its session row.
//!
//! What is copied is what the model reads and what the transcript shows for
//! each copied turn: the turn rows, their messages, their settled tool calls,
//! the images and files the messages carried, and the citations under the
//! answers. The documents added before the branch point come along under new
//! ids, and every mention of an old id in the copied model context is
//! rewritten to the new one. The conversation's image publications come along
//! too, so a later turn in the branch can send those images again. The
//! compaction checkpoint comes along only when the latest turn it summarized
//! was copied: a summary written later can repeat what the branch left out.
//!
//! What stays with the original: the event journal (so a copied turn keeps its
//! answer but not the streamed reasoning summary), outputs and their bytes,
//! the file-change journal and its undo, background agent runs, approvals and
//! standing grants, the task plan, and connected folders. A branch in a
//! project gets the project's folders the way any new conversation does.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{CallId, DocumentId, MessageId, SessionId, TurnId};
use crate::model::{
    validate_chat_root_projection, ChatRootAttachment, OwnerId, RootAttachmentOrigin,
    TurnPlacement, TurnPlacements, TurnReplacementKind, TurnRunStatus,
};
use crate::storage::{BranchChat, BranchChatOutcome, ChatBranchPoint, ChatBranchRefusal};

use super::super::{entities, store_err, DbStore};
use super::acquire_chat_write_lock;
use super::agent_run::insert_foreground_agent_run_on;
use super::blob as blob_ops;
use super::conversation::{
    insert_chat_on, internal_sessions, load_chat_project_roots, reserve_message_identity_on,
    MESSAGE_IDENTITY_OWNER_MESSAGE,
};

/// The stored statuses of a turn that has finished, however it ended.
const SETTLED: &[&str] = TurnRunStatus::TERMINAL;

/// Copy `request.source`'s conversation up to `request.point` into the new
/// conversation `request.chat`, in one transaction.
pub(in crate::db) async fn branch_chat(
    store: &DbStore,
    owner: Option<&OwnerId>,
    request: &BranchChat,
) -> Result<BranchChatOutcome> {
    if request.chat.attachment_revision != 0 || !request.chat.root_attachments.is_empty() {
        return Err(AgentError::Store(
            "a branch takes its folders from its project, not from the request".into(),
        ));
    }
    let source = request.source;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, source).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(BranchChatOutcome::NotFound);
    }
    let mut owned = entities::session::Entity::find_by_id(source.0).filter(internal_sessions());
    if let Some(owner) = owner {
        owned = owned.filter(entities::session::Column::Owner.eq(owner.as_str()));
    }
    if owned.one(&transaction).await.map_err(store_err)?.is_none() {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(BranchChatOutcome::NotFound);
    }

    let turns = entities::turn::Entity::find()
        .filter(entities::turn::Column::SessionId.eq(source.0))
        .order_by_asc(entities::turn::Column::Ordinal)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let replacements = super::turn::list_turn_replacements_on(&transaction, source).await?;
    let placements = TurnPlacements::new(&replacements);
    let (copied, point) = match turns_to_copy(&turns, &placements, request.point) {
        Ok(copied) => copied,
        Err(refusal) => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(BranchChatOutcome::Refused(refusal));
        }
    };

    // The new conversation: the caller's settings, the project's folders, and
    // the link back to where it came from.
    let project_roots =
        load_chat_project_roots(&transaction, request.chat.project_id, owner).await?;
    let mut chat = request.chat.clone();
    chat.root_attachments = project_roots
        .into_iter()
        .map(|root_id| ChatRootAttachment {
            root_id,
            origin: RootAttachmentOrigin::ProjectDefault,
        })
        .collect();
    if !chat.root_attachments.is_empty() {
        chat.attachment_revision = 1;
    }
    validate_chat_root_projection(&chat).map_err(|message| AgentError::Store(message.into()))?;
    insert_chat_on(&transaction, &chat, owner).await?;
    insert_foreground_agent_run_on(&transaction, chat.id, chat.created_at).await?;
    entities::session::ActiveModel {
        id: Set(chat.id.0),
        branched_from_session_id: Set(Some(source.0)),
        branched_from_turn_id: Set(copied.last().map(|turn| turn.id)),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    let branch = chat.id;
    let branch_owner = entities::session::Entity::find_by_id(branch.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .map(|session| session.owner)
        .ok_or_else(|| AgentError::Store(format!("branch {branch} vanished after insert")))?;

    let documents = copy_documents(
        &transaction,
        source,
        branch,
        &branch_owner,
        &point,
        &request.carry_documents,
    )
    .await?;
    copy_image_publications(&transaction, source, branch).await?;

    let turn_ids: HashMap<uuid::Uuid, uuid::Uuid> = copied
        .iter()
        .map(|turn| (turn.id, TurnId::new().0))
        .collect();
    let source_turns: Vec<uuid::Uuid> = copied.iter().map(|turn| turn.id).collect();
    let messages = copy_messages(
        &transaction,
        source,
        branch,
        &source_turns,
        &turn_ids,
        &documents,
    )
    .await?;

    for (ordinal, turn) in copied.iter().enumerate() {
        copy_turn(
            &transaction,
            turn,
            branch,
            &branch_owner,
            i64::try_from(ordinal + 1)
                .map_err(|_| AgentError::Store("branch turn ordinal overflow".into()))?,
            &turn_ids,
            &messages,
        )
        .await?;
    }
    copy_tool_calls(
        &transaction,
        source,
        branch,
        &source_turns,
        &turn_ids,
        &documents,
    )
    .await?;
    copy_attachments(
        &transaction,
        &source_turns,
        &turn_ids,
        &messages,
        &documents,
        &branch_owner,
    )
    .await?;
    copy_citations(&transaction, &messages, &documents).await?;
    copy_checkpoint(
        &transaction,
        source,
        branch,
        &messages,
        &documents,
        &turn_ids,
        &point,
    )
    .await?;

    transaction.commit().await.map_err(store_err)?;
    Ok(BranchChatOutcome::Branched {
        chat_id: branch,
        documents: documents
            .into_iter()
            .map(|(old, new)| (DocumentId(old), DocumentId(new)))
            .collect(),
    })
}

/// The moment a branch copies up to: what existed by then belongs to it.
struct BranchCutoff {
    at: DateTime<Utc>,
    /// Whether something stamped exactly `at` is still before the point.
    inclusive: bool,
}

impl BranchCutoff {
    fn includes(&self, stamped: DateTime<Utc>) -> bool {
        if self.inclusive {
            stamped <= self.at
        } else {
            stamped < self.at
        }
    }
}

/// The source turns a branch copies, oldest first, and the moment it copies
/// up to.
///
/// Only the conversation as it stands is copied: a regenerated answer's
/// earlier versions and an edited turn stay behind. A retry and the turns it
/// retried are one attempt; branching through a turn copies its attempt up to
/// that turn, and branching before a turn leaves its whole attempt out.
/// Branching from an earlier version copies the history before it and that
/// version.
fn turns_to_copy<'a>(
    turns: &'a [entities::turn::Model],
    placements: &TurnPlacements,
    point: ChatBranchPoint,
) -> std::result::Result<(Vec<&'a entities::turn::Model>, BranchCutoff), ChatBranchRefusal> {
    let (target, inclusive) = match point {
        ChatBranchPoint::Through(turn) => (turn, true),
        ChatBranchPoint::Before(turn) => (turn, false),
    };
    let Some(target_row) = turns.iter().find(|turn| turn.id == target.0) else {
        return Err(ChatBranchRefusal::UnknownTurn);
    };
    if inclusive {
        if !SETTLED.contains(&target_row.status.as_str()) {
            return Err(ChatBranchRefusal::Unsettled);
        }
        if placements.placement(target) == TurnPlacement::Discarded {
            return Err(ChatBranchRefusal::NotInConversation);
        }
    }
    let attempt: HashSet<uuid::Uuid> = placements
        .attempt_turns(placements.attempt(target))
        .into_iter()
        .map(|turn| turn.0)
        .collect();
    let attempt_start = turns
        .iter()
        .filter(|turn| attempt.contains(&turn.id))
        .map(|turn| turn.ordinal)
        .min()
        .unwrap_or(target_row.ordinal);
    let copied: Vec<&entities::turn::Model> = turns
        .iter()
        .filter(|turn| {
            if turn.ordinal < attempt_start {
                placements.in_conversation(TurnId(turn.id))
            } else {
                inclusive && attempt.contains(&turn.id) && turn.ordinal <= target_row.ordinal
            }
        })
        .collect();
    // Everything before a settled turn has settled too; a row that has not
    // is one this copy would have to fence, so refuse rather than copy it.
    if copied
        .iter()
        .any(|turn| !SETTLED.contains(&turn.status.as_str()))
    {
        return Err(ChatBranchRefusal::Unsettled);
    }
    let cutoff = if inclusive {
        BranchCutoff {
            at: target_row.ended_at.unwrap_or(target_row.started_at),
            inclusive: true,
        }
    } else {
        let started = turns
            .iter()
            .find(|turn| turn.ordinal == attempt_start)
            .unwrap_or(target_row);
        BranchCutoff {
            at: started.started_at,
            inclusive: false,
        }
    };
    Ok((copied, cutoff))
}

/// Copy the documents added before the branch point, and the ones the
/// branch's first message carries, under new ids, keeping the derived ids
/// derived so a later re-import in the branch finds its copy.
///
/// A document added after the point would be offered to the model in the
/// branch, which never saw the turn that added it.
async fn copy_documents<C>(
    conn: &C,
    source: SessionId,
    branch: SessionId,
    owner: &str,
    point: &BranchCutoff,
    carry: &[DocumentId],
) -> Result<HashMap<uuid::Uuid, uuid::Uuid>>
where
    C: ConnectionTrait,
{
    let rows: Vec<entities::document::Model> = entities::document::Entity::find()
        .filter(entities::document::Column::ChatId.eq(source.0))
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .filter(|row| point.includes(row.created_at) || carry.contains(&DocumentId(row.id)))
        .collect();
    let now = Utc::now();
    let mut ids = HashMap::with_capacity(rows.len());
    for row in rows {
        let old = DocumentId(row.id);
        let by_origin = row
            .origin_uri
            .as_deref()
            .filter(|uri| DocumentId::derive_for_chat(source, uri) == old)
            .map(|uri| DocumentId::derive_for_chat(branch, uri));
        let by_content = row
            .source_sha256
            .as_deref()
            .and_then(|digest| <[u8; 32]>::try_from(digest).ok())
            .filter(|digest| DocumentId::derive_for_chat_content(source, *digest) == old)
            .map(|digest| DocumentId::derive_for_chat_content(branch, digest));
        let new = by_origin.or(by_content).unwrap_or_else(DocumentId::new);
        ids.insert(row.id, new.0);
        if let Some(blob) = row.source_blob_id {
            blob_ops::cancel_on(conn, blob).await?;
        }
        let mut copy: entities::document::ActiveModel = row.into();
        copy.id = Set(new.0);
        copy.chat_id = Set(Some(branch.0));
        copy.project_id = Set(None);
        copy.owner = Set(owner.to_owned());
        copy.updated_at = Set(now);
        copy.insert(conn).await.map_err(store_err)?;
    }
    Ok(ids)
}

/// Give the branch the same authority to attach images the original had.
async fn copy_image_publications<C>(conn: &C, source: SessionId, branch: SessionId) -> Result<()>
where
    C: ConnectionTrait,
{
    let rows = entities::chat_image_publication::Entity::find()
        .filter(entities::chat_image_publication::Column::ChatId.eq(source.0))
        .all(conn)
        .await
        .map_err(store_err)?;
    for row in rows {
        blob_ops::cancel_on(conn, row.blob_id).await?;
        let mut copy: entities::chat_image_publication::ActiveModel = row.into();
        copy.chat_id = Set(branch.0);
        copy.insert(conn).await.map_err(store_err)?;
    }
    Ok(())
}

/// Copy every message of the copied turns, in order, and return the id map.
async fn copy_messages<C>(
    conn: &C,
    source: SessionId,
    branch: SessionId,
    source_turns: &[uuid::Uuid],
    turn_ids: &HashMap<uuid::Uuid, uuid::Uuid>,
    documents: &HashMap<uuid::Uuid, uuid::Uuid>,
) -> Result<HashMap<uuid::Uuid, uuid::Uuid>>
where
    C: ConnectionTrait,
{
    if source_turns.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = entities::message::Entity::find()
        .filter(entities::message::Column::ChatId.eq(source.0))
        .filter(entities::message::Column::TurnId.is_in(source_turns.iter().copied()))
        .order_by_asc(entities::message::Column::Seq)
        .all(conn)
        .await
        .map_err(store_err)?;
    let mut ids = HashMap::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        let new = MessageId::new();
        let turn = *turn_ids
            .get(&row.turn_id)
            .ok_or_else(|| AgentError::Store("a copied message lost its turn".into()))?;
        if !reserve_message_identity_on(
            conn,
            new,
            branch,
            TurnId(turn),
            MESSAGE_IDENTITY_OWNER_MESSAGE,
        )
        .await?
        {
            return Err(AgentError::Store(format!(
                "branch message identity {new} is already reserved"
            )));
        }
        ids.insert(row.id, new.0);
        let llm_content = row
            .llm_content
            .as_deref()
            .map(|text| rewrite_ids(text, documents));
        let mut copy: entities::message::ActiveModel = row.into();
        copy.id = Set(new.0);
        copy.chat_id = Set(branch.0);
        copy.turn_id = Set(turn);
        copy.seq = Set(i64::try_from(index + 1)
            .map_err(|_| AgentError::Store("branch message sequence overflow".into()))?);
        copy.llm_content = Set(llm_content);
        copy.turn_lease_token = Set(None);
        copy.insert(conn).await.map_err(store_err)?;
    }
    Ok(ids)
}

/// Copy one settled turn under its new id and ordinal.
async fn copy_turn<C>(
    conn: &C,
    turn: &entities::turn::Model,
    branch: SessionId,
    owner: &str,
    ordinal: i64,
    turn_ids: &HashMap<uuid::Uuid, uuid::Uuid>,
    messages: &HashMap<uuid::Uuid, uuid::Uuid>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let new = *turn_ids
        .get(&turn.id)
        .ok_or_else(|| AgentError::Store("a copied turn lost its id".into()))?;
    let input = turn
        .input_message_id
        .and_then(|id| messages.get(&id).copied());
    let output = turn
        .output_message_id
        .and_then(|id| messages.get(&id).copied());
    let mut copy: entities::turn::ActiveModel = turn.clone().into();
    copy.id = Set(new);
    copy.owner = Set(owner.to_owned());
    copy.session_id = Set(branch.0);
    copy.ordinal = Set(ordinal);
    copy.input_message_id = Set(input);
    copy.output_message_id = Set(output);
    copy.lease_token = Set(None);
    copy.lease_expires_at = Set(None);
    copy.park_ref = Set(None);
    copy.park_wait = Set(None);
    copy.available_at = Set(None);
    // A copy was never submitted under its new id, so it has no request to
    // compare a retry against.
    copy.fingerprint = Set(None);
    // A retry copied with the turn it retried is still one attempt in the
    // branch. Every other rerun's replaced turn stayed behind.
    let retried = turn
        .replaces_turn_id
        .filter(|_| turn.replacement.as_deref() == Some(TurnReplacementKind::Retry.as_str()))
        .and_then(|replaced| turn_ids.get(&replaced).copied());
    copy.replaces_turn_id = Set(retried);
    copy.replacement = Set(retried.map(|_| TurnReplacementKind::Retry.as_str().to_owned()));
    copy.updated_at = Set(Some(Utc::now()));
    copy.insert(conn).await.map_err(store_err)?;
    Ok(())
}

/// Copy the settled tool calls of the copied turns, with their order.
async fn copy_tool_calls<C>(
    conn: &C,
    source: SessionId,
    branch: SessionId,
    source_turns: &[uuid::Uuid],
    turn_ids: &HashMap<uuid::Uuid, uuid::Uuid>,
    documents: &HashMap<uuid::Uuid, uuid::Uuid>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if source_turns.is_empty() {
        return Ok(());
    }
    let rows = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(source.0))
        .filter(entities::tool_call::Column::TurnId.is_in(source_turns.iter().copied()))
        .filter(entities::tool_call::Column::Status.is_in([
            crate::model::ToolCallStatus::Completed.as_str(),
            crate::model::ToolCallStatus::Failed.as_str(),
            crate::model::ToolCallStatus::Cancelled.as_str(),
        ]))
        .order_by_asc(entities::tool_call::Column::HistoryOrder)
        .all(conn)
        .await
        .map_err(store_err)?;
    for row in rows {
        let turn = *turn_ids
            .get(&row.turn_id)
            .ok_or_else(|| AgentError::Store("a copied tool call lost its turn".into()))?;
        let arguments = rewrite_json(&row.arguments, documents)?;
        let raw_arguments = row
            .raw_arguments
            .as_deref()
            .map(|text| rewrite_ids(text, documents));
        let result = row
            .result
            .as_deref()
            .map(|text| rewrite_ids(text, documents));
        let mut copy: entities::tool_call::ActiveModel = row.into();
        copy.id = Set(CallId::new().0);
        copy.chat_id = Set(branch.0);
        copy.turn_id = Set(turn);
        copy.arguments = Set(arguments);
        copy.raw_arguments = Set(raw_arguments);
        copy.result = Set(result);
        copy.client_executor_id = Set(None);
        copy.client_lease_token = Set(None);
        copy.client_lease_expires_at = Set(None);
        copy.turn_lease_token = Set(None);
        copy.resolution_turn_lease_token = Set(None);
        copy.insert(conn).await.map_err(store_err)?;
    }
    Ok(())
}

/// Copy the images and files the copied user messages carried.
async fn copy_attachments<C>(
    conn: &C,
    source_turns: &[uuid::Uuid],
    turn_ids: &HashMap<uuid::Uuid, uuid::Uuid>,
    messages: &HashMap<uuid::Uuid, uuid::Uuid>,
    documents: &HashMap<uuid::Uuid, uuid::Uuid>,
    owner: &str,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if source_turns.is_empty() {
        return Ok(());
    }
    let images = entities::turn_attachment::Entity::find()
        .filter(entities::turn_attachment::Column::TurnId.is_in(source_turns.iter().copied()))
        .all(conn)
        .await
        .map_err(store_err)?;
    let mut blobs = HashSet::new();
    for row in images {
        let turn = *turn_ids
            .get(&row.turn_id)
            .ok_or_else(|| AgentError::Store("a copied image lost its turn".into()))?;
        blobs.insert(row.blob_id);
        let message = row.message_id.and_then(|id| messages.get(&id).copied());
        let mut copy: entities::turn_attachment::ActiveModel = row.into();
        copy.turn_id = Set(turn);
        copy.owner = Set(owner.to_owned());
        copy.message_id = Set(message);
        copy.insert(conn).await.map_err(store_err)?;
    }
    for blob in blobs {
        blob_ops::cancel_on(conn, blob).await?;
    }
    let files = entities::code_turn_document_attachment::Entity::find()
        .filter(
            entities::code_turn_document_attachment::Column::TurnId
                .is_in(source_turns.iter().copied()),
        )
        .all(conn)
        .await
        .map_err(store_err)?;
    for row in files {
        let turn = *turn_ids
            .get(&row.turn_id)
            .ok_or_else(|| AgentError::Store("a copied file lost its turn".into()))?;
        let Some(document) = documents.get(&row.document_id).copied() else {
            // A file from outside this conversation (a project source) stays
            // where it is; the branch reads it through its project if it has
            // one, exactly as the original did.
            continue;
        };
        let message = row.message_id.and_then(|id| messages.get(&id).copied());
        let mut copy: entities::code_turn_document_attachment::ActiveModel = row.into();
        copy.turn_id = Set(turn);
        copy.owner = Set(owner.to_owned());
        copy.message_id = Set(message);
        copy.document_id = Set(document);
        copy.insert(conn).await.map_err(store_err)?;
    }
    Ok(())
}

/// Copy the citations under the copied answers.
async fn copy_citations<C>(
    conn: &C,
    messages: &HashMap<uuid::Uuid, uuid::Uuid>,
    documents: &HashMap<uuid::Uuid, uuid::Uuid>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if messages.is_empty() {
        return Ok(());
    }
    let rows = entities::assistant_citation::Entity::find()
        .filter(entities::assistant_citation::Column::MessageId.is_in(messages.keys().copied()))
        .all(conn)
        .await
        .map_err(store_err)?;
    for row in rows {
        let message = messages[&row.message_id];
        let document = documents
            .get(&row.document_id)
            .copied()
            .unwrap_or(row.document_id);
        let mut copy: entities::assistant_citation::ActiveModel = row.into();
        copy.id = Set(uuid::Uuid::new_v4());
        copy.message_id = Set(message);
        copy.document_id = Set(document);
        copy.insert(conn).await.map_err(store_err)?;
    }
    Ok(())
}

/// Copy the compaction checkpoint when everything it summarizes was copied.
///
/// The summary covers the whole view it was written from, not only the
/// messages before its boundary, so the latest turn of that view has to be
/// copied too. A checkpoint from before that turn was recorded is copied only
/// when it was written before the branch point.
async fn copy_checkpoint<C>(
    conn: &C,
    source: SessionId,
    branch: SessionId,
    messages: &HashMap<uuid::Uuid, uuid::Uuid>,
    documents: &HashMap<uuid::Uuid, uuid::Uuid>,
    turn_ids: &HashMap<uuid::Uuid, uuid::Uuid>,
    point: &BranchCutoff,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let Some(row) = entities::context_checkpoint::Entity::find_by_id(source.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(());
    };
    let through = match row.through_turn_id {
        Some(through) => match turn_ids.get(&through) {
            Some(copied) => Some(*copied),
            None => return Ok(()),
        },
        None if point.includes(row.created_at) => None,
        None => return Ok(()),
    };
    let Some(source_message) = messages.get(&row.source_message_id).copied() else {
        return Ok(());
    };
    let Some(seq) = entities::message::Entity::find_by_id(source_message)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(|message| message.seq)
    else {
        return Ok(());
    };
    let content = rewrite_ids(&row.content, documents);
    let mut copy: entities::context_checkpoint::ActiveModel = row.into();
    copy.chat_id = Set(branch.0);
    copy.source_message_id = Set(source_message);
    copy.source_message_seq = Set(seq);
    copy.content = Set(content);
    copy.through_turn_id = Set(through);
    copy.insert(conn).await.map_err(store_err)?;
    Ok(())
}

/// `text` with every copied document's old id replaced by its new one.
///
/// Ids are UUIDs, so an exact textual match is that document and nothing
/// else. The model context names documents by id, and a branch has to name
/// its own copies or `read_document` would refuse them.
fn rewrite_ids(text: &str, documents: &HashMap<uuid::Uuid, uuid::Uuid>) -> String {
    let mut out = text.to_owned();
    for (old, new) in documents {
        let old = old.to_string();
        if out.contains(&old) {
            out = out.replace(&old, &new.to_string());
        }
    }
    out
}

fn rewrite_json(
    value: &serde_json::Value,
    documents: &HashMap<uuid::Uuid, uuid::Uuid>,
) -> Result<serde_json::Value> {
    if documents.is_empty() {
        return Ok(value.clone());
    }
    let text = serde_json::to_string(value)?;
    let rewritten = rewrite_ids(&text, documents);
    if rewritten == text {
        return Ok(value.clone());
    }
    Ok(serde_json::from_str(&rewritten)?)
}
