use axum::http::StatusCode;
use axum::response::IntoResponse;
use std::path::PathBuf;
use uuid::Uuid;

use crate::code::runtime::RepoRegistration;
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};

use super::types::{
    CloneRepoBody, CodeCloneDefaults, CodeCloneJobSnapshot, CodeGithubRepositories,
    CodeRepoSnapshot, CodeRepoSources, CreateRepoBody, PatchRepoBody, RemoveRepoQuery,
};
use tidebreak_core::RepoId;

pub async fn create_repo(
    code: ScopedCode,
    Json(body): Json<CreateRepoBody>,
) -> Result<impl IntoResponse, ServerError> {
    let repo = code
        .register_repo(
            PathBuf::from(body.path),
            RepoRegistration {
                // A registration names a directory that already exists, so it
                // is never Tidebreak's to delete. Only the clone path sets
                // this, and a client cannot claim it.
                cloned_from: None,
                display_name: body.display_name,
                default_base_ref: body.default_base_ref,
                branch_prefix: body.branch_prefix,
                setup_script: body.setup_script,
                archive_script: body.archive_script,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(CodeRepoSnapshot::from(repo))))
}

pub async fn list_repos(code: ScopedCode) -> Result<Json<Vec<CodeRepoSnapshot>>, ServerError> {
    let repos = code.list_repos().await?;
    Ok(Json(
        repos.into_iter().map(CodeRepoSnapshot::from).collect(),
    ))
}

pub async fn get_repo(
    code: ScopedCode,
    Path(id): Path<RepoId>,
) -> Result<Json<CodeRepoSnapshot>, ServerError> {
    Ok(Json(CodeRepoSnapshot::from(code.get_repo(id).await?)))
}

pub async fn patch_repo(
    code: ScopedCode,
    Path(id): Path<RepoId>,
    Json(body): Json<PatchRepoBody>,
) -> Result<Json<CodeRepoSnapshot>, ServerError> {
    let mut repo = code.get_repo(id).await?;
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
    code.save_repo(&repo).await?;
    Ok(Json(CodeRepoSnapshot::from(repo)))
}

/// `DELETE /code/repos/{id}` — remove a registration, optionally reclaiming
/// the checkout.
///
/// Removal is soft: archived workspaces and their transcripts stay reachable.
/// `reclaim_checkout` additionally deletes the directory, and is honored only
/// for a repository Tidebreak cloned — 409 `checkout_not_reclaimable`
/// otherwise, and `checkout_not_a_repository` if the path stopped being the
/// checkout that was cloned.
pub async fn delete_repo(
    code: ScopedCode,
    Path(id): Path<RepoId>,
    Query(query): Query<RemoveRepoQuery>,
) -> Result<StatusCode, ServerError> {
    code.remove_repo(id, query.reclaim_checkout).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn clone_defaults(code: ScopedCode) -> Result<Json<CodeCloneDefaults>, ServerError> {
    Ok(Json(code.clone_defaults().await?))
}

pub async fn repo_sources(code: ScopedCode) -> Result<Json<CodeRepoSources>, ServerError> {
    Ok(Json(code.repo_sources().await?))
}

pub async fn list_github_repositories(
    code: ScopedCode,
) -> Result<Json<CodeGithubRepositories>, ServerError> {
    Ok(Json(code.list_github_repositories().await?))
}

pub async fn start_clone(
    code: ScopedCode,
    Json(body): Json<CloneRepoBody>,
) -> Result<(StatusCode, Json<CodeCloneJobSnapshot>), ServerError> {
    let job = code
        .start_clone(crate::code::clone::CloneRequest {
            url: body.url,
            github: body.github,
            parent_dir: body.parent_dir,
            name: body.name,
        })
        .await?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

pub async fn get_clone_job(
    code: ScopedCode,
    Path(job): Path<Uuid>,
) -> Result<Json<CodeCloneJobSnapshot>, ServerError> {
    Ok(Json(code.get_clone_job(job)?))
}
