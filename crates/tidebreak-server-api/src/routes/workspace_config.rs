//! Export, preview, and apply portable workspace configuration (decision 83).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::response::IntoResponse;
use tidebreak_core::CodeRepo;

use crate::code::runtime::RepoRegistration;
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::Json;
use crate::mcp_config::{ManualLockdown, McpServerDefinition, McpServersConfig};
use crate::principal::AuthContext;
use crate::state::AppState;
use crate::workspace_config::{
    apply_repo_path, export_code_repositories, export_mcp_servers, find_repo,
    imported_mcp_definition, mcp_lockdown_blocks, parse_document, preview_document, repo_key,
    starts_local_command, WorkspaceConfigAction, WorkspaceConfigApplyRequest,
    WorkspaceConfigApplyResult, WorkspaceConfigDecision, WorkspaceConfigDocument,
    WorkspaceConfigPreview, WorkspaceConfigSectionId, FORMAT_VERSION,
};

use super::code::normalize_quick_actions;

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

/// `POST /workspace-config/apply`, and its native twin
/// `POST /native/workspace-config/apply`.
///
/// Every decision is validated before anything is written, so a refused
/// entry leaves this machine exactly as it was. The MCP set commits first,
/// because it is the step most likely to fail (a server that will not start);
/// repository writes follow once it has landed.
///
/// On the desktop, an import that would start a local MCP command needs the
/// native confirmation, the same rule `PUT /mcp/servers` enforces (decision
/// 27): the renderer's route refuses it with `native_confirmation_required`,
/// and only the native twin, which the desktop reaches after an OS dialog
/// lists the commands, may write it. An import that brings such a server in
/// turned off starts nothing and needs no confirmation. On a multi-user
/// deployment, only an administrator may import MCP servers at all.
pub async fn apply_workspace_config(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(body): Json<WorkspaceConfigApplyRequest>,
) -> Result<impl IntoResponse, ServerError> {
    // Re-parse through the version gate by round-tripping JSON so a client
    // cannot skip parse_document by sending a typed body with a future version.
    let value = serde_json::to_value(&body.document).map_err(|error| {
        ServerError::bad_request(format!("invalid workspace configuration: {error}"))
    })?;
    let document = parse_document(value)?;

    let policy = state.managed_policy()?;
    let lockdown = ManualLockdown::for_policy(&policy);
    let code = state
        .code
        .clone()
        .map(|runtime| ScopedCode::for_owner(runtime, auth.principal.owner_id()));
    let repos = match &code {
        Some(code) => Some(code.list_repos().await?),
        None => None,
    };
    // Plugin-sourced servers ride along in the published set, but they are
    // derived from the plugin tree and a replacement must never name them.
    let configured: Vec<McpServerDefinition> = state
        .mcp
        .definitions()
        .await
        .into_iter()
        .filter(|definition| definition.plugin.is_none())
        .collect();

    let plan = plan_apply(
        &document,
        &body.decisions,
        configured,
        repos.as_deref(),
        lockdown,
    )
    .await?;

    // The MCP half of an import replaces the deployment's MCP servers, host
    // processes included, which `PUT /mcp/servers` reserves for the
    // deployment plane's role (decision 6). This route lives on the member
    // plane for the repository half, so it applies the same rule itself.
    if plan.mcp.is_some() && !auth.principal.is_admin() {
        return Err(ServerError::forbidden(
            "only an administrator can import MCP servers into this deployment; skip the MCP server entries",
        ));
    }

    if state.config.profile == tidebreak_core::Profile::Desktop
        && !auth.client_executor
        && plan.mcp_imports.iter().any(starts_local_command)
    {
        return Err(ServerError::bad_request_kind(
            "native_confirmation_required",
            "importing a local command MCP server that starts right away needs Tidebreak's native confirmation; import it turned off, or apply the import from the Tidebreak app",
        ));
    }

    let result = WorkspaceConfigApplyResult {
        applied: plan.applied,
        skipped: plan.skipped,
    };
    // Once the writes begin, finish them even if the client disconnects and
    // drops this handler future, so a dropped request never leaves the MCP
    // set applied without the repositories that came with it.
    let mcp = state.mcp.clone();
    let writes = tokio::spawn(async move { write_plan(plan, &mcp, code, lockdown).await });
    writes
        .await
        .map_err(|_| ServerError::internal("workspace configuration import task failed"))??;
    Ok(Json(result))
}

/// What an apply writes, decided and validated before the first write.
struct ApplyPlan {
    /// The complete configured MCP set to commit, when a decision changes it.
    mcp: Option<Vec<McpServerDefinition>>,
    /// The definitions this apply adds or replaces, as they will be written.
    mcp_imports: Vec<McpServerDefinition>,
    repos: Vec<RepoWrite>,
    applied: usize,
    skipped: usize,
}

enum RepoWrite {
    /// Overwrite an existing registration's settings with the file's.
    Replace(Box<CodeRepo>),
    /// Register a checkout that is new to this machine.
    Register {
        root: PathBuf,
        registration: RepoRegistration,
    },
}

async fn plan_apply(
    document: &WorkspaceConfigDocument,
    decisions: &[WorkspaceConfigDecision],
    configured: Vec<McpServerDefinition>,
    repos: Option<&[CodeRepo]>,
    lockdown: ManualLockdown,
) -> Result<ApplyPlan, ServerError> {
    let mut seen = HashSet::new();
    let mut planned_checkouts = HashSet::new();
    let mut mcp = configured;
    let mut mcp_changed = false;
    let mut mcp_imports = Vec::new();
    let mut repo_writes = Vec::new();
    let mut applied = 0usize;
    let mut skipped = 0usize;

    for decision in decisions {
        if !seen.insert((decision.section, decision.key.as_str())) {
            return Err(ServerError::bad_request_kind(
                "workspace_config_duplicate_decision",
                format!(
                    "the import decides {} more than once; choose one action for it",
                    decision.key
                ),
            ));
        }
        if decision.action == WorkspaceConfigAction::Skip {
            skipped += 1;
            continue;
        }
        match decision.section {
            WorkspaceConfigSectionId::McpServers => {
                check_remaps(decision, &["command", "cwd"])?;
                let Some(mut definition) = imported_mcp_definition(document, decision) else {
                    return Err(ServerError::bad_request(format!(
                        "no MCP server named {} in the imported file",
                        decision.key
                    )));
                };
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
                // Keep stored env values: never send env_values on import.
                definition.env_values.clear();
                match existing_idx {
                    Some(idx) => mcp[idx] = definition.clone(),
                    None => mcp.push(definition.clone()),
                }
                mcp_imports.push(definition);
                mcp_changed = true;
                applied += 1;
            }
            WorkspaceConfigSectionId::CodeRepositories => {
                if decision.enabled.is_some() {
                    return Err(ServerError::bad_request(format!(
                        "code repository {}: enabled applies only to MCP servers",
                        decision.key
                    )));
                }
                check_remaps(decision, &["root_path"])?;
                let Some(exported) = document
                    .sections
                    .code_repositories
                    .iter()
                    .find(|item| repo_key(item) == decision.key)
                else {
                    return Err(ServerError::bad_request(format!(
                        "no code repository {} in the imported file",
                        decision.key
                    )));
                };
                let Some(repos) = repos else {
                    return Err(ServerError::bad_request(
                        "code mode is not configured on this server; skip code repository entries",
                    ));
                };
                let name = if exported.display_name.trim().is_empty() {
                    decision.key.as_str()
                } else {
                    exported.display_name.as_str()
                };
                let display_name = required_field(name, "display_name", &exported.display_name)?;
                let default_base_ref =
                    required_field(name, "default_base_ref", &exported.default_base_ref)?;
                let branch_prefix = required_field(name, "branch_prefix", &exported.branch_prefix)?;
                let quick_actions = normalize_quick_actions(exported.quick_actions.clone())
                    .map_err(|error| {
                        ServerError::bad_request(format!(
                            "code repository {name}: {}",
                            error.message()
                        ))
                    })?;
                let setup_script = non_empty_script(&exported.setup_script);
                let archive_script = non_empty_script(&exported.archive_script);
                match find_repo(exported, repos) {
                    Some(existing) => {
                        if decision.action != WorkspaceConfigAction::Replace {
                            return Err(ServerError::conflict_kind(
                                "workspace_config_replace_required",
                                format!(
                                    "repository {name} already exists; choose replace to overwrite its settings, or skip to leave it"
                                ),
                            ));
                        }
                        let mut repo = existing.clone();
                        repo.display_name = display_name;
                        repo.default_base_ref = default_base_ref;
                        repo.branch_prefix = branch_prefix;
                        repo.setup_script = setup_script;
                        repo.archive_script = archive_script;
                        repo.quick_actions = quick_actions;
                        repo_writes.push(RepoWrite::Replace(Box::new(repo)));
                    }
                    None => {
                        let root = PathBuf::from(apply_repo_path(exported, &decision.remaps));
                        let checkout = check_new_checkout(name, &root, repos).await?;
                        if !planned_checkouts.insert(checkout) {
                            return Err(ServerError::bad_request_kind(
                                "workspace_config_duplicate_decision",
                                format!(
                                    "code repository {name} points at a checkout another entry already adds; skip one of them"
                                ),
                            ));
                        }
                        repo_writes.push(RepoWrite::Register {
                            root,
                            registration: RepoRegistration {
                                cloned_from: exported.cloned_from.clone(),
                                display_name: Some(display_name),
                                default_base_ref: Some(default_base_ref),
                                branch_prefix: Some(branch_prefix),
                                setup_script,
                                archive_script,
                                quick_actions,
                            },
                        });
                    }
                }
                applied += 1;
            }
        }
    }

    Ok(ApplyPlan {
        mcp: mcp_changed.then_some(mcp),
        mcp_imports,
        repos: repo_writes,
        applied,
        skipped,
    })
}

/// Refuse a remap this entry does not take, or one left blank: a blank
/// command or path would only fail later, after other entries were written.
fn check_remaps(decision: &WorkspaceConfigDecision, fields: &[&str]) -> Result<(), ServerError> {
    for (field, value) in &decision.remaps {
        if !fields.contains(&field.as_str()) {
            return Err(ServerError::bad_request(format!(
                "{}: {field} cannot be remapped",
                decision.key
            )));
        }
        if value.trim().is_empty() {
            return Err(ServerError::bad_request(format!(
                "{}: enter the {field} on this machine, or skip the entry",
                decision.key
            )));
        }
    }
    Ok(())
}

fn required_field(repo: &str, field: &str, value: &str) -> Result<String, ServerError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ServerError::bad_request(format!(
            "code repository {repo}: {field} must not be empty"
        )));
    }
    Ok(value.to_owned())
}

fn non_empty_script(script: &Option<String>) -> Option<String> {
    script
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned()
}

/// Check a repository the import would register: the path is a git checkout
/// on this machine, and not one already registered under another name. These
/// are the refusals registration itself would raise, raised before anything
/// is written. Returns the checkout's canonical top level.
async fn check_new_checkout(
    name: &str,
    root: &Path,
    repos: &[CodeRepo],
) -> Result<PathBuf, ServerError> {
    let validated = crate::code::worktree::validate_repo_path(root)
        .await
        .map_err(|error| {
            ServerError::bad_request_kind(
                "workspace_config_repository_path",
                format!(
                    "code repository {name}: {error}; set its path on this machine, or skip it"
                ),
            )
        })?;
    if repos.iter().any(|repo| {
        repo.removed_at.is_none() && Path::new(&repo.root_path) == validated.toplevel.as_path()
    }) {
        return Err(ServerError::conflict_kind(
            "repo_already_registered",
            format!(
                "code repository {name}: {} is already registered; skip this entry",
                validated.toplevel.display()
            ),
        ));
    }
    Ok(validated.toplevel)
}

async fn write_plan(
    plan: ApplyPlan,
    mcp: &crate::mcp_config::McpRuntime,
    code: Option<ScopedCode>,
    lockdown: ManualLockdown,
) -> Result<(), ServerError> {
    if let Some(servers) = plan.mcp {
        let outcome = mcp
            .replace_under_policy(McpServersConfig { servers }, lockdown)
            .await
            .map_err(ServerError::from)?;
        if let crate::mcp_config::McpReplaceOutcome::RefusedManual(refused) = outcome {
            return Err(crate::providers::managed_profile_refusal(format!(
                "this profile is managed by a model gateway; manual MCP servers are locked ({})",
                refused.join(", ")
            )));
        }
    }
    if plan.repos.is_empty() {
        return Ok(());
    }
    let Some(code) = code else {
        // The plan only holds repository writes when code mode is present.
        return Err(ServerError::internal(
            "code mode disappeared during the import",
        ));
    };
    for write in plan.repos {
        match write {
            RepoWrite::Replace(repo) => code.save_repo(&repo).await?,
            RepoWrite::Register { root, registration } => {
                code.register_repo(root, registration).await?;
            }
        }
    }
    Ok(())
}
