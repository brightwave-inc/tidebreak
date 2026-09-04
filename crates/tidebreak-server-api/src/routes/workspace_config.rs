//! Export, preview, and apply portable workspace configuration (decision 83).

use axum::extract::State;
use axum::response::IntoResponse;

use crate::code::runtime::RepoRegistration;
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::Json;
use crate::mcp_config::{ManualLockdown, McpServersConfig};
use crate::principal::AuthContext;
use crate::state::AppState;
use crate::workspace_config::{
    apply_mcp_remap, apply_repo_path, export_code_repositories, export_mcp_servers,
    exported_mcp_to_definition, mcp_lockdown_blocks, parse_document, preview_document,
    WorkspaceConfigAction, WorkspaceConfigApplyRequest, WorkspaceConfigApplyResult,
    WorkspaceConfigDocument, WorkspaceConfigPreview, WorkspaceConfigSectionId, FORMAT_VERSION,
};

pub async fn export_workspace_config(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<WorkspaceConfigDocument>, ServerError> {
    let repos = if let Some(runtime) = state.code.clone() {
        let code = ScopedCode::for_owner(runtime, auth.principal.owner_id());
        code.list_repos().await?
    } else {
        Vec::new()
    };
    let definitions = state.mcp.definitions().await;
    Ok(Json(WorkspaceConfigDocument {
        tidebreak_config: FORMAT_VERSION,
        exported_at: chrono::Utc::now(),
        sections: crate::workspace_config::WorkspaceConfigSections {
            code_repositories: export_code_repositories(&repos).await,
            mcp_servers: export_mcp_servers(&definitions),
        },
    }))
}

pub async fn preview_workspace_config(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(value): Json<serde_json::Value>,
) -> Result<Json<WorkspaceConfigPreview>, ServerError> {
    let document = parse_document(value)?;
    let repos = if let Some(runtime) = state.code.clone() {
        let code = ScopedCode::for_owner(runtime, auth.principal.owner_id());
        code.list_repos().await?
    } else {
        Vec::new()
    };
    let definitions = state.mcp.definitions().await;
    Ok(Json(preview_document(&document, &repos, &definitions)))
}

pub async fn apply_workspace_config(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(body): Json<WorkspaceConfigApplyRequest>,
) -> Result<impl IntoResponse, ServerError> {
    let document = body.document;
    // Re-parse through the version gate by round-tripping JSON so a client
    // cannot skip parse_document by sending a typed body with a future version.
    let value = serde_json::to_value(&document).map_err(|error| {
        ServerError::bad_request(format!("invalid workspace configuration: {error}"))
    })?;
    let document = parse_document(value)?;

    let policy = state.managed_policy()?;
    let lockdown = ManualLockdown::for_policy(&policy);

    let mut applied = 0usize;
    let mut skipped = 0usize;

    let mut mcp = state.mcp.definitions().await;
    let mut mcp_changed = false;

    for decision in &body.decisions {
        if decision.action == WorkspaceConfigAction::Skip {
            skipped += 1;
            continue;
        }
        match decision.section {
            WorkspaceConfigSectionId::McpServers => {
                let Some(exported) = document
                    .sections
                    .mcp_servers
                    .iter()
                    .find(|item| item.name == decision.key)
                else {
                    return Err(ServerError::bad_request(format!(
                        "no MCP server named {} in the imported file",
                        decision.key
                    )));
                };
                let mut definition =
                    apply_mcp_remap(exported_mcp_to_definition(exported), &decision.remaps);
                let existing_idx = mcp.iter().position(|item| item.name == definition.name);
                if existing_idx.is_some() && decision.action != WorkspaceConfigAction::Replace {
                    return Err(ServerError::conflict_kind(
                        "workspace_config_replace_required",
                        format!(
                            "MCP server {} already exists; choose replace to overwrite it, or skip to leave it",
                            definition.name
                        ),
                    ));
                }
                if mcp_lockdown_blocks(&definition, lockdown) {
                    let existing_same = existing_idx
                        .and_then(|idx| mcp.get(idx))
                        .is_some_and(|current| current == &definition);
                    if !existing_same {
                        return Err(crate::providers::managed_profile_refusal(format!(
                            "this profile is managed by a model gateway; manual MCP server {} cannot be imported. Mount gateway-managed endpoints instead.",
                            definition.name
                        )));
                    }
                }
                if let Some(idx) = existing_idx {
                    // Keep stored env values: never send env_values on import.
                    definition.env_values.clear();
                    mcp[idx] = definition;
                } else {
                    mcp.push(definition);
                }
                mcp_changed = true;
                applied += 1;
            }
            WorkspaceConfigSectionId::CodeRepositories => {
                let Some(exported) = document.sections.code_repositories.iter().find(|item| {
                    item.origin_url.as_deref() == Some(decision.key.as_str())
                        || item.cloned_from.as_deref() == Some(decision.key.as_str())
                        || item.display_name == decision.key
                }) else {
                    return Err(ServerError::bad_request(format!(
                        "no code repository {} in the imported file",
                        decision.key
                    )));
                };
                let Some(runtime) = state.code.clone() else {
                    return Err(ServerError::bad_request(
                        "code mode is not configured on this server; skip code repository entries",
                    ));
                };
                let code = ScopedCode::for_owner(runtime, auth.principal.owner_id());
                let repos = code.list_repos().await?;
                let matching = repos.iter().find(|repo| {
                    repo.removed_at.is_none()
                        && (exported
                            .origin_url
                            .as_deref()
                            .is_some_and(|origin| repo.cloned_from.as_deref() == Some(origin))
                            || repo.display_name == exported.display_name)
                });
                if matching.is_some() && decision.action != WorkspaceConfigAction::Replace {
                    return Err(ServerError::conflict_kind(
                        "workspace_config_replace_required",
                        format!(
                            "repository {} already exists; choose replace to overwrite its settings, or skip to leave it",
                            exported.display_name
                        ),
                    ));
                }
                let root = apply_repo_path(exported, &decision.remaps);
                if let Some(existing) = matching {
                    let mut repo = existing.clone();
                    repo.display_name = exported.display_name.clone();
                    repo.default_base_ref = exported.default_base_ref.clone();
                    repo.branch_prefix = exported.branch_prefix.clone();
                    repo.setup_script = exported.setup_script.clone();
                    repo.archive_script = exported.archive_script.clone();
                    repo.quick_actions = exported.quick_actions.clone();
                    code.save_repo(&repo).await?;
                } else {
                    code.register_repo(
                        std::path::PathBuf::from(&root),
                        RepoRegistration {
                            cloned_from: exported.cloned_from.clone(),
                            display_name: Some(exported.display_name.clone()),
                            default_base_ref: Some(exported.default_base_ref.clone()),
                            branch_prefix: Some(exported.branch_prefix.clone()),
                            setup_script: exported.setup_script.clone(),
                            archive_script: exported.archive_script.clone(),
                            quick_actions: exported.quick_actions.clone(),
                        },
                    )
                    .await?;
                }
                applied += 1;
            }
        }
    }

    if mcp_changed {
        let outcome = state
            .mcp
            .replace_under_policy(McpServersConfig { servers: mcp }, lockdown)
            .await
            .map_err(ServerError::from)?;
        match outcome {
            crate::mcp_config::McpReplaceOutcome::Replaced(_) => {}
            crate::mcp_config::McpReplaceOutcome::RefusedManual(refused) => {
                return Err(crate::providers::managed_profile_refusal(format!(
                    "this profile is managed by a model gateway; manual MCP servers are locked ({})",
                    refused.join(", ")
                )));
            }
        }
    }

    Ok(Json(WorkspaceConfigApplyResult { applied, skipped }))
}
