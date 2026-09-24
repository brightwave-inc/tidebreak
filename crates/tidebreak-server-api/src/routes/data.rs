//! Data and privacy: where the profile lives, a backup of it, and exports of
//! the caller's conversations.
//!
//! The overview and the backup cover the whole profile, every owner's data
//! included, so they sit on the deployment plane. The export reads only the
//! caller's own conversations and sits on the member plane.

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::Response;
use tokio::io::AsyncReadExt as _;

use crate::error::ServerError;
use crate::extract::Json;
use crate::profile_data::{
    build_backup, disk_usage, export_conversations, ConversationExportRequest, DataOverview,
    DataStorage,
};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

/// How much of a backup archive one response chunk carries.
const BACKUP_CHUNK_BYTES: usize = 256 * 1024;

/// Where this server's database file is, or why it cannot be backed up.
async fn backup_database(state: &AppState) -> Result<std::path::PathBuf, String> {
    if state.config.blob_store_url.is_some() {
        return Err(
            "This server keeps attachments in object storage. Back up the bucket and the \
             database with their own tools."
                .to_owned(),
        );
    }
    let Some(db) = state.memory.as_ref() else {
        return Err("This server has no local database to back up.".to_owned());
    };
    match db.sqlite_database_path().await {
        Ok(Some(path)) => Ok(path),
        Ok(None) => Err(
            "This server keeps conversations in PostgreSQL. Back it up with pg_dump, as the \
             self-hosting guide describes."
                .to_owned(),
        ),
        Err(error) => Err(format!("Tidebreak could not find its database: {error}")),
    }
}

/// `GET /data` — where the profile lives, which database holds it, and how
/// much disk each part uses.
pub async fn get_data_overview(
    State(state): State<AppState>,
) -> Result<Json<DataOverview>, ServerError> {
    let database = backup_database(&state).await;
    let sqlite = match state.memory.as_ref() {
        Some(db) => db.sqlite_database_path().await?.is_some(),
        None => false,
    };
    let data_dir = state.config.data_dir.clone();
    let measured = data_dir.clone();
    let usage = tokio::task::spawn_blocking(move || disk_usage(&measured))
        .await
        .map_err(|_| ServerError::internal("the disk use worker stopped"))?;
    let total_bytes = usage
        .iter()
        .map(|entry| entry.bytes)
        .fold(0, u64::saturating_add);
    Ok(Json(DataOverview {
        data_dir: data_dir.display().to_string(),
        storage: if sqlite {
            DataStorage::Sqlite
        } else {
            DataStorage::Postgres
        },
        usage,
        total_bytes,
        backup_unavailable: database.err(),
    }))
}

/// `POST /data/backup` — a `.tar.gz` of the database, taken with SQLite's
/// own `VACUUM INTO` while the server keeps running, plus the attached files
/// and outputs it points at.
///
/// The archive is finished before the response starts, so the status reflects
/// the whole backup, and `Content-Length` lets a client check it received
/// every byte. A PostgreSQL deployment answers `409 backup_unavailable`: its
/// operator owns its backups (decision 100).
pub async fn post_data_backup(State(state): State<AppState>) -> Result<Response, ServerError> {
    let database = backup_database(&state)
        .await
        .map_err(|reason| ServerError::conflict_kind("backup_unavailable", reason))?;
    let archive = build_backup(&database, &state.config.data_dir).await?;
    let file_name = format!(
        "Tidebreak backup {}.tar.gz",
        chrono::Utc::now().format("%Y-%m-%d %H%M")
    );
    let file = tokio::fs::File::from_std(archive.file);
    let chunks = futures::stream::try_unfold(file, |mut file| async move {
        let mut chunk = vec![0_u8; BACKUP_CHUNK_BYTES];
        let read = file.read(&mut chunk).await?;
        if read == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        chunk.truncate(read);
        Ok(Some((Bytes::from(chunk), file)))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{file_name}\""),
        )
        .header(header::CONTENT_LENGTH, archive.bytes.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("x-tidebreak-backup-files", archive.files.to_string())
        .body(Body::from_stream(chunks))
        .map_err(|_| ServerError::internal("could not build the backup response"))
}

/// `POST /data/export` — the caller's conversations as a `.zip` of Markdown
/// files or one JSON document.
pub async fn post_data_export(
    store: ScopedStore,
    Json(request): Json<ConversationExportRequest>,
) -> Result<Response, ServerError> {
    let export = export_conversations(&store, &request, chrono::Utc::now()).await?;
    let length = export.bytes.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, export.content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", export.file_name),
        )
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            "x-tidebreak-conversations",
            export.conversations.to_string(),
        )
        .body(Body::from(export.bytes))
        .map_err(|_| ServerError::internal("could not build the export response"))
}
