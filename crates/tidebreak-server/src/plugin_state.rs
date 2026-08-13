//! Install-wide enable flags for plugins and the skills they bundle.
//!
//! State is global, not per-chat: turning a bundle off is a statement about
//! this installation, and a later slice can add per-chat overrides on top of
//! it. It rides the same `setting` key-value row every other install-wide
//! preference uses (the chat model, the background-agent ceiling), because a
//! handful of booleans does not earn a table of its own.
//!
//! Built-in and hand-authored packages default to enabled: only a *disabled*
//! component is persisted. That keeps the row proportional to what the user
//! actually turned off, and it gives the plugin gate the behavior the product
//! wants for free: a plugin's flag is a gate *over* its members' own flags
//! rather than a rewrite of them, so re-enabling a bundle restores exactly the
//! member choices that were in place when it was switched off.
//!
//! A public import is the exception. The installer records the new plugin as
//! disabled before anything can start its bundled MCP servers; enabling it is
//! the consent to launch them. Absent still means enabled, so that recording
//! has to be a stored `false`, not a change to this default.
//!
//! A disabled component must actually bite, which means both consumers read
//! this: the workspace staging pass never writes it into a sandbox, and the
//! prompt catalog never advertises it. Both come from the one filtered read in
//! [`crate::code_execution`], so the two can't disagree.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tidebreak_core::Store;

/// The `setting` row the flags live in.
pub(crate) const PLUGIN_ENABLE_STATE_SETTING: &str = "plugins.enable_state";

/// How many disabled components one install may record, per kind. A bound, not
/// a product limit: nothing legitimate approaches it, and it stops a client
/// from growing the settings row without end.
pub(crate) const MAX_DISABLED_COMPONENTS: usize = 512;

/// Which plugins and skills this installation has switched off.
///
/// Absent means enabled, so a fresh install and an install that has never
/// touched a toggle are the same state.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PluginEnableState {
    /// Plugin slugs the user turned off. Only `false` entries are kept.
    #[serde(default)]
    plugins: BTreeMap<String, bool>,
    /// Skill slugs the user turned off, whether the skill belongs to a plugin
    /// or stands alone. Kept independently of the plugin flag above.
    #[serde(default)]
    skills: BTreeMap<String, bool>,
}

impl PluginEnableState {
    /// Whether the bundle itself is on.
    pub(crate) fn plugin_enabled(&self, plugin: &str) -> bool {
        self.plugins.get(plugin).copied().unwrap_or(true)
    }

    /// Whether the skill's own flag is on, ignoring any owning plugin.
    pub(crate) fn skill_flag(&self, skill: &str) -> bool {
        self.skills.get(skill).copied().unwrap_or(true)
    }

    /// Whether the skill is live: its own flag is on *and* the bundle that
    /// claims it, if any, is on. A skill no plugin claims is gated only by its
    /// own flag.
    pub(crate) fn skill_enabled(&self, skill: &str, owner: Option<&str>) -> bool {
        self.skill_flag(skill) && owner.is_none_or(|plugin| self.plugin_enabled(plugin))
    }

    /// Record a plugin's flag. Enabling drops the row rather than storing
    /// `true`, since absent already means enabled.
    pub(crate) fn set_plugin(&mut self, plugin: &str, enabled: bool) {
        set(&mut self.plugins, plugin, enabled);
    }

    /// Record a skill's own flag, independent of any owning plugin's.
    pub(crate) fn set_skill(&mut self, skill: &str, enabled: bool) {
        set(&mut self.skills, skill, enabled);
    }

    /// Whether the state is within the recorded-flag bound.
    pub(crate) fn within_bounds(&self) -> bool {
        self.plugins.len() <= MAX_DISABLED_COMPONENTS
            && self.skills.len() <= MAX_DISABLED_COMPONENTS
    }
}

fn set(flags: &mut BTreeMap<String, bool>, name: &str, enabled: bool) {
    if enabled {
        flags.remove(name);
    } else {
        flags.insert(name.to_owned(), false);
    }
}

/// Read the install's flags.
///
/// Unreadable or malformed state degrades to "everything enabled" with a
/// warning rather than failing the caller: this gates prompt composition and
/// workspace staging on every turn, and a bad settings row must not take the
/// skill system down or, worse, silently disable it.
pub(crate) async fn read_plugin_enable_state(store: &dyn Store) -> PluginEnableState {
    let value = match store.get_setting(PLUGIN_ENABLE_STATE_SETTING).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("plugin enable state is unreadable ({error}); treating all as enabled");
            return PluginEnableState::default();
        }
    };
    let Some(value) = value else {
        return PluginEnableState::default();
    };
    match serde_json::from_value(value) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!("plugin enable state is malformed ({error}); treating all as enabled");
            PluginEnableState::default()
        }
    }
}

/// Persist the install's flags.
pub(crate) async fn write_plugin_enable_state(
    store: &dyn Store,
    state: &PluginEnableState,
) -> tidebreak_core::Result<()> {
    let value = serde_json::to_value(state)
        .map_err(|error| tidebreak_core::AgentError::config(error.to_string()))?;
    store.set_setting(PLUGIN_ENABLE_STATE_SETTING, &value).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract: a plugin's flag gates its members without overwriting them,
    /// so switching a bundle off and back on restores the member choices that
    /// were in place — the reason the two maps are stored independently.
    #[test]
    fn the_plugin_flag_gates_member_flags_without_replacing_them() {
        let mut state = PluginEnableState::default();
        // Default: nothing recorded, everything on.
        assert!(state.plugin_enabled("documents"));
        assert!(state.skill_enabled("word-documents", Some("documents")));

        state.set_skill("pdf-documents", false);
        state.set_plugin("documents", false);
        assert!(!state.skill_enabled("word-documents", Some("documents")));
        // A standalone skill is untouched by another bundle's gate.
        assert!(state.skill_enabled("meeting-notes", None));

        state.set_plugin("documents", true);
        assert!(state.skill_enabled("word-documents", Some("documents")));
        assert!(!state.skill_enabled("pdf-documents", Some("documents")));
    }
}
