//! Session access rows, visibility, and the read that resolves them.
//!
//! Decision 0086. A session keeps one owner, who stays its execution identity
//! and its lifecycle authority. Everything else a second person may do comes
//! from a `session_access` row or from `deployment` visibility, and this
//! module is the only place that decides which.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set, TryIntoModel};

use crate::code::{Session, SessionAccessLevel, SessionId, SessionVisibility};
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
pub fn principal_subject(owner: &OwnerId) -> String {
    format!("principal:{}", owner.as_str())
}

/// The subject that names one channel identity.
pub fn external_subject(channel_kind: &str, external_identity: &str) -> String {
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

    let Some(level) = level else {
        return Ok(None);
    };
    Ok(Some(ResolvedSessionAccess {
        session: session_from_row(row)?,
        level,
        owner: false,
    }))
}

/// Every session this principal may read, newest first.
///
/// One query rather than a resolve per session: their own, every
/// `deployment` session, and every session a subject they answer to holds a
/// row on.
pub async fn list_accessible_sessions(
    store: &DbStore,
    principal: &OwnerId,
) -> Result<Vec<Session>> {
    let granted = sea_orm::sea_query::Query::select()
        .column(entities::session_access::Column::SessionId)
        .from(entities::session_access::Entity)
        .and_where(
            entities::session_access::Column::Subject.is_in(subjects_for(store, principal).await?),
        )
        .to_owned();
    entities::session::Entity::find()
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
        .collect()
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
    if rows.is_empty() {
        return Ok(principals);
    }
    let grants = entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    for row in rows {
        if let Some(key) = row.subject.strip_prefix("principal:") {
            let principal = OwnerId::new(key)?;
            if !principals.contains(&principal) {
                principals.push(principal);
            }
            continue;
        }
        for grant in &grants {
            if external_subject(&grant.channel_kind, &grant.external_identity) != row.subject {
                continue;
            }
            let principal = OwnerId::new(&grant.owner)?;
            if !principals.contains(&principal) {
                principals.push(principal);
            }
        }
    }
    Ok(principals)
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
    Ok(Some(access_from_row(
        model.try_into_model().map_err(store_err)?,
    )?))
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
