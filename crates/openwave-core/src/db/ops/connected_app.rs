//! Profile-scoped connected-app records.
//!
//! The write surface mirrors the settings pages that own it: a wholesale
//! replacement of one kind's complete list, in one transaction, so a
//! concurrent save can never interleave into a mixed state. Reads return
//! every kind together — the Connected apps surface lists them side by side
//! and callers filter by kind.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::connected_app::{
    validate_connected_app, ConnectedApp, ConnectedAppKind, MAX_CONNECTED_APPS,
};
use crate::error::{AgentError, Result};
use crate::id::ConnectedAppId;

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;

pub(in crate::db) async fn list_connected_apps(store: &DbStore) -> Result<Vec<ConnectedApp>> {
    entities::connected_app::Entity::find()
        .order_by_asc(entities::connected_app::Column::Kind)
        .order_by_asc(entities::connected_app::Column::Position)
        .order_by_asc(entities::connected_app::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(connected_app_from_model)
        .collect()
}

pub(in crate::db) async fn replace_connected_apps(
    store: &DbStore,
    kind: ConnectedAppKind,
    apps: &[ConnectedApp],
) -> Result<()> {
    if apps.len() > MAX_CONNECTED_APPS {
        return Err(AgentError::Store(format!(
            "cannot store more than {MAX_CONNECTED_APPS} connected apps"
        )));
    }
    for app in apps {
        if app.kind != kind {
            return Err(AgentError::Store(format!(
                "connected app {} is {}, not the {kind} this replacement covers",
                app.id, app.kind
            )));
        }
        validate_connected_app(app)
            .map_err(|message| AgentError::Store(format!("invalid connected app: {message}")))?;
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let existing: Vec<entities::connected_app::Model> = entities::connected_app::Entity::find()
        .filter(entities::connected_app::Column::Kind.eq(kind.as_str()))
        .all(&transaction)
        .await
        .map_err(store_err)?;
    for row in &existing {
        if !apps.iter().any(|app| app.id.0 == row.id) {
            entities::connected_app::Entity::delete_by_id(row.id)
                .exec(&transaction)
                .await
                .map_err(store_err)?;
        }
    }
    for (index, app) in apps.iter().enumerate() {
        // The settings surfaces edit an ordered list; the position within the
        // kind is part of the stored state.
        let position = i32::try_from(index)
            .map_err(|_| AgentError::Store("connected-app position is out of range".into()))?;
        let updated_at = canonical_db_timestamp(app.updated_at)?;
        match existing.iter().find(|row| row.id == app.id.0) {
            Some(row) => {
                if row.name == app.name
                    && row.definition_json == app.definition
                    && row.position == position
                {
                    continue;
                }
                entities::connected_app::ActiveModel {
                    id: Set(app.id.0),
                    name: Set(app.name.clone()),
                    definition_json: Set(app.definition.clone()),
                    position: Set(position),
                    updated_at: Set(
                        if row.name == app.name && row.definition_json == app.definition {
                            // A pure reorder is not an edit of what the record is.
                            row.updated_at
                        } else {
                            updated_at
                        },
                    ),
                    ..Default::default()
                }
                .update(&transaction)
                .await
                .map_err(store_err)?;
            }
            None => {
                entities::connected_app::ActiveModel {
                    id: Set(app.id.0),
                    name: Set(app.name.clone()),
                    kind: Set(kind.as_str().to_owned()),
                    definition_json: Set(app.definition.clone()),
                    position: Set(position),
                    created_at: Set(canonical_db_timestamp(app.created_at)?),
                    updated_at: Set(updated_at),
                }
                .insert(&transaction)
                .await
                .map_err(store_err)?;
            }
        }
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

fn connected_app_from_model(model: entities::connected_app::Model) -> Result<ConnectedApp> {
    let kind = ConnectedAppKind::parse(&model.kind).ok_or_else(|| {
        AgentError::Store(format!(
            "stored connected app has unknown kind {:?}",
            model.kind
        ))
    })?;
    Ok(ConnectedApp {
        id: ConnectedAppId(model.id),
        name: model.name,
        kind,
        definition: model.definition_json,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}
