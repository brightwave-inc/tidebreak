//! Owner-checked, immutable inference selection and verified personal links.
use crate::code::{inference::SessionInference, CodeExternalGrant, CodeGrantId, CodeGrantKind};
use crate::db::{entities, store_err, DbStore};
use crate::{AgentError, Result};
use crate::{OwnerId, SessionId};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait,
};

pub async fn session_inference(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
) -> Result<Option<SessionInference>> {
    if super::get_session(store, owner, session).await?.is_none() {
        return Err(AgentError::InvalidTarget("session not found".into()));
    }
    read_on(&store.conn, session).await
}
async fn read_on<C: ConnectionTrait>(
    conn: &C,
    session: SessionId,
) -> Result<Option<SessionInference>> {
    entities::code_session_inference::Entity::find_by_id(session.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(|row| {
            serde_json::from_value(row.selection).map_err(|e| AgentError::Store(e.to_string()))
        })
        .transpose()
}
pub(super) async fn insert_inference_on<C: ConnectionTrait>(
    conn: &C,
    session: SessionId,
    selection: &SessionInference,
) -> Result<()> {
    if let Some(sponsor) = &selection.sponsor {
        sponsor.validate()?;
    }
    entities::code_session_inference::ActiveModel {
        session_id: Set(session.0),
        selection: Set(
            serde_json::to_value(selection).map_err(|e| AgentError::Store(e.to_string()))?
        ),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Copy the parent's frozen selection before the child's first turn. A retry cannot replace it.
pub async fn inherit_session_inference(
    store: &DbStore,
    owner: &OwnerId,
    parent: SessionId,
    child: SessionId,
) -> Result<()> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    super::acquire_code_session_write_lock(&transaction, child).await?;
    for id in [parent, child] {
        if entities::session::Entity::find_by_id(id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_none_or(|row| row.owner != owner.as_str())
        {
            return Err(AgentError::InvalidTarget("session not found".into()));
        }
    }
    if let Some(selection) = read_on(&transaction, parent).await? {
        match read_on(&transaction, child).await? {
            Some(existing) if existing != selection => {
                return Err(AgentError::InvalidTarget(
                    "the child has a different inference selection".into(),
                ))
            }
            Some(_) => {}
            None => insert_inference_on(&transaction, child, &selection).await?,
        }
    }
    transaction.commit().await.map_err(store_err)
}

/// System lookup for a verified Slack starter when a channel service owns the session.
/// The personal connection can belong to another owner, so callers supply the
/// adapter-verified workspace and identity. Only a completed, unambiguous connection
/// can carry sponsorship authority; Gateway validates the live consent separately.
pub async fn personal_inference_grant_all_owners(
    store: &DbStore,
    workspace: &str,
    identity: &str,
    exact: Option<CodeGrantId>,
) -> Result<Option<CodeExternalGrant>> {
    let mut query = entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::ChannelKind.eq("slack"))
        .filter(entities::code_external_grant::Column::WorkspaceIdentity.eq(workspace))
        .filter(entities::code_external_grant::Column::ExternalIdentity.eq(identity))
        .filter(entities::code_external_grant::Column::Kind.eq(CodeGrantKind::Person.as_str()))
        .filter(entities::code_external_grant::Column::RevokedAt.is_null());
    if let Some(id) = exact {
        query = query.filter(entities::code_external_grant::Column::Id.eq(id.0));
    }
    let rows = query.all(&store.conn).await.map_err(store_err)?;
    let mut found = None;
    for row in rows {
        let owner = OwnerId::new(&row.owner)?;
        let id = CodeGrantId(row.id);
        if super::completed_connect_handshake_for_grant(store, &owner, id)
            .await?
            .is_none()
        {
            continue;
        }
        if found.is_some() {
            return Ok(None);
        }
        found = super::get_external_grant(store, &owner, id).await?;
    }
    if exact.is_some() && found.is_none() {
        return Err(AgentError::InvalidTarget(
            "the supplied personal connection is not live and verified for this Slack identity"
                .into(),
        ));
    }
    Ok(found)
}

/// Retain only Gateway's resolved choices; a preference alone is not usage evidence.
pub async fn record_inference_resolutions(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    resolutions: &[crate::code::inference::InferenceResolution],
) -> Result<()> {
    if resolutions.is_empty() {
        return Ok(());
    }
    let selection = session_inference(store, owner, session)
        .await?
        .ok_or_else(|| {
            AgentError::InvalidTarget("the conversation has no inference selection".into())
        })?;
    let tx = store.conn.begin().await.map_err(store_err)?;
    super::acquire_code_session_write_lock(&tx, selection.root_session_id).await?;
    for resolution in resolutions {
        if resolution.scope_id != selection.root_session_id.0
            || resolution.provider.is_empty()
            || resolution.provider.len() > 128
            || !matches!(
                resolution.source.as_str(),
                "owned_subscription" | "execution_default"
            )
        {
            return Err(AgentError::InvalidTarget(
                "Gateway returned a different inference scope".into(),
            ));
        }
        let json =
            serde_json::to_value(resolution).map_err(|e| AgentError::Store(e.to_string()))?;
        if let Some(existing) = entities::code_inference_resolution::Entity::find_by_id((
            selection.root_session_id.0,
            resolution.provider.clone(),
        ))
        .one(&tx)
        .await
        .map_err(store_err)?
        {
            if existing.resolution != json {
                return Err(AgentError::InvalidTarget(
                    "Gateway changed the conversation's inference choice".into(),
                ));
            }
        } else {
            entities::code_inference_resolution::ActiveModel {
                root_session_id: Set(selection.root_session_id.0),
                provider: Set(resolution.provider.clone()),
                resolution: Set(json),
            }
            .insert(&tx)
            .await
            .map_err(store_err)?;
        }
    }
    tx.commit().await.map_err(store_err)
}

pub async fn inference_resolutions(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
) -> Result<Vec<crate::code::inference::InferenceResolution>> {
    let Some(selection) = session_inference(store, owner, session).await? else {
        return Ok(Vec::new());
    };
    let mut rows = entities::code_inference_resolution::Entity::find()
        .filter(
            entities::code_inference_resolution::Column::RootSessionId
                .eq(selection.root_session_id.0),
        )
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    rows.sort_by(|a, b| a.provider.cmp(&b.provider));
    rows.into_iter()
        .map(|row| {
            serde_json::from_value(row.resolution).map_err(|e| AgentError::Store(e.to_string()))
        })
        .collect()
}
