//! `m20260902_000003_one_approval_surface`: every card is one approval row.
//!
//! Decision 0048 step 5, slice D3. After this migration a consent card, a
//! questions card, and a plan proposal are each one `code_approval` row on
//! every engine, with one `approval_requested` and one `approval_resolved`
//! journal row. The chat side's approval half of `tool_call`, and the
//! `user_question_request`, `user_question`, and `plan_request` tables,
//! retire into it:
//!
//! - `code_approval` gains `auto_judge_status` (the internal engine's judge
//!   marker) and loses its foreign key to `code_turn`: the internal engine's
//!   turns are `turn_run` rows until slice D4 merges the turn lane, and a
//!   card's `turn_id` names whichever lane parked it.
//! - Every `tool_call` row with an approval becomes an approval row whose id
//!   is the call id, kind is the call's exact preview and grant ladder, and
//!   raw payload is the engine's own request (tool name, consent kind, and
//!   the standing grant that authorized it, when one did).
//! - Every question request and plan request becomes an approval row of the
//!   matching kind; the plan body rides the row's raw payload.
//! - The journal rows that named the old cards — `tool_approval_required`,
//!   `tool_approval_decided`, `questions_asked`, `plan_proposed` — are
//!   rewritten as `approval_requested` and `approval_resolved` rows carrying
//!   the same facts, and the rows the bridge's worker minted beside them on
//!   internal sessions (#3010, slice D2) are removed with the rows they
//!   described, so each card has one row and one pair of journal rows.
//! - The nine approval columns leave `tool_call` with the checks and the
//!   journal foreign key that named them; the three tables are dropped.
//!
//! Idempotent on the absence of `plan_request`, the last table dropped on
//! both backends. The SQLite branch runs autocommit steps that each skip
//! work an interrupted attempt already did.

use std::collections::HashMap;

use sea_orm::{ConnectionTrait, DbBackend};
use sea_orm_migration::prelude::*;

use crate::approval::{GrantScope, InternalToolApprovalRequest, ToolApprovalKind};
use crate::code::{
    ApprovalDecisionKind, CodeApprovalKind, CodeApprovalState, CodeEvent, CodeTurnId,
    InternalApprovalRequest, MAX_TOOL_SUMMARY_CHARS,
};
use crate::db::entities;
use crate::preview::ToolActionPreview;

pub(super) struct OneApprovalSurface;

impl MigrationName for OneApprovalSurface {
    fn name(&self) -> &str {
        "m20260902_000003_one_approval_surface"
    }
}

/// The `tool_call` columns that were the approval half.
pub(super) const RETIRED_TOOL_CALL_COLUMNS: &[&str] = &[
    "approval_status",
    "approval_class",
    "approval_kind",
    "approval_reason",
    "approval_requested_at",
    "approval_decided_at",
    "approval_event_seq",
    "approval_grant_source_call_id",
    "auto_judge_status",
];

/// The tables that retire, in the order their foreign keys allow.
pub(super) const RETIRED_TABLES: &[&str] =
    &["user_question", "user_question_request", "plan_request"];

/// How many rows one page of a backfill reads.
const BACKFILL_PAGE: usize = 1_000;

/// The journal `type` tags this migration rewrites.
const RETIRED_EVENT_TYPES: &[&str] = &[
    "tool_approval_required",
    "tool_approval_decided",
    "questions_asked",
    "plan_proposed",
];

#[async_trait::async_trait]
impl MigrationTrait for OneApprovalSurface {
    fn use_transaction(&self) -> Option<bool> {
        None
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("plan_request").await? {
            return Ok(());
        }
        match manager.get_database_backend() {
            DbBackend::Postgres => {
                let transaction = manager.begin().await?;
                let connection = transaction.get_connection();
                connection
                    .execute_unprepared(
                        r#"
ALTER TABLE "code_approval"
    ADD COLUMN "auto_judge_status" text,
    ADD CONSTRAINT "chk_code_approval_auto_judge_status"
        CHECK ("auto_judge_status" IS NULL OR "auto_judge_status" IN ('judging', 'approved', 'declined'));
DO $unbind$
DECLARE
    old_constraint text;
BEGIN
    FOR old_constraint IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        WHERE constraint_row.conrelid = 'code_approval'::regclass
          AND constraint_row.contype = 'f'
          AND constraint_row.confrelid = 'code_turn'::regclass
    LOOP
        EXECUTE format('ALTER TABLE "code_approval" DROP CONSTRAINT %I', old_constraint);
    END LOOP;
END
$unbind$;
"#,
                    )
                    .await?;
                backfill(connection).await?;
                connection
                    .execute_unprepared(&format!(
                        r#"ALTER TABLE "tool_call" {}"#,
                        RETIRED_TOOL_CALL_COLUMNS
                            .iter()
                            .map(|column| format!(r#"DROP COLUMN "{column}""#))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .await?;
                for table in RETIRED_TABLES {
                    connection
                        .execute_unprepared(&format!(r#"DROP TABLE "{table}""#))
                        .await?;
                }
                transaction.commit().await
            }
            DbBackend::Sqlite => {
                super::rebuild_sqlite_table(manager, "code_approval", one_approval_row).await?;
                let transaction = manager.begin().await?;
                backfill(transaction.get_connection()).await?;
                transaction.commit().await?;
                super::rebuild_sqlite_table(manager, "tool_call", retire_approval_columns).await?;
                // The last drop is the marker, so the tables go last and in
                // foreign-key order.
                let connection = manager.get_connection();
                for table in RETIRED_TABLES {
                    connection
                        .execute_unprepared(&format!(r#"DROP TABLE IF EXISTS "{table}""#))
                        .await?;
                }
                Ok(())
            }
            backend => Err(DbErr::Custom(format!(
                "unsupported database backend for the one-approval-surface migration: {backend:?}"
            ))),
        }
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The cards now live in `code_approval`; recreating the old tables
        // would leave every card in rows nothing reads.
        Ok(())
    }
}

/// The retired tables and the approval half of `tool_call`, as this
/// migration reads them. The live entities are gone with the columns.
mod legacy {
    pub mod tool_call {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "tool_call")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub chat_id: Uuid,
            pub turn_id: Uuid,
            pub name: String,
            #[sea_orm(column_type = "JsonBinary")]
            pub arguments: Json,
            pub approval_status: Option<String>,
            pub approval_kind: Option<String>,
            pub approval_reason: Option<String>,
            pub approval_requested_at: Option<DateTimeUtc>,
            pub approval_decided_at: Option<DateTimeUtc>,
            pub approval_grant_source_call_id: Option<Uuid>,
            pub auto_judge_status: Option<String>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod user_question_request {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "user_question_request")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub call_id: Uuid,
            pub turn_id: Uuid,
            pub chat_id: Uuid,
            pub status: String,
            pub asked_at: DateTimeUtc,
            pub resolved_at: Option<DateTimeUtc>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod user_question {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "user_question")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub call_id: Uuid,
            #[sea_orm(primary_key, auto_increment = false)]
            pub question_id: String,
            pub position: i32,
            pub header: String,
            pub prompt: String,
            #[sea_orm(column_type = "JsonBinary")]
            pub options: Json,
            pub question_type: String,
            pub allow_free_form: bool,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod plan_request {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "plan_request")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub call_id: Uuid,
            pub turn_id: Uuid,
            pub chat_id: Uuid,
            pub status: String,
            pub title: String,
            pub plan: String,
            pub feedback: Option<String>,
            pub proposed_at: DateTimeUtc,
            pub resolved_at: Option<DateTimeUtc>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }
}

/// The owner and worker epoch of the conversations the backfill mints rows
/// for, read by id as they come up: the ids arrive from the retired rows in
/// the form the live store binds, and only those rows are decoded.
struct Sessions {
    by_id: HashMap<uuid::Uuid, (String, i64)>,
}

impl Sessions {
    fn new() -> Self {
        Self {
            by_id: HashMap::new(),
        }
    }

    async fn owner_and_epoch<C>(
        &mut self,
        conn: &C,
        chat_id: uuid::Uuid,
        what: &str,
    ) -> Result<(String, i64), DbErr>
    where
        C: ConnectionTrait,
    {
        use sea_orm::{EntityTrait, QuerySelect};
        if let Some(found) = self.by_id.get(&chat_id) {
            return Ok(found.clone());
        }
        // Name the columns rather than reading the live entity: this
        // migration is historical, and the entity's model grows with the
        // schema of the chain's end, which does not exist yet here.
        let found = entities::code_session::Entity::find_by_id(chat_id)
            .select_only()
            .column(entities::code_session::Column::Owner)
            .column(entities::code_session::Column::SpawnEpoch)
            .into_tuple::<(String, i64)>()
            .one(conn)
            .await?
            .ok_or_else(|| {
                DbErr::Custom(format!(
                    "{what} names conversation {chat_id}, which does not exist"
                ))
            })?;
        self.by_id.insert(chat_id, found.clone());
        Ok(found)
    }
}

/// Move every card onto `code_approval` and rewrite the journal rows that
/// named the old cards. Runs inside the caller's transaction, and skips a
/// row an interrupted attempt already minted.
pub(super) async fn backfill<C>(conn: &C) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    let mut sessions = Sessions::new();
    remove_bridge_rows(conn).await?;
    rewrite_journal(conn).await?;
    backfill_tool_approvals(conn, &mut sessions).await?;
    backfill_questions(conn, &mut sessions).await?;
    backfill_plans(conn, &mut sessions).await?;
    Ok(())
}

/// Every internal session's approval rows before this migration were the
/// bridge worker's copies of the chat cards (#3010), and its
/// `approval_requested` / `approval_resolved` journal rows were their hints:
/// the chat lane never wrote either. Both go, so each card has one row.
async fn remove_bridge_rows<C>(conn: &C) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

    // Columns by name, not the live entity, for the same reason as above.
    let internal = entities::code_session::Entity::find()
        .select_only()
        .column(entities::code_session::Column::Id)
        .filter(entities::code_session::Column::WorkspaceId.is_null())
        .filter(entities::code_session::Column::HarnessKind.eq("internal"))
        .into_tuple::<uuid::Uuid>()
        .all(conn)
        .await?;
    if internal.is_empty() {
        return Ok(());
    }
    // A bridge row is keyed by an id of its own; a card's row is keyed by
    // its call. Rows an interrupted attempt already minted are therefore
    // kept, and only the bridge's copies go.
    let bridge_rows = entities::code_approval::Entity::find()
        .filter(entities::code_approval::Column::SessionId.is_in(internal.clone()))
        .all(conn)
        .await?;
    for row in bridge_rows {
        if !is_call(conn, row.id).await? {
            entities::code_approval::Entity::delete_by_id(row.id)
                .exec(conn)
                .await?;
        }
    }
    let mut after: Option<(uuid::Uuid, i64)> = None;
    loop {
        let mut query = entities::code_event::Entity::find()
            .filter(entities::code_event::Column::SessionId.is_in(internal.clone()))
            .order_by_asc(entities::code_event::Column::SessionId)
            .order_by_asc(entities::code_event::Column::Seq)
            .limit(BACKFILL_PAGE as u64);
        if let Some((session_id, seq)) = after {
            query = query.filter(page_after(session_id, seq));
        }
        let rows = query.all(conn).await?;
        let page = rows.len();
        for row in rows {
            after = Some((row.session_id, row.seq));
            let kind = row.event.get("type").and_then(serde_json::Value::as_str);
            if !matches!(kind, Some("approval_requested" | "approval_resolved")) {
                continue;
            }
            // A row an earlier attempt already rewrote names its card's
            // request, or its call; the bridge's hint names neither.
            if row.event.get("request").is_some() {
                continue;
            }
            let names_a_call = match row
                .event
                .get("approval_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| id.parse::<uuid::Uuid>().ok())
            {
                Some(id) => is_call(conn, id).await?,
                None => false,
            };
            if names_a_call {
                continue;
            }
            entities::code_event::Entity::delete_by_id((row.session_id, row.seq))
                .exec(conn)
                .await?;
        }
        if page < BACKFILL_PAGE {
            return Ok(());
        }
    }
}

/// Whether an id is a tool call's: the identity every card's row and
/// journal hint carry after this migration.
async fn is_call<C>(conn: &C, id: uuid::Uuid) -> Result<bool, DbErr>
where
    C: ConnectionTrait,
{
    use sea_orm::EntityTrait;
    Ok(legacy::tool_call::Entity::find_by_id(id)
        .one(conn)
        .await?
        .is_some())
}

fn page_after(session_id: uuid::Uuid, seq: i64) -> sea_orm::Condition {
    use sea_orm::{ColumnTrait, Condition};
    Condition::any()
        .add(entities::code_event::Column::SessionId.gt(session_id))
        .add(
            Condition::all()
                .add(entities::code_event::Column::SessionId.eq(session_id))
                .add(entities::code_event::Column::Seq.gt(seq)),
        )
}

/// Rewrite the four retired journal shapes into the approval rows' hints.
/// A payload the mapping cannot read fails the migration by `(session,
/// seq)`: the journal fixture is the compatibility contract for every shape
/// ever written, so an unreadable row is corruption to look at.
async fn rewrite_journal<C>(conn: &C) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    use sea_orm::{ActiveModelTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};

    let mut after: Option<(uuid::Uuid, i64)> = None;
    loop {
        let mut query = entities::code_event::Entity::find()
            .order_by_asc(entities::code_event::Column::SessionId)
            .order_by_asc(entities::code_event::Column::Seq)
            .limit(BACKFILL_PAGE as u64);
        if let Some((session_id, seq)) = after {
            query = query.filter(page_after(session_id, seq));
        }
        let rows = query.all(conn).await?;
        let page = rows.len();
        for row in rows {
            after = Some((row.session_id, row.seq));
            let Some(kind) = row.event.get("type").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if !RETIRED_EVENT_TYPES.contains(&kind) {
                continue;
            }
            let rewritten = rewrite_event(&row.event).map_err(|error| {
                DbErr::Custom(format!(
                    "event ({}, {}) does not read as a retired card event: {error}",
                    row.session_id, row.seq
                ))
            })?;
            let event = serde_json::to_value(&rewritten).map_err(|error| {
                DbErr::Custom(format!("event ({}, {}): {error}", row.session_id, row.seq))
            })?;
            entities::code_event::ActiveModel {
                session_id: Set(row.session_id),
                seq: Set(row.seq),
                event: Set(event),
                ..Default::default()
            }
            .update(conn)
            .await?;
        }
        if page < BACKFILL_PAGE {
            return Ok(());
        }
    }
}

/// One retired journal payload as the row it is now.
fn rewrite_event(payload: &serde_json::Value) -> Result<CodeEvent, String> {
    fn field<'a>(
        payload: &'a serde_json::Value,
        name: &str,
    ) -> Result<&'a serde_json::Value, String> {
        payload
            .get(name)
            .ok_or_else(|| format!("missing field {name}"))
    }
    fn parse<T: serde::de::DeserializeOwned>(
        payload: &serde_json::Value,
        name: &str,
    ) -> Result<T, String> {
        serde_json::from_value(field(payload, name)?.clone())
            .map_err(|error| format!("field {name}: {error}"))
    }
    fn parse_optional<T: serde::de::DeserializeOwned>(
        payload: &serde_json::Value,
        name: &str,
    ) -> Result<Option<T>, String> {
        match payload.get(name) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|error| format!("field {name}: {error}")),
        }
    }
    let call_id: uuid::Uuid = field(payload, "call_id")?
        .as_str()
        .ok_or_else(|| "call_id is not a string".to_owned())?
        .parse()
        .map_err(|error| format!("call_id: {error}"))?;
    let approval_id = crate::code::CodeApprovalId(call_id);
    let kind = field(payload, "type")?.as_str().unwrap_or_default();
    Ok(match kind {
        "tool_approval_required" => CodeEvent::ApprovalRequested {
            approval_id,
            request: Some(InternalApprovalRequest::ToolUse {
                auto_judging: parse_optional::<bool>(payload, "auto_judging")?.unwrap_or(false),
                tool_name: parse(payload, "tool_name")?,
                class: parse(payload, "class")?,
                approval: parse(payload, "kind")?,
                grant_scopes: parse_optional::<Vec<GrantScope>>(payload, "grant_scopes")?
                    .unwrap_or_default(),
                preview: parse_optional::<ToolActionPreview>(payload, "preview")?,
            }),
        },
        "tool_approval_decided" => CodeEvent::ApprovalResolved {
            approval_id,
            decision: if parse::<bool>(payload, "approved")? {
                ApprovalDecisionKind::Approve
            } else {
                ApprovalDecisionKind::Deny { feedback: None }
            },
        },
        "questions_asked" => CodeEvent::ApprovalRequested {
            approval_id,
            request: Some(InternalApprovalRequest::Questions {
                turn_id: parse::<CodeTurnId>(payload, "turn_id")?,
            }),
        },
        "plan_proposed" => CodeEvent::ApprovalRequested {
            approval_id,
            request: Some(InternalApprovalRequest::Plan {
                turn_id: parse::<CodeTurnId>(payload, "turn_id")?,
            }),
        },
        other => return Err(format!("unexpected type {other}")),
    })
}

/// The consent kind a `tool_call` row stored, recovered the way the retired
/// read did: the stored spelling folded several kinds into a closed column,
/// and the tool name tells them apart.
fn stored_tool_approval_kind(
    spelling: Option<&str>,
    name: &str,
) -> Result<ToolApprovalKind, DbErr> {
    Ok(match spelling {
        Some("search_may_share_query_and_excerpts") if name.starts_with("mcp__") => {
            ToolApprovalKind::ExternalMcpMayCallServer
        }
        Some("search_may_share_query_and_excerpts") if name == "web_search" => {
            ToolApprovalKind::WebSearchMayShareQuery
        }
        Some("search_may_share_query_and_excerpts") if name == "web_extract" => {
            ToolApprovalKind::WebExtractMayFetchUrl
        }
        Some("search_may_share_query_and_excerpts") => {
            ToolApprovalKind::SearchMayShareQueryAndExcerpts
        }
        Some("web_search_may_share_query") if name == "web_search" => {
            ToolApprovalKind::WebSearchMayShareQuery
        }
        Some("exec_may_run_networked_command") => ToolApprovalKind::ExecMayRunNetworkedCommand,
        Some("unsupported") => match ToolApprovalKind::for_tool_name(name) {
            kind @ (ToolApprovalKind::WorkspaceMayModifyFiles
            | ToolApprovalKind::DelegateMayRunBackgroundAgent
            | ToolApprovalKind::ComputerMayControlApp) => kind,
            _ => ToolApprovalKind::Unsupported,
        },
        other => {
            return Err(DbErr::Custom(format!(
                "tool_call names an unknown approval kind {other:?}"
            )))
        }
    })
}

async fn approval_exists<C>(conn: &C, id: uuid::Uuid) -> Result<bool, DbErr>
where
    C: ConnectionTrait,
{
    use sea_orm::EntityTrait;
    Ok(entities::code_approval::Entity::find_by_id(id)
        .one(conn)
        .await?
        .is_some())
}

/// One card as the backfill mints it: the row's identity, its conversation,
/// and what the retired row said about it.
struct CardRow<'a> {
    owner: String,
    session_id: uuid::Uuid,
    turn_id: uuid::Uuid,
    worker_epoch: i64,
    id: uuid::Uuid,
    kind: &'a CodeApprovalKind,
    raw: serde_json::Value,
    state: CodeApprovalState,
    feedback: Option<String>,
    requested_at: chrono::DateTime<chrono::Utc>,
    decided_at: Option<chrono::DateTime<chrono::Utc>>,
    auto_judge_status: Option<String>,
}

fn approval_row(card: CardRow<'_>) -> Result<entities::code_approval::ActiveModel, DbErr> {
    use sea_orm::Set;
    let id = card.id;
    Ok(entities::code_approval::ActiveModel {
        id: Set(id),
        owner: Set(card.owner),
        session_id: Set(card.session_id),
        turn_id: Set(card.turn_id),
        kind: Set(serde_json::to_value(card.kind)
            .map_err(|error| DbErr::Custom(format!("approval {id} kind: {error}")))?),
        harness_raw: Set(card.raw),
        native_call_id: Set(Some(id.to_string())),
        server_capability: Set(None),
        request_sha256: Set(None),
        worker_epoch: Set(Some(card.worker_epoch)),
        decision_claim: Set(None),
        claimed_at: Set(None),
        state: Set(card.state.as_str().to_owned()),
        feedback: Set(card.feedback),
        requested_at: Set(card.requested_at),
        decided_at: Set(card.decided_at),
        auto_judge_status: Set(card.auto_judge_status),
    })
}

async fn backfill_tool_approvals<C>(conn: &C, sessions: &mut Sessions) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    };

    let mut after: Option<uuid::Uuid> = None;
    loop {
        let mut query = legacy::tool_call::Entity::find()
            .filter(legacy::tool_call::Column::ApprovalStatus.is_not_null())
            .order_by_asc(legacy::tool_call::Column::Id)
            .limit(BACKFILL_PAGE as u64);
        if let Some(id) = after {
            query = query.filter(legacy::tool_call::Column::Id.gt(id));
        }
        let rows = query.all(conn).await?;
        let page = rows.len();
        for call in rows {
            after = Some(call.id);
            if approval_exists(conn, call.id).await? {
                continue;
            }
            let (owner, epoch) = sessions
                .owner_and_epoch(conn, call.chat_id, "tool_call")
                .await?;
            let kind = stored_tool_approval_kind(call.approval_kind.as_deref(), &call.name)?;
            let state = match call.approval_status.as_deref() {
                Some("pending") => CodeApprovalState::Pending,
                Some("approved") => CodeApprovalState::Approved,
                Some("rejected") => CodeApprovalState::Denied,
                other => {
                    return Err(DbErr::Custom(format!(
                        "tool_call {} has an unknown approval status {other:?}",
                        call.id
                    )))
                }
            };
            let requested_at = call.approval_requested_at.ok_or_else(|| {
                DbErr::Custom(format!(
                    "tool_call {} approval has no requested_at",
                    call.id
                ))
            })?;
            let row_kind = match ToolActionPreview::build(&call.name, &call.arguments) {
                Some(preview) => CodeApprovalKind::ToolUse {
                    preview: preview.without_summary(),
                    offered_grants: GrantScope::mintable_ladder_for(
                        kind,
                        &call.name,
                        &call.arguments,
                    ),
                },
                None => CodeApprovalKind::Other {
                    summary: crate::chat_journal::bounded(&call.name, MAX_TOOL_SUMMARY_CHARS),
                },
            };
            let raw = InternalToolApprovalRequest {
                tool_name: call.name.clone(),
                kind,
                granted_by: call.approval_grant_source_call_id.map(crate::CallId),
            }
            .to_raw()
            .map_err(|error| DbErr::Custom(format!("tool_call {}: {error}", call.id)))?;
            approval_row(CardRow {
                owner,
                session_id: call.chat_id,
                turn_id: call.turn_id,
                worker_epoch: epoch,
                id: call.id,
                kind: &row_kind,
                raw,
                state,
                feedback: call.approval_reason.clone(),
                requested_at,
                decided_at: call.approval_decided_at,
                auto_judge_status: call.auto_judge_status.clone(),
            })?
            .insert(conn)
            .await?;
        }
        if page < BACKFILL_PAGE {
            return Ok(());
        }
    }
}

async fn backfill_questions<C>(conn: &C, sessions: &mut Sessions) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    };

    let mut after: Option<uuid::Uuid> = None;
    loop {
        let mut query = legacy::user_question_request::Entity::find()
            .order_by_asc(legacy::user_question_request::Column::CallId)
            .limit(BACKFILL_PAGE as u64);
        if let Some(id) = after {
            query = query.filter(legacy::user_question_request::Column::CallId.gt(id));
        }
        let rows = query.all(conn).await?;
        let page = rows.len();
        for request in rows {
            after = Some(request.call_id);
            if approval_exists(conn, request.call_id).await? {
                continue;
            }
            let (owner, epoch) = sessions
                .owner_and_epoch(conn, request.chat_id, "user_question_request")
                .await?;
            let questions = legacy::user_question::Entity::find()
                .filter(legacy::user_question::Column::CallId.eq(request.call_id))
                .order_by_asc(legacy::user_question::Column::Position)
                .all(conn)
                .await?
                .into_iter()
                .map(|question| {
                    Ok(crate::UserQuestion {
                        id: question.question_id,
                        header: question.header,
                        question: question.prompt,
                        options: serde_json::from_value(question.options).map_err(|error| {
                            DbErr::Custom(format!(
                                "user_question {} options: {error}",
                                request.call_id
                            ))
                        })?,
                        question_type: match question.question_type.as_str() {
                            "single_select" => crate::UserQuestionType::SingleSelect,
                            "multi_select" => crate::UserQuestionType::MultiSelect,
                            other => {
                                return Err(DbErr::Custom(format!(
                                    "user_question {} has an unknown type {other}",
                                    request.call_id
                                )))
                            }
                        },
                        allow_free_form: question.allow_free_form,
                    })
                })
                .collect::<Result<Vec<_>, DbErr>>()?;
            let state = match request.status.as_str() {
                "pending" => CodeApprovalState::Pending,
                "answered" => CodeApprovalState::Approved,
                "cancelled" => CodeApprovalState::Abandoned,
                other => {
                    return Err(DbErr::Custom(format!(
                        "user_question_request {} has an unknown status {other}",
                        request.call_id
                    )))
                }
            };
            approval_row(CardRow {
                owner,
                session_id: request.chat_id,
                turn_id: request.turn_id,
                worker_epoch: epoch,
                id: request.call_id,
                kind: &CodeApprovalKind::Questions { questions },
                raw: serde_json::Value::Null,
                state,
                feedback: None,
                requested_at: request.asked_at,
                decided_at: request.resolved_at,
                auto_judge_status: None,
            })?
            .insert(conn)
            .await?;
        }
        if page < BACKFILL_PAGE {
            return Ok(());
        }
    }
}

async fn backfill_plans<C>(conn: &C, sessions: &mut Sessions) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    };

    let mut after: Option<uuid::Uuid> = None;
    loop {
        let mut query = legacy::plan_request::Entity::find()
            .order_by_asc(legacy::plan_request::Column::CallId)
            .limit(BACKFILL_PAGE as u64);
        if let Some(id) = after {
            query = query.filter(legacy::plan_request::Column::CallId.gt(id));
        }
        let rows = query.all(conn).await?;
        let page = rows.len();
        for request in rows {
            after = Some(request.call_id);
            if approval_exists(conn, request.call_id).await? {
                continue;
            }
            let (owner, epoch) = sessions
                .owner_and_epoch(conn, request.chat_id, "plan_request")
                .await?;
            let state = match request.status.as_str() {
                "pending" => CodeApprovalState::Pending,
                "accepted" => CodeApprovalState::Approved,
                "rejected" => CodeApprovalState::Denied,
                "cancelled" => CodeApprovalState::Abandoned,
                other => {
                    return Err(DbErr::Custom(format!(
                        "plan_request {} has an unknown status {other}",
                        request.call_id
                    )))
                }
            };
            let raw = crate::PlanProposalBody {
                title: request.title.clone(),
                plan: request.plan.clone(),
            }
            .to_raw()
            .map_err(|error| DbErr::Custom(format!("plan_request {}: {error}", request.call_id)))?;
            approval_row(CardRow {
                owner,
                session_id: request.chat_id,
                turn_id: request.turn_id,
                worker_epoch: epoch,
                id: request.call_id,
                kind: &CodeApprovalKind::Plan {
                    proposed_mode: crate::DEFAULT_ACCEPTED_PLAN_MODE,
                },
                raw,
                state,
                feedback: request.feedback.clone(),
                requested_at: request.proposed_at,
                decided_at: request.resolved_at,
                auto_judge_status: None,
            })?
            .insert(conn)
            .await?;
        }
        if page < BACKFILL_PAGE {
            return Ok(());
        }
    }
}

/// Split a stored `CREATE TABLE` body into its top-level items — columns,
/// constraints — and the text around them.
fn table_items(create: &str) -> Result<(String, Vec<String>, String), DbErr> {
    let open = create
        .find('(')
        .ok_or_else(|| DbErr::Custom(format!("SQLite definition has no body: {create}")))?;
    let close = create
        .rfind(')')
        .ok_or_else(|| DbErr::Custom(format!("SQLite definition has no body: {create}")))?;
    let head = create[..=open].to_owned();
    let tail = create[close..].to_owned();
    let body = &create[open + 1..close];
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut current = String::new();
    for character in body.chars() {
        match character {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                items.push(current.trim().to_owned());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.trim().is_empty() {
        items.push(current.trim().to_owned());
    }
    Ok((head, items, tail))
}

fn join_items(head: &str, items: &[String], tail: &str) -> String {
    format!("{head}\n    {}\n{tail}", items.join(",\n    "))
}

/// Drop the approval half from a stored SQLite `tool_call` definition:
/// the nine columns, the journal foreign key that named
/// `approval_event_seq`, and every check that mentions them. A definition
/// without them comes back unchanged.
pub(super) fn retire_approval_columns(create: &str) -> Result<String, DbErr> {
    let (head, items, tail) = table_items(create)?;
    let names_retired = |item: &str| {
        RETIRED_TOOL_CALL_COLUMNS
            .iter()
            .any(|column| item.contains(&format!("\"{column}\"")))
    };
    let kept = items
        .iter()
        .filter(|item| !names_retired(item))
        .cloned()
        .collect::<Vec<_>>();
    if kept.len() == items.len() {
        return Ok(create.to_owned());
    }
    Ok(join_items(&head, &kept, &tail))
}

/// Give a stored SQLite `code_approval` definition the judge marker and
/// free its `turn_id` from `code_turn`. A definition that already carries
/// the marker comes back unchanged.
pub(super) fn one_approval_row(create: &str) -> Result<String, DbErr> {
    if create.contains(r#""auto_judge_status""#) {
        return Ok(create.to_owned());
    }
    let (head, items, tail) = table_items(create)?;
    let mut kept = items
        .iter()
        .filter(|item| !item.contains(r#"REFERENCES "code_turn""#))
        .cloned()
        .collect::<Vec<_>>();
    let last_column = kept
        .iter()
        .rposition(|item| item.starts_with('"'))
        .ok_or_else(|| DbErr::Custom(format!("SQLite code_approval has no columns: {create}")))?;
    kept.insert(last_column + 1, r#""auto_judge_status" text"#.to_owned());
    kept.push(
        r#"CHECK ("auto_judge_status" IS NULL OR "auto_judge_status" IN ('judging', 'approved', 'declined'))"#
            .to_owned(),
    );
    Ok(join_items(&head, &kept, &tail))
}
