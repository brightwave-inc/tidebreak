//! Session access rows, visibility, and the read that resolves them.
//!
//! Decision 0086. A session keeps one owner, who stays its execution identity
//! and its lifecycle authority. Everything else a second person may do comes
//! from a `session_access` row, from `deployment` visibility, or — for a
//! private, externally bound direct child — from the parent's live access
//! under a shared live grant. This module is the only place that decides
//! which. Children keep their own visibility and access rows; inheritance
//! is computed, never copied.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
    TryIntoModel,
};

use crate::code::{CodeGrantId, Session, SessionAccessLevel, SessionId, SessionVisibility};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::session::{code_runtime_sessions, session_from_row};

/// Longest subject a grant may name. A subject is a principal key or a
/// channel identity, both of which are short; the bound is a guard against a
/// route storing something else.
const MAX_SUBJECT_CHARS: usize = 512;

/// One row of a session's access list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAccess {
    pub session_id: SessionId,
    /// `principal:<owner key>` or `external:<channel kind>:<user id>`.
    pub subject: String,
    pub level: SessionAccessLevel,
    /// The owner who granted it. Only an owner may.
    pub granted_by: OwnerId,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// What one principal may do with one session.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSessionAccess {
    pub session: Session,
    /// The strongest level this principal holds. An owner reads as
    /// `Contribute`; `owner` says whether they may do more than that.
    pub level: SessionAccessLevel,
    /// Whether this principal owns the session, and so holds its lifecycle
    /// authority.
    pub owner: bool,
}

/// The subject that names one principal.
fn principal_subject(owner: &OwnerId) -> String {
    format!("principal:{}", owner.as_str())
}

/// The subject that names one channel identity.
fn external_subject(channel_kind: &str, external_identity: &str) -> String {
    format!("external:{channel_kind}:{external_identity}")
}

/// Every subject a principal answers to right now.
///
/// Their own principal subject, plus one external subject per live grant they
/// hold. A revoked grant is left out, which is what makes an external row
/// stop resolving without the row changing.
async fn subjects_for(store: &DbStore, principal: &OwnerId) -> Result<Vec<String>> {
    let mut subjects = vec![principal_subject(principal)];
    let grants = entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::Owner.eq(principal.as_str()))
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    subjects.extend(
        grants
            .into_iter()
            .map(|grant| external_subject(&grant.channel_kind, &grant.external_identity)),
    );
    Ok(subjects)
}

/// What this principal may do with this session, or `None` when the session
/// does not exist or is not theirs to see.
///
/// A caller cannot tell those two apart, which is the point: a session the
/// principal holds no claim on answers exactly as a session that never
/// existed.
pub async fn resolve_session_access(
    store: &DbStore,
    principal: &OwnerId,
    id: SessionId,
) -> Result<Option<ResolvedSessionAccess>> {
    let Some(row) = entities::session::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if row.owner == principal.as_str() {
        return Ok(Some(ResolvedSessionAccess {
            session: session_from_row(row)?,
            level: SessionAccessLevel::Contribute,
            owner: true,
        }));
    }

    let visible = SessionVisibility::from_token(&row.visibility).ok_or_else(|| {
        AgentError::Store(format!(
            "session {} has unknown visibility {}",
            row.id, row.visibility
        ))
    })? == SessionVisibility::Deployment;
    let mut level = visible.then_some(SessionAccessLevel::View);

    let rows = entities::session_access::Entity::find()
        .filter(entities::session_access::Column::SessionId.eq(id.0))
        .filter(
            entities::session_access::Column::Subject.is_in(subjects_for(store, principal).await?),
        )
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    for access in rows {
        let candidate = access_level(&access.level)?;
        if candidate == SessionAccessLevel::Contribute {
            level = Some(candidate);
            break;
        }
        level.get_or_insert(candidate);
    }

    if level.is_none() {
        level = inherit_bound_child_level(store, principal, &row).await?;
    }
    let Some(level) = level else {
        return Ok(None);
    };
    Ok(Some(ResolvedSessionAccess {
        session: session_from_row(row)?,
        level,
        owner: false,
    }))
}

/// Direct child's access taken from its parent, when the tree is one
/// externally bound conversation.
///
/// Same owner, a stored parent link, and one shared live grant on both
/// sessions. Mixed bindings refuse so a second connection cannot widen the
/// first. Parent `deployment` visibility is not inherited: only the parent's
/// access rows apply, at their live level, so a revoke or a fenced grant
/// drops the child without a copied row going stale.
async fn inherit_bound_child_level(
    store: &DbStore,
    principal: &OwnerId,
    child: &entities::session::Model,
) -> Result<Option<SessionAccessLevel>> {
    let Some(parent) = bound_inherit_parent(store, child).await? else {
        return Ok(None);
    };
    access_level_for_subjects(store, parent.id, &subjects_for(store, principal).await?).await
}

async fn bound_inherit_parent(
    store: &DbStore,
    child: &entities::session::Model,
) -> Result<Option<entities::session::Model>> {
    let Some(context) = entities::code_session_context::Entity::find_by_id(child.id)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let Some(parent_id) = context.parent_session_id else {
        return Ok(None);
    };
    let Some(parent) = entities::session::Entity::find_by_id(parent_id)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if parent.owner != child.owner {
        return Ok(None);
    }
    let Some(parent_grant) = sole_live_grant_id(store, &parent.owner, parent.id).await? else {
        return Ok(None);
    };
    let Some(child_grant) = sole_live_grant_id(store, &child.owner, child.id).await? else {
        return Ok(None);
    };
    if parent_grant != child_grant {
        return Ok(None);
    }
    Ok(Some(parent))
}

/// The one live grant on a session, or `None` when it has none, several, or
/// a revoked connection.
async fn sole_live_grant_id(
    store: &DbStore,
    owner: &str,
    session_id: uuid::Uuid,
) -> Result<Option<CodeGrantId>> {
    let bindings = entities::code_external_binding::Entity::find()
        .filter(entities::code_external_binding::Column::Owner.eq(owner))
        .filter(entities::code_external_binding::Column::SessionId.eq(session_id))
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let Some(first) = bindings.first() else {
        return Ok(None);
    };
    if bindings
        .iter()
        .any(|binding| binding.grant_id != first.grant_id)
    {
        return Ok(None);
    }
    let Some(grant) = entities::code_external_grant::Entity::find_by_id(first.grant_id)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if grant.owner != owner || grant.revoked_at.is_some() {
        return Ok(None);
    }
    Ok(Some(CodeGrantId(first.grant_id)))
}

async fn access_level_for_subjects(
    store: &DbStore,
    session_id: uuid::Uuid,
    subjects: &[String],
) -> Result<Option<SessionAccessLevel>> {
    if subjects.is_empty() {
        return Ok(None);
    }
    let rows = entities::session_access::Entity::find()
        .filter(entities::session_access::Column::SessionId.eq(session_id))
        .filter(entities::session_access::Column::Subject.is_in(subjects.to_vec()))
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut level = None;
    for access in rows {
        let candidate = access_level(&access.level)?;
        if candidate == SessionAccessLevel::Contribute {
            return Ok(Some(candidate));
        }
        level.get_or_insert(candidate);
    }
    Ok(level)
}

/// Every session this principal may read, newest first.
///
/// One query rather than a resolve per session: their own, every
/// `deployment` session, and every session a subject they answer to holds a
/// row on. Externally bound direct children of those granted sessions are
/// included when inheritance would resolve them; `deployment` parents do
/// not list private children.
pub async fn list_accessible_sessions(
    store: &DbStore,
    principal: &OwnerId,
) -> Result<Vec<Session>> {
    let subjects = subjects_for(store, principal).await?;
    let granted = sea_orm::sea_query::Query::select()
        .column(entities::session_access::Column::SessionId)
        .from(entities::session_access::Entity)
        .and_where(entities::session_access::Column::Subject.is_in(subjects.clone()))
        .to_owned();
    let mut sessions: Vec<Session> = entities::session::Entity::find()
        .filter(code_runtime_sessions())
        .filter(
            Condition::any()
                .add(entities::session::Column::Owner.eq(principal.as_str()))
                .add(
                    entities::session::Column::Visibility
                        .eq(SessionVisibility::Deployment.as_str()),
                )
                .add(entities::session::Column::Id.in_subquery(granted)),
        )
        .order_by_desc(entities::session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect::<Result<Vec<_>>>()?;
    let mut seen: std::collections::HashSet<uuid::Uuid> =
        sessions.iter().map(|session| session.id.0).collect();
    let granted_parents = entities::session_access::Entity::find()
        .filter(entities::session_access::Column::Subject.is_in(subjects))
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let parent_ids: Vec<uuid::Uuid> = granted_parents
        .into_iter()
        .map(|row| row.session_id)
        .collect();
    if !parent_ids.is_empty() {
        let contexts = entities::code_session_context::Entity::find()
            .filter(entities::code_session_context::Column::ParentSessionId.is_in(parent_ids))
            .all(&store.conn)
            .await
            .map_err(store_err)?;
        for context in contexts {
            if seen.contains(&context.session_id) {
                continue;
            }
            let Some(row) = entities::session::Entity::find_by_id(context.session_id)
                .filter(code_runtime_sessions())
                .one(&store.conn)
                .await
                .map_err(store_err)?
            else {
                continue;
            };
            if inherit_bound_child_level(store, principal, &row)
                .await?
                .is_none()
            {
                continue;
            }
            seen.insert(row.id);
            sessions.push(session_from_row(row)?);
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.created_at));
    }
    Ok(sessions)
}

/// Every principal that reads this session right now: its owner, plus each
/// principal an access row resolves for.
///
/// A system path, not a request path: it answers about the whole machine
/// because the live fan-out has to address a notice to people who are not the
/// caller. Nothing reachable from a route may call it.
///
/// The fan-out asks this so a digest reaches the people the session was
/// shared with, not only its owner. A `principal:` subject resolves directly.
/// An `external:` subject resolves through each live grant that binds a
/// principal to that channel identity, which is the same rule
/// [`resolve_session_access`] applies per caller — a fenced grant drops the
/// principal from this list without the row changing.
///
/// `deployment` visibility is not expanded here. It admits any authenticated
/// principal, which is not a set this store can enumerate; the fan-out reaches
/// those readers through the sockets they hold open.
pub async fn session_readers_all_owners(store: &DbStore, id: SessionId) -> Result<Vec<OwnerId>> {
    let Some(session) = entities::session::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(Vec::new());
    };
    let mut principals = vec![OwnerId::new(&session.owner)?];
    let rows = entities::session_access::Entity::find()
        .filter(entities::session_access::Column::SessionId.eq(id.0))
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let grants = entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    extend_readers_from_access_rows(&mut principals, &rows, &grants)?;
    if let Some(parent) = bound_inherit_parent(store, &session).await? {
        let parent_rows = entities::session_access::Entity::find()
            .filter(entities::session_access::Column::SessionId.eq(parent.id))
            .all(&store.conn)
            .await
            .map_err(store_err)?;
        extend_readers_from_access_rows(&mut principals, &parent_rows, &grants)?;
    }
    Ok(principals)
}

fn extend_readers_from_access_rows(
    principals: &mut Vec<OwnerId>,
    rows: &[entities::session_access::Model],
    grants: &[entities::code_external_grant::Model],
) -> Result<()> {
    for row in rows {
        if let Some(key) = row.subject.strip_prefix("principal:") {
            let principal = OwnerId::new(key)?;
            if !principals.contains(&principal) {
                principals.push(principal);
            }
            continue;
        }
        for grant in grants {
            if external_subject(&grant.channel_kind, &grant.external_identity) != row.subject {
                continue;
            }
            let principal = OwnerId::new(&grant.owner)?;
            if !principals.contains(&principal) {
                principals.push(principal);
            }
        }
    }
    Ok(())
}

/// One session's access list, oldest grant first. Owner-only: a caller who
/// does not own the session gets an empty list, never another owner's roster.
pub async fn list_session_access(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
) -> Result<Vec<SessionAccess>> {
    if !owns_session(store, owner, id).await? {
        return Ok(Vec::new());
    }
    entities::session_access::Entity::find()
        .filter(entities::session_access::Column::SessionId.eq(id.0))
        .order_by_asc(entities::session_access::Column::CreatedAt)
        .order_by_asc(entities::session_access::Column::Subject)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(access_from_row)
        .collect()
}

/// Add or raise one subject's access. `None` when the session is not this
/// owner's.
///
/// Granting a subject that already holds a row rewrites its level, so the
/// route is idempotent and a level change does not need a revoke first.
pub async fn grant_session_access(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
    subject: &str,
    level: SessionAccessLevel,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<SessionAccess>> {
    if !valid_access_subject(subject) {
        return Err(AgentError::InvalidRequest(format!(
            "a session access subject is `principal:<key>` or \
             `external:<channel kind>:<id>`, at most {MAX_SUBJECT_CHARS} characters"
        )));
    }
    if !owns_session(store, owner, id).await? {
        return Ok(None);
    }
    let model = entities::session_access::ActiveModel {
        session_id: Set(id.0),
        subject: Set(subject.to_owned()),
        level: Set(level.as_str().to_owned()),
        granted_by: Set(owner.as_str().to_owned()),
        created_at: Set(now),
    };
    entities::session_access::Entity::insert(model.clone())
        .on_conflict(
            OnConflict::columns([
                entities::session_access::Column::SessionId,
                entities::session_access::Column::Subject,
            ])
            .update_columns([
                entities::session_access::Column::Level,
                entities::session_access::Column::GrantedBy,
            ])
            .to_owned(),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    let row = entities::session_access::Entity::find_by_id((id.0, subject.to_owned()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("session access disappeared after grant".into()))?;
    Ok(Some(access_from_row(row)?))
}

/// Replace every `external:<channel_kind>:` contribute row on the session
/// with the identities the adapter sent. Other subjects are left alone.
pub async fn replace_external_session_contributors(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
    channel_kind: &str,
    identities: &[String],
    visibility: Option<SessionVisibility>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Vec<SessionAccess>>> {
    if !owns_session(store, owner, id).await? {
        return Ok(None);
    }
    let prefix = format!("external:{channel_kind}:");
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let existing = entities::session_access::Entity::find()
        .filter(entities::session_access::Column::SessionId.eq(id.0))
        .all(&transaction)
        .await
        .map_err(store_err)?;
    for row in existing {
        if row.subject.starts_with(&prefix) {
            entities::session_access::Entity::delete_by_id((id.0, row.subject))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
        }
    }
    let mut kept = Vec::new();
    for identity in dedupe_identities(identities) {
        let subject = external_subject(channel_kind, identity);
        if !valid_access_subject(&subject) {
            return Err(AgentError::InvalidRequest(format!(
                "a session access subject is `principal:<key>` or \
                 `external:<channel kind>:<id>`, at most {MAX_SUBJECT_CHARS} characters"
            )));
        }
        let model = entities::session_access::ActiveModel {
            session_id: Set(id.0),
            subject: Set(subject),
            level: Set(SessionAccessLevel::Contribute.as_str().to_owned()),
            granted_by: Set(owner.as_str().to_owned()),
            created_at: Set(now),
        };
        entities::session_access::Entity::insert(model.clone())
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        kept.push(access_from_row(model.try_into_model().map_err(store_err)?)?);
    }
    if let Some(visibility) = visibility {
        entities::session::Entity::update_many()
            .col_expr(
                entities::session::Column::Visibility,
                Expr::value(visibility.as_str()),
            )
            .filter(entities::session::Column::Id.eq(id.0))
            .filter(entities::session::Column::Owner.eq(owner.as_str()))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(kept))
}

/// Drop one subject's access. `false` when the session is not this owner's,
/// or when the subject held no row.
pub async fn revoke_session_access(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
    subject: &str,
) -> Result<bool> {
    if !owns_session(store, owner, id).await? {
        return Ok(false);
    }
    Ok(
        entities::session_access::Entity::delete_by_id((id.0, subject.to_owned()))
            .exec(&store.conn)
            .await
            .map_err(store_err)?
            .rows_affected
            == 1,
    )
}

/// Set who may discover the session without a row. `None` when the session is
/// not this owner's.
pub async fn set_session_visibility(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
    visibility: SessionVisibility,
) -> Result<Option<Session>> {
    let updated = entities::session::Entity::update_many()
        .col_expr(
            entities::session::Column::Visibility,
            Expr::value(visibility.as_str()),
        )
        .filter(entities::session::Column::Id.eq(id.0))
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Ok(None);
    }
    super::get_session(store, owner, id).await
}

async fn owns_session(store: &DbStore, owner: &OwnerId, id: SessionId) -> Result<bool> {
    Ok(entities::session::Entity::find_by_id(id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some())
}

/// Whether a string is a subject this schema accepts. The route checks it
/// too, so a client sees a bad request rather than a store failure.
pub fn valid_access_subject(subject: &str) -> bool {
    if subject.chars().count() > MAX_SUBJECT_CHARS {
        return false;
    }
    if let Some(key) = subject.strip_prefix("principal:") {
        return OwnerId::new(key).is_ok();
    }
    subject.strip_prefix("external:").is_some_and(|value| {
        value
            .split_once(':')
            .is_some_and(|(kind, identity)| !kind.is_empty() && !identity.is_empty())
    })
}

fn dedupe_identities(identities: &[String]) -> Vec<&String> {
    let mut seen = std::collections::HashSet::new();
    identities
        .iter()
        .filter(|identity| seen.insert(identity.as_str()))
        .collect()
}

fn access_level(value: &str) -> Result<SessionAccessLevel> {
    SessionAccessLevel::from_token(value)
        .ok_or_else(|| AgentError::Store(format!("session access row has unknown level {value}")))
}

fn access_from_row(row: entities::session_access::Model) -> Result<SessionAccess> {
    Ok(SessionAccess {
        session_id: SessionId(row.session_id),
        subject: row.subject,
        level: access_level(&row.level)?,
        granted_by: OwnerId::new(&row.granted_by)?,
        created_at: row.created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::dedupe_identities;
    use crate::attention::{Attention, AttentionSource};
    use crate::code::{
        CodeGrantKind, ExecutionLocation, HarnessKind, Session, SessionAccessLevel, SessionKind,
        SessionLifecycle,
    };
    use crate::db::code::{
        bind_external_session, grant_session_access, insert_session, list_accessible_sessions,
        mint_external_grant, resolve_session_access, revoke_external_grant, revoke_session_access,
        session_readers_all_owners, set_session_context, set_session_visibility, MintGrantSubject,
    };
    use crate::{DbStore, OwnerId, PermissionMode, SessionVisibility};

    #[test]
    fn duplicate_external_identities_are_kept_once_in_input_order() {
        let identities = vec!["alice".to_owned(), "bob".to_owned(), "alice".to_owned()];
        let kept: Vec<_> = dedupe_identities(&identities)
            .into_iter()
            .map(String::as_str)
            .collect();
        assert_eq!(kept, ["alice", "bob"]);
    }

    fn session(owner: &OwnerId) -> Session {
        Session {
            visibility: SessionVisibility::Private,
            id: crate::SessionId::new(),
            owner: owner.clone(),
            owner_kind: None,
            workspace_id: None,
            kind: SessionKind::Interactive,
            harness_kind: HarnessKind::Internal,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: PermissionMode::Allow,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: SessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: chrono::Utc::now(),
            execution_location: ExecutionLocation::Machine,
            acts_as: None,
        }
    }

    async fn store() -> (tempfile::TempDir, DbStore) {
        let dir = tempfile::tempdir().unwrap();
        let db = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("session-access.db").display()
        ))
        .await
        .unwrap();
        (dir, db)
    }

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    async fn bound_tree(
        db: &DbStore,
        owner: &OwnerId,
        identity: &str,
    ) -> (Session, Session, crate::code::CodeGrantId) {
        let parent = session(owner);
        let child = session(owner);
        insert_session(db, &parent).await.unwrap();
        insert_session(db, &child).await.unwrap();
        set_session_context(db, owner, child.id, None, Some(parent.id), Some("one"))
            .await
            .unwrap();
        let grant = mint_external_grant(
            db,
            owner,
            MintGrantSubject {
                channel_kind: "slack",
                external_identity: identity,
                workspace_identity: "W",
                kind: CodeGrantKind::Person,
            },
            HASH_A,
            HASH_B,
        )
        .await
        .unwrap();
        bind_external_session(db, owner, grant.id, "slack", "W/C/1", parent.id)
            .await
            .unwrap();
        bind_external_session(
            db,
            owner,
            grant.id,
            "slack",
            &format!("child/{}/one", parent.id),
            child.id,
        )
        .await
        .unwrap();
        (parent, child, grant.id)
    }

    #[tokio::test]
    async fn bound_children_inherit_parent_access_without_copied_rows() {
        let (_dir, db) = store().await;
        let owner = OwnerId::new("user:alice").unwrap();
        let reader = OwnerId::new("user:bob").unwrap();
        let (parent, child, _) = bound_tree(&db, &owner, "U-alice").await;

        assert!(resolve_session_access(&db, &reader, child.id)
            .await
            .unwrap()
            .is_none());
        grant_session_access(
            &db,
            &owner,
            parent.id,
            "principal:user:bob",
            SessionAccessLevel::View,
            chrono::Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();

        let parent_access = resolve_session_access(&db, &reader, parent.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!parent_access.owner);
        assert_eq!(parent_access.level, SessionAccessLevel::View);
        let child_access = resolve_session_access(&db, &reader, child.id)
            .await
            .unwrap()
            .expect("a parent viewer must read the bound child");
        assert!(!child_access.owner);
        assert_eq!(child_access.level, SessionAccessLevel::View);
        assert_eq!(
            list_session_access_len(&db, &owner, child.id).await,
            0,
            "inheritance must not copy access rows onto the child"
        );
        let listed = list_accessible_sessions(&db, &reader).await.unwrap();
        assert!(listed.iter().any(|session| session.id == child.id));
        let readers = session_readers_all_owners(&db, child.id).await.unwrap();
        assert!(readers.contains(&reader));

        grant_session_access(
            &db,
            &owner,
            parent.id,
            "principal:user:bob",
            SessionAccessLevel::Contribute,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let contributed = resolve_session_access(&db, &reader, child.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(contributed.level, SessionAccessLevel::Contribute);
        assert!(!contributed.owner);
        let owner_access = resolve_session_access(&db, &owner, child.id)
            .await
            .unwrap()
            .unwrap();
        assert!(owner_access.owner);
    }

    async fn list_session_access_len(db: &DbStore, owner: &OwnerId, id: crate::SessionId) -> usize {
        crate::db::code::list_session_access(db, owner, id)
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn parent_revoke_and_privacy_drop_bound_children() {
        let (_dir, db) = store().await;
        let owner = OwnerId::new("user:alice").unwrap();
        let reader = OwnerId::new("user:bob").unwrap();
        let (parent, child, grant_id) = bound_tree(&db, &owner, "U-alice").await;
        grant_session_access(
            &db,
            &owner,
            parent.id,
            "principal:user:bob",
            SessionAccessLevel::Contribute,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        set_session_visibility(&db, &owner, parent.id, SessionVisibility::Deployment)
            .await
            .unwrap();
        set_session_visibility(&db, &owner, parent.id, SessionVisibility::Private)
            .await
            .unwrap();
        assert!(
            resolve_session_access(&db, &reader, child.id)
                .await
                .unwrap()
                .is_some(),
            "access rows still inherit after a visibility downgrade"
        );

        revoke_session_access(&db, &owner, parent.id, "principal:user:bob")
            .await
            .unwrap();
        assert!(resolve_session_access(&db, &reader, child.id)
            .await
            .unwrap()
            .is_none());
        assert!(!list_accessible_sessions(&db, &reader)
            .await
            .unwrap()
            .iter()
            .any(|session| session.id == child.id));

        grant_session_access(
            &db,
            &owner,
            parent.id,
            "principal:user:bob",
            SessionAccessLevel::View,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        revoke_external_grant(&db, &owner, grant_id, "disconnected")
            .await
            .unwrap();
        assert!(
            resolve_session_access(&db, &reader, child.id)
                .await
                .unwrap()
                .is_none(),
            "a revoked connection drops inherited child access"
        );
        assert!(
            resolve_session_access(&db, &reader, parent.id)
                .await
                .unwrap()
                .is_some(),
            "a principal row on the parent still resolves after the grant is fenced"
        );
    }

    #[tokio::test]
    async fn deployment_visibility_and_unrelated_sessions_do_not_widen() {
        let (_dir, db) = store().await;
        let owner = OwnerId::new("user:alice").unwrap();
        let stranger = OwnerId::new("user:carol").unwrap();
        let other = OwnerId::new("user:dana").unwrap();
        let (parent, child, _) = bound_tree(&db, &owner, "U-alice").await;
        set_session_visibility(&db, &owner, parent.id, SessionVisibility::Deployment)
            .await
            .unwrap();
        assert!(resolve_session_access(&db, &stranger, parent.id)
            .await
            .unwrap()
            .is_some());
        assert!(
            resolve_session_access(&db, &stranger, child.id)
                .await
                .unwrap()
                .is_none(),
            "deployment on the parent must not open private children"
        );

        let private = session(&owner);
        insert_session(&db, &private).await.unwrap();
        grant_session_access(
            &db,
            &owner,
            parent.id,
            "principal:user:carol",
            SessionAccessLevel::View,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        assert!(resolve_session_access(&db, &stranger, private.id)
            .await
            .unwrap()
            .is_none());

        let foreign_parent = session(&other);
        let foreign_child = session(&other);
        insert_session(&db, &foreign_parent).await.unwrap();
        insert_session(&db, &foreign_child).await.unwrap();
        assert!(resolve_session_access(&db, &stranger, foreign_child.id)
            .await
            .unwrap()
            .is_none());
        assert!(resolve_session_access(&db, &owner, foreign_child.id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn mixed_bindings_do_not_inherit() {
        let (_dir, db) = store().await;
        let owner = OwnerId::new("user:alice").unwrap();
        let reader = OwnerId::new("user:bob").unwrap();
        let parent = session(&owner);
        let child = session(&owner);
        insert_session(&db, &parent).await.unwrap();
        insert_session(&db, &child).await.unwrap();
        set_session_context(&db, &owner, child.id, None, Some(parent.id), Some("one"))
            .await
            .unwrap();
        let first = mint_external_grant(
            &db,
            &owner,
            MintGrantSubject {
                channel_kind: "slack",
                external_identity: "U-a",
                workspace_identity: "W",
                kind: CodeGrantKind::Person,
            },
            HASH_A,
            HASH_B,
        )
        .await
        .unwrap();
        let second = mint_external_grant(
            &db,
            &owner,
            MintGrantSubject {
                channel_kind: "slack",
                external_identity: "U-b",
                workspace_identity: "W",
                kind: CodeGrantKind::Person,
            },
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        )
        .await
        .unwrap();
        bind_external_session(&db, &owner, first.id, "slack", "W/C/1", parent.id)
            .await
            .unwrap();
        bind_external_session(
            &db,
            &owner,
            second.id,
            "slack",
            &format!("child/{}/one", parent.id),
            child.id,
        )
        .await
        .unwrap();
        grant_session_access(
            &db,
            &owner,
            parent.id,
            "principal:user:bob",
            SessionAccessLevel::Contribute,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        assert!(
            resolve_session_access(&db, &reader, child.id)
                .await
                .unwrap()
                .is_none(),
            "a child bound to a different grant must not inherit"
        );
    }
}
