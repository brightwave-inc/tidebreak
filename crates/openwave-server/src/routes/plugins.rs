//! `/plugins` — the install-wide plugin, skill, and prompt management surface.
//!
//! One read route projects everything installed: each bundle with its display
//! identity, its host-derived capability badges, and its member skills with
//! their own enable state, plus the skills no bundle claims. A disabled
//! component is still listed — a management surface that hid it would offer no
//! way to turn it back on — with `enabled: false` saying so.
//!
//! Reusable prompts ride the same read route as a flat list. They are
//! user-side text with no catalog line and no staged bytes, so there is
//! nothing to gate per prompt: a bundled one follows its bundle's flag and a
//! standalone one is always offered. Bodies come from their own route, fetched
//! when a prompt is actually picked.
//!
//! One write route sets flags. It is a merge patch, not a replacement: a body
//! names only the components it means to change, so two clients toggling
//! different bundles do not clobber each other. The flags themselves live in
//! [`crate::plugin_state`]; what is enforced here is that a name is a
//! well-formed slug and that the recorded set stays bounded.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use openwave_code_execution::{
    derived_capabilities, is_valid_plugin_name, is_valid_prompt_name, is_valid_skill_name,
    PluginCapability, PluginCategory, PluginOrigin, PromptOrigin, PromptPackage, SkillOrigin,
    SkillPackage,
};

use crate::error::ServerError;
use crate::extract::Json;
use crate::plugin_state::{read_plugin_enable_state, write_plugin_enable_state};
use crate::state::AppState;

/// Body bound for the toggle route: a merge patch over slug-keyed booleans,
/// so even a body naming every recordable component is small.
pub const MAX_PLUGIN_ENABLE_BODY_BYTES: usize = 64 * 1024;

/// Everything this installation has, in the state it is in.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct PluginCatalog {
    /// Bundles in load order (by slug), each with its members.
    pub plugins: Vec<PluginInfo>,
    /// Skills no bundle claims — user-authored packages land here.
    pub skills: Vec<PluginSkillInfo>,
    /// Every installed prompt, bundled or standalone, in one flat list.
    ///
    /// Flat rather than nested under its bundle because the consumer is a
    /// picker over the whole library; a plugin's members are the entries whose
    /// `plugin` names it.
    pub prompts: Vec<PluginPromptInfo>,
}

/// One bundle, as a management surface renders it.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct PluginInfo {
    /// The slug the toggle route addresses it by.
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub category: PluginCategory,
    /// Where the bundle was loaded from; host-derived, never claimed.
    pub origin: PluginOrigin,
    /// What the bundle can do, derived by the host from what it contains.
    /// Never self-declared: a manifest has no key for this.
    pub capabilities: Vec<PluginCapability>,
    /// Import-time static compatibility disclosure. A hand-authored bundle is
    /// explicitly unchecked; imported bundles say whether they fit the
    /// prepared sandbox image and why not.
    pub compatibility: openwave_code_execution::PluginCompatibility,
    /// Whether the bundle is on. Off gates every member regardless of the
    /// member's own flag, which the member entries still report unchanged.
    pub enabled: bool,
    /// Member skills in manifest order.
    pub skills: Vec<PluginSkillInfo>,
}

/// One skill, inside a bundle or standing alone.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct PluginSkillInfo {
    pub name: String,
    pub description: String,
    /// Where the package was loaded from; host-derived, never claimed.
    pub origin: SkillOrigin,
    /// The skill's *own* flag, independent of any owning bundle's — so a UI
    /// can show the member choices that come back when a bundle is re-enabled.
    pub enabled: bool,
}

/// One reusable prompt, as a picker or a management surface renders it.
///
/// Deliberately without a body: the text is fetched from
/// [`get_prompt_body`] when the user actually picks one, so the catalog stays
/// bytes per entry no matter how long the prompts are.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct PluginPromptInfo {
    /// The slug the body route addresses it by.
    pub name: String,
    /// The tip a card or popover shows.
    pub description: String,
    /// Where the package was loaded from; host-derived, never claimed.
    pub origin: PromptOrigin,
    /// The bundle that claims this prompt, if any. `None` is a standalone
    /// package — every user-authored prompt is one.
    pub plugin: Option<String>,
    /// Whether the prompt is offered. A prompt has no flag of its own: a
    /// bundled one follows its bundle, and a standalone one is always on.
    pub enabled: bool,
}

/// Body of `PUT /plugins/enabled`. Absent names are left alone.
#[derive(Debug, Default, Deserialize, ts_rs::TS)]
pub struct PluginEnableUpdate {
    /// Bundle flags to set, by slug.
    #[serde(default)]
    pub plugins: BTreeMap<String, bool>,
    /// Skill flags to set, by slug. Setting one inside a disabled bundle is
    /// allowed and remembered; it takes effect when the bundle comes back.
    #[serde(default)]
    pub skills: BTreeMap<String, bool>,
}

/// `GET /plugins` — the installed catalog with its current enable state.
pub async fn get_plugins(
    State(state): State<AppState>,
) -> Result<Json<PluginCatalog>, ServerError> {
    let flags = read_plugin_enable_state(&*state.store).await;
    let Some(exec) = state.code_execution.as_ref() else {
        // An embedding with no code execution stages no skills and advertises
        // none; an empty catalog is the honest answer, not an error.
        return Ok(Json(PluginCatalog {
            plugins: Vec::new(),
            skills: Vec::new(),
            prompts: Vec::new(),
        }));
    };
    let installed: Vec<SkillPackage> = exec
        .installed_skills()
        .into_iter()
        .map(|skill| skill.package)
        .collect();
    let by_name = |name: &str| installed.iter().find(|skill| skill.name == name);

    let mut claimed: Vec<&str> = Vec::new();
    let mut plugins = Vec::new();
    // Which bundle claims each prompt, and whether that bundle is on. Built
    // while walking the bundles so the flat prompt list below can attribute
    // and gate each entry without a second pass over the manifests.
    let mut prompt_owners: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for plugin in exec.installed_plugins() {
        let members: Vec<&SkillPackage> = plugin
            .skills
            .iter()
            .filter_map(|member| by_name(member))
            .collect();
        claimed.extend(members.iter().map(|skill| skill.name.as_str()));
        let plugin_enabled = flags.plugin_enabled(&plugin.name);
        for prompt in &plugin.prompts {
            prompt_owners.insert(prompt.clone(), (plugin.name.clone(), plugin_enabled));
        }
        plugins.push(PluginInfo {
            capabilities: derived_capabilities(&plugin, &members),
            compatibility: plugin.compatibility.clone(),
            enabled: plugin_enabled,
            skills: members
                .into_iter()
                .map(|skill| skill_info(skill, &flags))
                .collect(),
            name: plugin.name,
            display_name: plugin.display_name,
            description: plugin.description,
            category: plugin.category,
            origin: plugin.origin,
        });
    }
    let skills = installed
        .iter()
        .filter(|skill| !claimed.contains(&skill.name.as_str()))
        .map(|skill| skill_info(skill, &flags))
        .collect();
    let prompts = exec
        .installed_prompts()
        .into_iter()
        .map(|prompt| prompt_info(&prompt.package, &prompt_owners))
        .collect();
    Ok(Json(PluginCatalog {
        plugins,
        skills,
        prompts,
    }))
}

/// `POST /plugins/install` — fetch and install one pinned instruction-only
/// plugin, returning the import-specific compatibility and skip disclosures.
pub async fn post_plugin_install(
    State(state): State<AppState>,
    Json(body): Json<crate::plugin_install::PluginInstallRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::plugin_install::PluginInstallOutcome>,
    ),
    ServerError,
> {
    let exec = state.code_execution.as_ref().ok_or_else(|| {
        ServerError::conflict_kind(
            "plugin_install_unavailable",
            "plugin installation requires code execution",
        )
    })?;
    let installed = exec
        .install_plugin(&body)
        .await
        .map_err(|error| match error {
            crate::plugin_install::PluginInstallError::InvalidSource(message) => {
                ServerError::bad_request_kind("plugin_source_invalid", message)
            }
            crate::plugin_install::PluginInstallError::Fetch(message) => {
                ServerError::unprocessable_kind("plugin_source_unavailable", message)
            }
            crate::plugin_install::PluginInstallError::InvalidArchive(message)
            | crate::plugin_install::PluginInstallError::InvalidPlugin(message) => {
                ServerError::unprocessable_kind("plugin_invalid", message)
            }
            crate::plugin_install::PluginInstallError::Conflict(message) => {
                ServerError::conflict_kind("plugin_conflict", message)
            }
            crate::plugin_install::PluginInstallError::Io(error) => {
                tracing::error!(%error, "plugin install could not publish validated files");
                ServerError::internal("plugin files could not be installed")
            }
        })?;
    // A freshly installed plugin may ship bundled MCP servers; bring them up
    // now rather than at the next restart.
    state.mcp.reconcile_plugin_servers().await;
    Ok((StatusCode::CREATED, Json(installed)))
}

fn prompt_info(
    prompt: &PromptPackage,
    owners: &BTreeMap<String, (String, bool)>,
) -> PluginPromptInfo {
    let owner = owners.get(&prompt.name);
    PluginPromptInfo {
        name: prompt.name.clone(),
        description: prompt.description.clone(),
        origin: prompt.origin,
        plugin: owner.map(|(plugin, _)| plugin.clone()),
        // A standalone prompt has nothing that could switch it off yet.
        enabled: owner.is_none_or(|(_, enabled)| *enabled),
    }
}

fn skill_info(
    skill: &SkillPackage,
    flags: &crate::plugin_state::PluginEnableState,
) -> PluginSkillInfo {
    PluginSkillInfo {
        name: skill.name.clone(),
        description: skill.description.clone(),
        origin: skill.origin,
        enabled: flags.skill_flag(&skill.name),
    }
}

/// One skill's instruction body, for the management surface's detail view.
///
/// Its own route rather than a catalog field on purpose: a body is kilobytes
/// where a catalog row is bytes, and the catalog is fetched far more often
/// than any one skill is read.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct SkillInstructions {
    pub name: String,
    /// The `SKILL.md` markdown body, with the frontmatter removed — what the
    /// model is taught when the skill is staged, shown to the reader verbatim.
    pub instructions: String,
}

/// `GET /plugins/skills/{name}/instructions`.
pub async fn get_skill_instructions(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<SkillInstructions>, ServerError> {
    if !is_valid_skill_name(&name) {
        return Err(ServerError::bad_request(format!(
            "not a skill name: {name:?}"
        )));
    }
    let skill = state
        .code_execution
        .as_ref()
        .and_then(|exec| {
            exec.installed_skills()
                .into_iter()
                .find(|skill| skill.package.name == name)
        })
        .ok_or_else(|| ServerError::not_found(format!("no skill named {name:?}")))?;
    Ok(Json(SkillInstructions {
        name,
        instructions: manifest_body(&skill.manifest).to_owned(),
    }))
}

/// The manifest with its frontmatter fences removed. Every staged manifest
/// parsed on the way in, so the split always succeeds; falling back to the
/// whole source keeps a malformed one readable rather than blank.
fn manifest_body(manifest: &str) -> &str {
    manifest
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(manifest, |(_, body)| body)
        .trim()
}

/// One prompt's insertable text, fetched when the user picks it.
///
/// Its own route for the same reason skill instructions have one: the catalog
/// is fetched far more often than any one prompt is inserted.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct PromptBody {
    pub name: String,
    /// The `PROMPT.md` markdown below the frontmatter — exactly what goes into
    /// the composer. It is never composed into the model's operating prompt;
    /// it reaches a model only if the user sends the message.
    pub body: String,
}

/// `GET /plugins/prompts/{name}/body`.
pub async fn get_prompt_body(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PromptBody>, ServerError> {
    if !is_valid_prompt_name(&name) {
        return Err(ServerError::bad_request(format!(
            "not a prompt name: {name:?}"
        )));
    }
    let prompt = state
        .code_execution
        .as_ref()
        .and_then(|exec| {
            exec.installed_prompts()
                .into_iter()
                .find(|prompt| prompt.package.name == name)
        })
        .ok_or_else(|| ServerError::not_found(format!("no prompt named {name:?}")))?;
    Ok(Json(PromptBody {
        name,
        body: prompt.body,
    }))
}

/// `PUT /plugins/enabled` — set the named flags, returning the fresh catalog.
///
/// Names are checked against the slug grammar rather than against what is
/// installed: a flag for a bundle that is not present today is a legitimate
/// thing to record — reinstalling it should not silently re-enable it — while
/// an unbounded or malformed name is not.
pub async fn put_plugins_enabled(
    State(state): State<AppState>,
    Json(body): Json<PluginEnableUpdate>,
) -> Result<Json<PluginCatalog>, ServerError> {
    let mut flags = read_plugin_enable_state(&*state.store).await;
    let before = flags.clone();
    for (plugin, enabled) in &body.plugins {
        if !is_valid_plugin_name(plugin) {
            return Err(ServerError::bad_request(format!(
                "not a plugin name: {plugin:?}"
            )));
        }
        flags.set_plugin(plugin, *enabled);
    }
    for (skill, enabled) in &body.skills {
        if !is_valid_skill_name(skill) {
            return Err(ServerError::bad_request(format!(
                "not a skill name: {skill:?}"
            )));
        }
        flags.set_skill(skill, *enabled);
    }
    if !flags.within_bounds() {
        return Err(ServerError::bad_request(
            "too many components are switched off",
        ));
    }
    write_plugin_enable_state(&*state.store, &flags).await?;
    // Enabling a plugin connects its bundled MCP servers and mounts their
    // tools; disabling one disconnects them. The flags are the only control a
    // plugin-sourced server has, so this has to happen on the same write that
    // moved them, not on the next restart.
    state.mcp.reconcile_plugin_servers().await;
    // Anything that just came live should start becoming real now rather than
    // mid-conversation: the host tools its manifest needs and the pinned
    // packages it installs are provisioned in the background. Liveness is
    // compared rather than the flags themselves, because enabling a bundle
    // brings its members back without touching a single skill flag. The pass
    // only spawns work, so the response is not delayed by it.
    if let Some(exec) = state.code_execution.as_ref() {
        let live_before = exec.live_skill_names(&before);
        if exec
            .live_skill_names(&flags)
            .iter()
            .any(|skill| !live_before.contains(skill))
        {
            exec.spawn_dependency_provisioning();
        }
    }
    get_plugins(State(state)).await
}
