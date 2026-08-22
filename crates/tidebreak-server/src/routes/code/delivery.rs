//! Cross-repository GitHub delivery routes.

use axum::extract::Query;
use serde::Deserialize;

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::Json;

use super::types::{
    CodeDeliveryActionResult, CodeDeliveryPullRequestActionBody, CodeDeliveryPullRequestDetail,
    CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestTarget, CodeDeliveryPullRequestsPage,
    CodeDeliveryRepositoriesSnapshot, CodeDeliveryRunActionBody, CodeDeliveryRunDetail,
    CodeDeliveryRunQuery, CodeDeliveryRunTarget, CodeDeliveryRunsPage,
    ResolveCodeDeliveryRepositoriesBody,
};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverCodeDeliveryRepositoriesQuery {
    #[serde(default)]
    refresh: bool,
}

pub async fn discover_repositories(
    code: ScopedCode,
    Query(query): Query<DiscoverCodeDeliveryRepositoriesQuery>,
) -> Result<Json<CodeDeliveryRepositoriesSnapshot>, ServerError> {
    Ok(Json(
        code.discover_delivery_repositories(query.refresh).await?,
    ))
}

pub async fn resolve_repositories(
    code: ScopedCode,
    Json(body): Json<ResolveCodeDeliveryRepositoriesBody>,
) -> Result<Json<CodeDeliveryRepositoriesSnapshot>, ServerError> {
    Ok(Json(code.resolve_delivery_repositories(body).await?))
}

pub async fn query_pull_requests(
    code: ScopedCode,
    Json(query): Json<CodeDeliveryPullRequestQuery>,
) -> Result<Json<CodeDeliveryPullRequestsPage>, ServerError> {
    Ok(Json(code.query_delivery_pull_requests(query).await?))
}

pub async fn pull_request_detail(
    code: ScopedCode,
    Json(target): Json<CodeDeliveryPullRequestTarget>,
) -> Result<Json<CodeDeliveryPullRequestDetail>, ServerError> {
    Ok(Json(code.delivery_pull_request_detail(target).await?))
}

pub async fn act_on_pull_request(
    code: ScopedCode,
    Json(body): Json<CodeDeliveryPullRequestActionBody>,
) -> Result<Json<CodeDeliveryActionResult>, ServerError> {
    Ok(Json(code.act_on_delivery_pull_request(body).await?))
}

pub async fn query_runs(
    code: ScopedCode,
    Json(query): Json<CodeDeliveryRunQuery>,
) -> Result<Json<CodeDeliveryRunsPage>, ServerError> {
    Ok(Json(code.query_delivery_runs(query).await?))
}

pub async fn run_detail(
    code: ScopedCode,
    Json(target): Json<CodeDeliveryRunTarget>,
) -> Result<Json<CodeDeliveryRunDetail>, ServerError> {
    Ok(Json(code.delivery_run_detail(target).await?))
}

pub async fn act_on_run(
    code: ScopedCode,
    Json(body): Json<CodeDeliveryRunActionBody>,
) -> Result<Json<CodeDeliveryActionResult>, ServerError> {
    Ok(Json(code.act_on_delivery_run(body).await?))
}
