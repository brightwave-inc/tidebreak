//! `/plugins` — the install-wide plugin and skill management surface.
//!
//! One read route projects everything installed: each bundle with its display
//! identity, its host-derived capability badges, and its member skills with
//! their own enable state, plus the skills no bundle claims. A disabled
//! component is still listed — a management surface that hid it would offer no
//! way to turn it back on — with `enabled: false` saying so.
//!
//! One write route sets flags. It is a merge patch, not a replacement: a body
//! names only the components it means to change, so two clients toggling
//! different bundles do not clobber each other. The flags themselves live in
//! [`crate::plugin_state`]; what is enforced here is that a name is a
//! well-formed slug and that the recorded set stays bounded.

use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use openwave_code_execution::{
    derived_capabilities, is_valid_plugin_name, is_valid_skill_name, PluginCapability,
    PluginCategory, SkillOrigin, SkillPackage,
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
}

/// One bundle, as a management surface renders it.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct PluginInfo {
    /// The slug the toggle route addresses it by.
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub category: PluginCategory,
    /// What the bundle can do, derived by the host from what it contains.
    /// Never self-declared: a manifest has no key for this.
    pub capabilities: Vec<PluginCapability>,
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
    for plugin in exec.installed_plugins() {
        let members: Vec<&SkillPackage> = plugin
            .skills
            .iter()
            .filter_map(|member| by_name(member))
            .collect();
        claimed.extend(members.iter().map(|skill| skill.name.as_str()));
        plugins.push(PluginInfo {
            capabilities: derived_capabilities(&plugin, &members),
            enabled: flags.plugin_enabled(&plugin.name),
            skills: members
                .into_iter()
                .map(|skill| skill_info(skill, &flags))
                .collect(),
            name: plugin.name,
            display_name: plugin.display_name,
            description: plugin.description,
            category: plugin.category,
        });
    }
    let skills = installed
        .iter()
        .filter(|skill| !claimed.contains(&skill.name.as_str()))
        .map(|skill| skill_info(skill, &flags))
        .collect();
    Ok(Json(PluginCatalog { plugins, skills }))
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
    get_plugins(State(state)).await
}
