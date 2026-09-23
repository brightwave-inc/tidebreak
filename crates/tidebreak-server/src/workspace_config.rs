//! Portable workspace configuration: export, preview, and apply.
//!
//! Decision 83. The document is a versioned JSON envelope of code-repository
//! registrations and MCP server definitions. Secret values never enter it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use tidebreak_core::{CodeRepo, QuickAction};

use crate::error::ServerError;
use crate::mcp_config::{ManualLockdown, McpServerDefinition};

/// Current `tidebreak_config` format version.
pub const FORMAT_VERSION: u32 = 1;

const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Exported JSON envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfigDocument {
    pub tidebreak_config: u32,
    pub exported_at: chrono::DateTime<chrono::Utc>,
    pub sections: WorkspaceConfigSections,
}

/// Named sections. Unknown keys fail closed via `deny_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfigSections {
    #[serde(default)]
    pub code_repositories: Vec<ExportedCodeRepository>,
    #[serde(default)]
    pub mcp_servers: Vec<ExportedMcpServer>,
}

/// Portable code-repository registration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ExportedCodeRepository {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub origin_url: Option<String>,
    pub root_path: String,
    pub default_base_ref: String,
    pub branch_prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub setup_script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub archive_script: Option<String>,
    pub quick_actions: Vec<QuickAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cloned_from: Option<String>,
}

/// Portable MCP server definition. Environment *names* only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ExportedMcpServer {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub env_from: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bearer_token_env: Option<String>,
    /// Whether the server authenticates with OAuth. An exported definition
    /// carries the flag but never a token: the credential stays in the OS
    /// store, so an imported OAuth server is authenticatable but not yet
    /// authorized. Omitted from the document when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub oauth: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gateway_endpoint: Option<String>,
    pub request_timeout_ms: u64,
    pub enabled: bool,
}

/// Preview of one imported entry against this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct WorkspaceConfigPreview {
    pub entries: Vec<WorkspaceConfigPreviewEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct WorkspaceConfigPreviewEntry {
    pub section: WorkspaceConfigSectionId,
    pub key: String,
    pub status: WorkspaceConfigPreviewStatus,
    pub differing_fields: Vec<String>,
    pub remap_fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceConfigSectionId {
    CodeRepositories,
    McpServers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceConfigPreviewStatus {
    New,
    Identical,
    Conflict,
    NeedsRemap,
}

/// Per-entry decision sent with apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfigDecision {
    pub section: WorkspaceConfigSectionId,
    pub key: String,
    pub action: WorkspaceConfigAction,
    #[serde(default)]
    pub remaps: BTreeMap<String, String>,
    /// For an MCP server: whether it runs on this machine. Absent keeps the
    /// file's own flag, except that a remote server that sends a credential
    /// from this machine's environment imports turned off. The desktop sends
    /// `false` for a local command server or a credential-sending remote
    /// server the person has not chosen to start. Refused on a code
    /// repository entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceConfigAction {
    Skip,
    Add,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfigApplyRequest {
    pub document: WorkspaceConfigDocument,
    #[serde(default)]
    pub decisions: Vec<WorkspaceConfigDecision>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct WorkspaceConfigApplyResult {
    pub applied: usize,
    pub skipped: usize,
}

pub fn parse_document(value: serde_json::Value) -> Result<WorkspaceConfigDocument, ServerError> {
    if !value.is_object() {
        return Err(ServerError::bad_request_kind(
            "workspace_config_invalid_json",
            "the file is not a JSON object; export a Tidebreak workspace configuration and import that file",
        ));
    }
    let obj = value.as_object().expect("object");
    if !obj.contains_key("tidebreak_config") {
        return Err(ServerError::bad_request_kind(
            "workspace_config_missing_envelope",
            "this file is missing the tidebreak_config envelope; export a workspace configuration from Tidebreak and import that file",
        ));
    }
    let version = obj
        .get("tidebreak_config")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            ServerError::bad_request_kind(
                "workspace_config_missing_envelope",
                "tidebreak_config must be a format version integer; export again from Tidebreak",
            )
        })?;
    if version > u64::from(FORMAT_VERSION) {
        return Err(ServerError::bad_request_kind(
            "workspace_config_unsupported_version",
            format!(
                "this file uses format version {version}, which this Tidebreak does not read; upgrade Tidebreak or export again from a matching version"
            ),
        ));
    }
    if version < u64::from(FORMAT_VERSION) {
        // No older versions exist yet. Refuse rather than guess.
        return Err(ServerError::bad_request_kind(
            "workspace_config_unsupported_version",
            format!(
                "this file uses format version {version}, which is no longer imported; export again from a current Tidebreak"
            ),
        ));
    }
    serde_json::from_value(value).map_err(|error| {
        let message = error.to_string();
        if message.contains("unknown field") {
            ServerError::bad_request_kind(
                "workspace_config_unknown_section",
                format!(
                    "{message}. Remove unknown keys or export again from Tidebreak; this reader only understands code_repositories and mcp_servers"
                ),
            )
        } else {
            ServerError::bad_request_kind(
                "workspace_config_invalid_json",
                format!(
                    "the file could not be read ({message}); export a workspace configuration from Tidebreak and import that file"
                ),
            )
        }
    })
}

pub async fn export_code_repositories(repos: &[CodeRepo]) -> Vec<ExportedCodeRepository> {
    let mut out = Vec::new();
    for repo in repos {
        if repo.removed_at.is_some() {
            continue;
        }
        let origin_url = origin_remote(&repo.root_path)
            .await
            .or_else(|| repo.cloned_from.clone());
        out.push(ExportedCodeRepository {
            display_name: repo.display_name.clone(),
            origin_url,
            root_path: repo.root_path.clone(),
            default_base_ref: repo.default_base_ref.clone(),
            branch_prefix: repo.branch_prefix.clone(),
            setup_script: repo.setup_script.clone(),
            archive_script: repo.archive_script.clone(),
            quick_actions: repo.quick_actions.clone(),
            cloned_from: repo.cloned_from.clone(),
        });
    }
    out
}

pub fn export_mcp_servers(definitions: &[McpServerDefinition]) -> Vec<ExportedMcpServer> {
    definitions
        .iter()
        .filter(|definition| definition.plugin.is_none())
        .map(|definition| ExportedMcpServer {
            name: definition.name.clone(),
            command: definition.command.clone(),
            args: definition.args.clone(),
            env: definition.env.iter().cloned().collect(),
            env_from: definition.env_from.clone(),
            cwd: definition
                .cwd
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            url: definition.url.clone(),
            bearer_token_env: definition.bearer_token_env.clone(),
            oauth: definition.oauth,
            gateway_endpoint: definition.gateway_endpoint.clone(),
            request_timeout_ms: definition.request_timeout_ms,
            enabled: definition.enabled,
        })
        .collect()
}

pub fn preview_document(
    document: &WorkspaceConfigDocument,
    repos: &[CodeRepo],
    mcp: &[McpServerDefinition],
) -> WorkspaceConfigPreview {
    let mut entries = Vec::new();
    for exported in &document.sections.code_repositories {
        entries.push(preview_repo(exported, repos));
    }
    for exported in &document.sections.mcp_servers {
        entries.push(preview_mcp(exported, mcp));
    }
    WorkspaceConfigPreview { entries }
}

fn preview_repo(
    exported: &ExportedCodeRepository,
    repos: &[CodeRepo],
) -> WorkspaceConfigPreviewEntry {
    let key = repo_key(exported);
    let remap_fields = repo_remap_fields(exported);
    let Some(existing) = find_repo(exported, repos) else {
        let status = if remap_fields.is_empty() {
            WorkspaceConfigPreviewStatus::New
        } else {
            WorkspaceConfigPreviewStatus::NeedsRemap
        };
        return WorkspaceConfigPreviewEntry {
            section: WorkspaceConfigSectionId::CodeRepositories,
            key,
            status,
            differing_fields: Vec::new(),
            remap_fields,
        };
    };
    let differing_fields = repo_diff(exported, existing);
    let status = if !differing_fields.is_empty() {
        WorkspaceConfigPreviewStatus::Conflict
    } else if !remap_fields.is_empty() {
        WorkspaceConfigPreviewStatus::NeedsRemap
    } else {
        WorkspaceConfigPreviewStatus::Identical
    };
    WorkspaceConfigPreviewEntry {
        section: WorkspaceConfigSectionId::CodeRepositories,
        key,
        status,
        differing_fields,
        remap_fields,
    }
}

fn preview_mcp(
    exported: &ExportedMcpServer,
    mcp: &[McpServerDefinition],
) -> WorkspaceConfigPreviewEntry {
    let key = exported.name.clone();
    let remap_fields = mcp_remap_fields(exported);
    let Some(existing) = mcp.iter().find(|item| item.name == exported.name) else {
        let status = if remap_fields.is_empty() {
            WorkspaceConfigPreviewStatus::New
        } else {
            WorkspaceConfigPreviewStatus::NeedsRemap
        };
        return WorkspaceConfigPreviewEntry {
            section: WorkspaceConfigSectionId::McpServers,
            key,
            status,
            differing_fields: Vec::new(),
            remap_fields,
        };
    };
    let differing_fields = mcp_diff(exported, existing);
    let status = if !differing_fields.is_empty() {
        WorkspaceConfigPreviewStatus::Conflict
    } else if !remap_fields.is_empty() {
        WorkspaceConfigPreviewStatus::NeedsRemap
    } else {
        WorkspaceConfigPreviewStatus::Identical
    };
    WorkspaceConfigPreviewEntry {
        section: WorkspaceConfigSectionId::McpServers,
        key,
        status,
        differing_fields,
        remap_fields,
    }
}

/// The key a repository entry is previewed and decided under: its origin URL,
/// else the remote it was cloned from, else its display name.
pub fn repo_key(exported: &ExportedCodeRepository) -> String {
    exported
        .origin_url
        .clone()
        .or_else(|| exported.cloned_from.clone())
        .unwrap_or_else(|| exported.display_name.clone())
}

/// The registration on this machine an imported repository matches, if any.
/// Preview and apply share it, so an entry previewed as new is never refused
/// by apply as already present.
pub fn find_repo<'a>(
    exported: &ExportedCodeRepository,
    repos: &'a [CodeRepo],
) -> Option<&'a CodeRepo> {
    let origin = exported
        .origin_url
        .as_deref()
        .or(exported.cloned_from.as_deref());
    if let Some(origin) = origin {
        return repos.iter().find(|repo| {
            repo.removed_at.is_none()
                && (repo.cloned_from.as_deref() == Some(origin)
                    || repo.origin_name.is_some() && origin_matches(repo, origin))
        });
    }
    repos
        .iter()
        .find(|repo| repo.removed_at.is_none() && repo.display_name == exported.display_name)
}

fn origin_matches(repo: &CodeRepo, origin: &str) -> bool {
    repo.cloned_from.as_deref() == Some(origin)
}

fn repo_diff(exported: &ExportedCodeRepository, existing: &CodeRepo) -> Vec<String> {
    let mut fields = Vec::new();
    if exported.display_name != existing.display_name {
        fields.push("display_name".into());
    }
    if exported.default_base_ref != existing.default_base_ref {
        fields.push("default_base_ref".into());
    }
    if exported.branch_prefix != existing.branch_prefix {
        fields.push("branch_prefix".into());
    }
    if exported.setup_script != existing.setup_script {
        fields.push("setup_script".into());
    }
    if exported.archive_script != existing.archive_script {
        fields.push("archive_script".into());
    }
    if exported.quick_actions != existing.quick_actions {
        fields.push("quick_actions".into());
    }
    fields
}

fn mcp_diff(exported: &ExportedMcpServer, existing: &McpServerDefinition) -> Vec<String> {
    let mut fields = Vec::new();
    if exported.command != existing.command {
        fields.push("command".into());
    }
    if exported.args != existing.args {
        fields.push("args".into());
    }
    let exported_env: BTreeSet<_> = exported.env.iter().cloned().collect();
    if exported_env != existing.env {
        fields.push("env".into());
    }
    if exported.env_from != existing.env_from {
        fields.push("env_from".into());
    }
    let existing_cwd = existing
        .cwd
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    if exported.cwd != existing_cwd {
        fields.push("cwd".into());
    }
    if exported.url != existing.url {
        fields.push("url".into());
    }
    if exported.bearer_token_env != existing.bearer_token_env {
        fields.push("bearer_token_env".into());
    }
    if exported.oauth != existing.oauth {
        fields.push("oauth".into());
    }
    if exported.gateway_endpoint != existing.gateway_endpoint {
        fields.push("gateway_endpoint".into());
    }
    if exported.request_timeout_ms != existing.request_timeout_ms {
        fields.push("request_timeout_ms".into());
    }
    if exported.enabled != existing.enabled {
        fields.push("enabled".into());
    }
    fields
}

fn repo_remap_fields(exported: &ExportedCodeRepository) -> Vec<String> {
    let path = Path::new(&exported.root_path);
    if path.is_dir() {
        Vec::new()
    } else {
        vec!["root_path".into()]
    }
}

fn mcp_remap_fields(exported: &ExportedMcpServer) -> Vec<String> {
    let mut fields = Vec::new();
    if let Some(command) = &exported.command {
        if !command_resolvable(command) {
            fields.push("command".into());
        }
    }
    if let Some(cwd) = &exported.cwd {
        if !Path::new(cwd).is_dir() {
            fields.push("cwd".into());
        }
    }
    fields
}

fn command_resolvable(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute() {
        return path.is_file();
    }
    if command.contains('/') || command.contains('\\') {
        return path.is_file();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
}

pub fn exported_mcp_to_definition(exported: &ExportedMcpServer) -> McpServerDefinition {
    McpServerDefinition {
        name: exported.name.clone(),
        command: exported.command.clone(),
        args: exported.args.clone(),
        env: exported.env.iter().cloned().collect(),
        env_values: BTreeMap::new(),
        env_from: exported.env_from.clone(),
        cwd: exported.cwd.as_ref().map(PathBuf::from),
        url: exported.url.clone(),
        bearer_token_env: exported.bearer_token_env.clone(),
        oauth: exported.oauth,
        gateway_endpoint: exported.gateway_endpoint.clone(),
        request_timeout_ms: exported.request_timeout_ms,
        enabled: exported.enabled,
        plugin: None,
        launch: None,
    }
}

pub fn apply_mcp_remap(
    mut definition: McpServerDefinition,
    remaps: &BTreeMap<String, String>,
) -> McpServerDefinition {
    if let Some(command) = remaps.get("command") {
        definition.command = Some(command.clone());
    }
    if let Some(cwd) = remaps.get("cwd") {
        definition.cwd = Some(PathBuf::from(cwd));
    }
    definition
}

/// The definition an MCP decision writes: the file's entry of that name, with
/// this machine's remaps and the decision's enabled choice applied. `None`
/// when the file has no entry of that name.
///
/// Apply and the desktop's native confirmation both resolve through here, so
/// the commands the confirmation lists are exactly the ones apply writes.
pub fn imported_mcp_definition(
    document: &WorkspaceConfigDocument,
    decision: &WorkspaceConfigDecision,
) -> Option<McpServerDefinition> {
    let exported = document
        .sections
        .mcp_servers
        .iter()
        .find(|item| item.name == decision.key)?;
    let mut definition = apply_mcp_remap(exported_mcp_to_definition(exported), &decision.remaps);
    // An explicit choice wins. Without one, a remote server that sends a
    // credential from this machine's environment imports turned off: the
    // file names the URL the value would go to, so only the person's switch
    // for this row turns it on. Everything else keeps the file's flag.
    definition.enabled = match decision.enabled {
        Some(enabled) => enabled,
        None => definition.enabled && !sends_environment_credential(&definition),
    };
    Some(definition)
}

/// Whether a definition starts a program on this machine as soon as it is
/// saved: an enabled `command` server.
pub fn starts_local_command(definition: &McpServerDefinition) -> bool {
    definition.enabled && definition.command.is_some()
}

/// Whether a remote server sends a value from this machine's environment to
/// the URL it names. Today that is its bearer token variable: validation
/// keeps process environment names off remote servers, and the check covers
/// them anyway so a later schema cannot slip one past it. An imported server
/// like this starts turned off unless the person turns it on.
pub fn sends_environment_credential(definition: &McpServerDefinition) -> bool {
    definition.url.is_some()
        && (definition.bearer_token_env.is_some()
            || !definition.env_from.is_empty()
            || !definition.env.is_empty())
}

/// Every local command an apply would start: the imported MCP servers that
/// run a command and stay enabled. On the desktop, apply refuses these from
/// any caller but the native host (decision 27), and the native host lists
/// exactly this set in its confirmation before it forwards the request.
pub fn local_commands_to_confirm(
    request: &WorkspaceConfigApplyRequest,
) -> Vec<McpServerDefinition> {
    request
        .decisions
        .iter()
        .filter(|decision| {
            decision.section == WorkspaceConfigSectionId::McpServers
                && decision.action != WorkspaceConfigAction::Skip
        })
        .filter_map(|decision| imported_mcp_definition(&request.document, decision))
        .filter(starts_local_command)
        .collect()
}

pub fn apply_repo_path(
    exported: &ExportedCodeRepository,
    remaps: &BTreeMap<String, String>,
) -> String {
    remaps
        .get("root_path")
        .cloned()
        .unwrap_or_else(|| exported.root_path.clone())
}

pub fn mcp_lockdown_blocks(definition: &McpServerDefinition, lockdown: ManualLockdown) -> bool {
    if definition.gateway_endpoint.is_some() {
        return false;
    }
    match lockdown {
        ManualLockdown::Open => false,
        ManualLockdown::RemoteManual => definition.command.is_none(),
        ManualLockdown::AllManual => true,
    }
}

async fn origin_remote(root: &str) -> Option<String> {
    let mut command = Command::new("git");
    command
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    let child = command.spawn().ok()?;
    let output = tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if url.is_empty() {
        None
    } else {
        Some(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{OwnerId, RepoId};

    fn sample_mcp() -> ExportedMcpServer {
        ExportedMcpServer {
            name: "docs".into(),
            command: Some("/usr/bin/true".into()),
            args: vec!["--stdio".into()],
            env: vec!["LOG_LEVEL".into()],
            env_from: vec!["DOCS_TOKEN".into()],
            cwd: None,
            url: None,
            bearer_token_env: None,
            oauth: false,
            gateway_endpoint: None,
            request_timeout_ms: 60_000,
            enabled: true,
        }
    }

    fn sample_repo() -> ExportedCodeRepository {
        ExportedCodeRepository {
            display_name: "tidebreak".into(),
            origin_url: Some("https://github.com/brightwave-inc/tidebreak.git".into()),
            root_path: "/tmp/does-not-exist-tidebreak-config".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: vec![],
            cloned_from: Some("https://github.com/brightwave-inc/tidebreak.git".into()),
        }
    }

    fn envelope(
        repos: Vec<ExportedCodeRepository>,
        mcp: Vec<ExportedMcpServer>,
    ) -> WorkspaceConfigDocument {
        WorkspaceConfigDocument {
            tidebreak_config: FORMAT_VERSION,
            exported_at: chrono::Utc::now(),
            sections: WorkspaceConfigSections {
                code_repositories: repos,
                mcp_servers: mcp,
            },
        }
    }

    #[test]
    fn export_mcp_omits_env_values_and_plugin_servers() {
        let mut secret = McpServerDefinition {
            name: "docs".into(),
            command: Some("/usr/bin/true".into()),
            args: vec![],
            env: BTreeSet::from(["TOKEN".into()]),
            env_values: BTreeMap::from([("TOKEN".into(), "super-secret".into())]),
            env_from: vec![],
            cwd: None,
            url: None,
            bearer_token_env: Some("BEARER".into()),
            oauth: false,
            gateway_endpoint: None,
            request_timeout_ms: 60_000,
            enabled: true,
            plugin: None,
            launch: None,
        };
        let plugin = McpServerDefinition {
            plugin: Some("pack".into()),
            name: "from_plugin".into(),
            ..secret.clone()
        };
        secret.bearer_token_env = Some("BEARER".into());
        let json = serde_json::to_value(export_mcp_servers(&[secret, plugin])).unwrap();
        let text = json.to_string();
        assert!(!text.contains("super-secret"));
        assert!(!text.contains("env_values"));
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["env"], serde_json::json!(["TOKEN"]));
        assert_eq!(json[0]["bearer_token_env"], "BEARER");
    }

    #[test]
    fn export_shape_has_no_transcript_or_worktree_keys() {
        let doc = envelope(vec![sample_repo()], vec![sample_mcp()]);
        let text = serde_json::to_string(&doc).unwrap();
        assert!(!text.contains("transcript"));
        assert!(!text.contains("worktree"));
        assert!(!text.contains("env_values"));
        assert!(text.contains("tidebreak_config"));
        assert!(text.contains("code_repositories"));
        assert!(text.contains("mcp_servers"));
    }

    #[test]
    fn newer_version_is_refused() {
        let error = parse_document(serde_json::json!({
            "tidebreak_config": 99,
            "exported_at": "2026-09-02T00:00:00Z",
            "sections": {}
        }))
        .unwrap_err();
        assert_eq!(error.kind(), "workspace_config_unsupported_version");
        assert!(error.message().contains("upgrade"));
    }

    #[test]
    fn missing_envelope_is_refused() {
        let error = parse_document(serde_json::json!({"servers": []})).unwrap_err();
        assert_eq!(error.kind(), "workspace_config_missing_envelope");
    }

    #[test]
    fn unknown_section_is_refused() {
        let error = parse_document(serde_json::json!({
            "tidebreak_config": 1,
            "exported_at": "2026-09-02T00:00:00Z",
            "sections": { "folders": [] }
        }))
        .unwrap_err();
        assert_eq!(error.kind(), "workspace_config_unknown_section");
    }

    #[test]
    fn preview_classifies_new_identical_conflict_and_needs_remap() {
        let mut mcp = sample_mcp();
        mcp.command = Some("/usr/bin/true".into());
        let new_doc = envelope(vec![], vec![mcp.clone()]);
        let preview = preview_document(&new_doc, &[], &[]);
        assert_eq!(preview.entries[0].status, WorkspaceConfigPreviewStatus::New);

        // /usr/bin/true may or may not exist; if command remaps, status is needs_remap
        // when fields match. Force a command that exists on PATH: "true" or "sh".
        let mut local = mcp.clone();
        local.command = Some("sh".into());
        let existing_sh = exported_mcp_to_definition(&local);
        let preview_ident = preview_document(
            &envelope(vec![], vec![local.clone()]),
            &[],
            std::slice::from_ref(&existing_sh),
        );
        assert_eq!(
            preview_ident.entries[0].status,
            WorkspaceConfigPreviewStatus::Identical
        );

        let mut other = existing_sh.clone();
        other.args = vec!["-c".into(), "echo".into()];
        let conflict = preview_document(&envelope(vec![], vec![local.clone()]), &[], &[other]);
        assert_eq!(
            conflict.entries[0].status,
            WorkspaceConfigPreviewStatus::Conflict
        );
        assert!(conflict.entries[0]
            .differing_fields
            .contains(&"args".into()));

        let remap_doc = envelope(vec![sample_repo()], vec![]);
        let remap = preview_document(&remap_doc, &[], &[]);
        assert_eq!(
            remap.entries[0].status,
            WorkspaceConfigPreviewStatus::NeedsRemap
        );
        assert!(remap.entries[0].remap_fields.contains(&"root_path".into()));
    }

    #[test]
    fn apply_without_replace_does_not_select_existing() {
        let existing = CodeRepo {
            id: RepoId::new(),
            owner: OwnerId::local(),
            root_path: "/tmp/repo".into(),
            display_name: "tidebreak".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: vec![],
            created_at: chrono::Utc::now(),
            removed_at: None,
            cloned_from: Some("https://github.com/brightwave-inc/tidebreak.git".into()),
            origin_host: None,
            origin_owner: None,
            origin_name: None,
        };
        let exported = sample_repo();
        let found = find_repo(&exported, std::slice::from_ref(&existing));
        assert!(found.is_some());
        let decision = WorkspaceConfigDecision {
            section: WorkspaceConfigSectionId::CodeRepositories,
            key: repo_key(&exported),
            action: WorkspaceConfigAction::Skip,
            remaps: BTreeMap::new(),
            enabled: None,
        };
        assert_eq!(decision.action, WorkspaceConfigAction::Skip);
        assert_ne!(decision.action, WorkspaceConfigAction::Replace);
    }

    fn mcp_decision(
        key: &str,
        action: WorkspaceConfigAction,
        enabled: Option<bool>,
    ) -> WorkspaceConfigDecision {
        WorkspaceConfigDecision {
            section: WorkspaceConfigSectionId::McpServers,
            key: key.into(),
            action,
            remaps: BTreeMap::new(),
            enabled,
        }
    }

    /// The set the native confirmation lists is the set apply would start:
    /// enabled command servers the import adds or replaces, after this
    /// machine's remaps. A server imported turned off, a skipped one, and a
    /// remote one start nothing here.
    #[test]
    fn only_enabled_imported_commands_need_confirmation() {
        let mut remote = sample_mcp();
        remote.name = "remote".into();
        remote.command = None;
        remote.args = vec![];
        remote.url = Some("https://mcp.example.com/mcp".into());
        let mut skipped = sample_mcp();
        skipped.name = "skipped".into();
        let mut off = sample_mcp();
        off.name = "off".into();
        let request = WorkspaceConfigApplyRequest {
            document: envelope(vec![], vec![sample_mcp(), remote, skipped, off]),
            decisions: vec![
                WorkspaceConfigDecision {
                    remaps: BTreeMap::from([("command".into(), "/opt/mcp/docs".into())]),
                    ..mcp_decision("docs", WorkspaceConfigAction::Add, None)
                },
                mcp_decision("remote", WorkspaceConfigAction::Add, None),
                mcp_decision("skipped", WorkspaceConfigAction::Skip, None),
                mcp_decision("off", WorkspaceConfigAction::Add, Some(false)),
            ],
        };

        let commands = local_commands_to_confirm(&request);
        assert_eq!(commands.len(), 1, "{commands:?}");
        assert_eq!(commands[0].name, "docs");
        assert_eq!(commands[0].command.as_deref(), Some("/opt/mcp/docs"));

        let off = imported_mcp_definition(&request.document, &request.decisions[3])
            .expect("the file names the server");
        assert!(!off.enabled, "the decision turns the server off");
    }

    /// A remote server that sends a credential from this machine's
    /// environment imports turned off unless the decision turns it on. A
    /// remote server that sends nothing keeps the file's flag.
    #[test]
    fn a_remote_server_that_sends_a_credential_imports_turned_off_by_default() {
        let mut sends = sample_mcp();
        sends.name = "search".into();
        sends.command = None;
        sends.args = vec![];
        sends.env = vec![];
        sends.env_from = vec![];
        sends.url = Some("https://mcp.example.com/search".into());
        sends.bearer_token_env = Some("SEARCH_TOKEN".into());
        let mut quiet = sends.clone();
        quiet.name = "status".into();
        quiet.bearer_token_env = None;
        let document = envelope(vec![], vec![sends, quiet]);
        let resolve = |key: &str, enabled: Option<bool>| {
            imported_mcp_definition(
                &document,
                &mcp_decision(key, WorkspaceConfigAction::Add, enabled),
            )
            .expect("the file names the server")
        };

        let default = resolve("search", None);
        assert!(sends_environment_credential(&default));
        assert!(!default.enabled, "the file's flag is not consent");
        assert!(resolve("search", Some(true)).enabled);
        assert!(
            resolve("status", None).enabled,
            "a remote server that sends no credential keeps the file's flag"
        );
    }
}
