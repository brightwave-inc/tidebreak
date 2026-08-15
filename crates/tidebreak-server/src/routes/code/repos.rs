use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use std::path::PathBuf;

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

use super::require_code;
use super::types::{CodeRepoSnapshot, CreateRepoBody, PatchRepoBody};
use tidebreak_core::RepoId;

pub async fn create_repo(
    State(state): State<AppState>,
    Json(body): Json<CreateRepoBody>,
) -> Result<impl IntoResponse, ServerError> {
    let runtime = require_code(&state)?;
    let repo = runtime
        .register_repo(
            PathBuf::from(body.path),
            body.display_name,
            body.default_base_ref,
            body.branch_prefix,
            body.setup_script,
            body.archive_script,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(CodeRepoSnapshot::from(repo))))
}

pub async fn list_repos(
    State(state): State<AppState>,
) -> Result<Json<Vec<CodeRepoSnapshot>>, ServerError> {
    let runtime = require_code(&state)?;
    let repos = runtime.list_repos().await?;
    Ok(Json(
        repos.into_iter().map(CodeRepoSnapshot::from).collect(),
    ))
}

pub async fn get_repo(
    State(state): State<AppState>,
    Path(id): Path<RepoId>,
) -> Result<Json<CodeRepoSnapshot>, ServerError> {
    let runtime = require_code(&state)?;
    Ok(Json(CodeRepoSnapshot::from(runtime.get_repo(id).await?)))
}

pub async fn patch_repo(
    State(state): State<AppState>,
    Path(id): Path<RepoId>,
    Json(body): Json<PatchRepoBody>,
) -> Result<Json<CodeRepoSnapshot>, ServerError> {
    let runtime = require_code(&state)?;
    let mut repo = runtime.get_repo(id).await?;
    if let Some(name) = body.display_name {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(ServerError::bad_request("display_name must not be empty"));
        }
        repo.display_name = name;
    }
    if let Some(base) = body.default_base_ref {
        let base = base.trim().to_owned();
        if base.is_empty() {
            return Err(ServerError::bad_request(
                "default_base_ref must not be empty",
            ));
        }
        repo.default_base_ref = base;
    }
    if let Some(prefix) = body.branch_prefix {
        let prefix = prefix.trim().to_owned();
        if prefix.is_empty() {
            return Err(ServerError::bad_request("branch_prefix must not be empty"));
        }
        repo.branch_prefix = prefix;
    }
    if let Some(script) = body.setup_script {
        repo.setup_script = script.filter(|value| !value.trim().is_empty());
    }
    if let Some(script) = body.archive_script {
        repo.archive_script = script.filter(|value| !value.trim().is_empty());
    }
    runtime.save_repo(&repo).await?;
    Ok(Json(CodeRepoSnapshot::from(repo)))
}

pub async fn delete_repo(
    State(state): State<AppState>,
    Path(id): Path<RepoId>,
) -> Result<StatusCode, ServerError> {
    require_code(&state)?.delete_repo(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
