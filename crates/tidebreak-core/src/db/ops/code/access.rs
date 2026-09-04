use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TryIntoModel,
};

use crate::code::{Session, SessionAccessLevel, SessionId, SessionVisibility};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::session::session_from_row;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionAccess {
    pub session_id: SessionId,
    pub subject: String,
    pub level: SessionAccessLevel,
    pub granted_by: OwnerId,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSessionAccess {
    pub session: Session,
    pub level: SessionAccessLevel,
    pub owner: bool,
}

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
    let owner = row.owner == principal.as_str();
    if owner {
        return Ok(Some(ResolvedSessionAccess {
            session: session_from_row(row)?,
            level: SessionAccessLevel::Contribute,
            owner: true,
        }));
    }
    let mut level = if row.visibility == SessionVisibility::Deployment.as_str() {
        Some(SessionAccessLevel::View)
    } else {
        None
    };
    let access_rows = entities::session_access::Entity::find()
        .filter(entities::session_access::Column::SessionId.eq(id.0))
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let principal_subject = format!("principal:{}", principal.as_str());
    let grants = entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::Owner.eq(principal.as_str()))
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    for access in access_rows {
        let matches = access.subject == principal_subject
            || grants.iter().any(|grant| {
                access.subject
                    == format!(
                        "external:{}:{}",
                        grant.channel_kind, grant.external_identity
                    )
            });
        if matches {
            let candidate = access_level(&access.level)?;
            if candidate == SessionAccessLevel::Contribute {
                level = Some(candidate);
                break;
            }
            level.get_or_insert(candidate);
        }
    }
    Ok(level.map(|level| ResolvedSessionAccess {
        session: session_from_row(row).expect("validated session row"),
        level,
        owner: false,
    }))
}

pub async fn list_session_access(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
) -> Result<Vec<SessionAccess>> {
    if entities::session::Entity::find_by_id(id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_none()
    {
        return Ok(Vec::new());
    }
    entities::session_access::Entity::find()
        .filter(entities::session_access::Column::SessionId.eq(id.0))
        .order_by_asc(entities::session_access::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(access_from_row)
        .collect()
}

pub async fn grant_session_access(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
    subject: &str,
    level: SessionAccessLevel,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<SessionAccess>> {
    if !valid_subject(subject) || subject.len() > 1024 {
        return Err(AgentError::Store(
            "session access subject is invalid".into(),
        ));
    }
    if entities::session::Entity::find_by_id(id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_none()
    {
        return Ok(None);
    }
    let model = entities::session_access::ActiveModel {
        session_id: Set(id.0),
        subject: Set(subject.to_owned()),
        level: Set(level.as_str().to_owned()),
        granted_by: Set(owner.as_str().to_owned()),
        created_at: Set(now),
    };
    let row = model
        .save(&store.conn)
        .await
        .map_err(store_err)?
        .try_into_model()
        .map_err(store_err)?;
    Ok(Some(access_from_row(row)?))
}

pub async fn revoke_session_access(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
    subject: &str,
) -> Result<bool> {
    let owned = entities::session::Entity::find_by_id(id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some();
    if !owned {
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

pub async fn set_session_visibility(
    store: &DbStore,
    owner: &OwnerId,
    id: SessionId,
    visibility: SessionVisibility,
) -> Result<Option<Session>> {
    let updated = entities::session::Entity::update_many()
        .col_expr(
            entities::session::Column::Visibility,
            sea_orm::sea_query::Expr::value(visibility.as_str()),
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

fn valid_subject(subject: &str) -> bool {
    subject
        .strip_prefix("principal:")
        .is_some_and(|value| !value.is_empty())
        || subject.strip_prefix("external:").is_some_and(|value| {
            value
                .split_once(':')
                .is_some_and(|(kind, identity)| !kind.is_empty() && !identity.is_empty())
        })
}

fn access_level(value: &str) -> Result<SessionAccessLevel> {
    match value {
        "view" => Ok(SessionAccessLevel::View),
        "contribute" => Ok(SessionAccessLevel::Contribute),
        _ => Err(AgentError::Store(format!(
            "session access row has unknown level {value}"
        ))),
    }
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
