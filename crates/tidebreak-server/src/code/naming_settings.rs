//! Per-user defaults for workspace branch names.

use serde::{Deserialize, Serialize};
use tidebreak_core::{OwnerId, Store};

use super::worktree::slugify;

pub(crate) const AUTO_RENAME_BRANCHES_KEY: &str = "code.git.auto_rename_branches";
pub(crate) const BRANCH_PREFIX_MODE_KEY: &str = "code.git.branch_prefix_mode";
pub(crate) const CUSTOM_BRANCH_PREFIX_KEY: &str = "code.git.custom_branch_prefix";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchPrefixMode {
    Account,
    Custom,
    None,
}

impl Default for BranchPrefixMode {
    fn default() -> Self {
        Self::Account
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct GitSourceControlSettings {
    pub auto_rename_branches: bool,
    pub branch_prefix_mode: BranchPrefixMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub custom_branch_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account_prefix: Option<String>,
    pub effective_branch_prefix: String,
}

pub(crate) fn user_setting_key(owner: &OwnerId, key: &str) -> String {
    if owner.is_local() {
        key.to_owned()
    } else {
        format!("{key}.owner.{}", owner.as_str())
    }
}

async fn read_value<T: serde::de::DeserializeOwned>(
    store: &dyn Store,
    owner: &OwnerId,
    key: &str,
) -> tidebreak_core::Result<Option<T>> {
    Ok(store
        .get_setting(&user_setting_key(owner, key))
        .await?
        .and_then(|value| serde_json::from_value(value).ok()))
}

pub(crate) async fn auto_rename_branches(
    store: &dyn Store,
    owner: &OwnerId,
) -> tidebreak_core::Result<bool> {
    Ok(read_value(store, owner, AUTO_RENAME_BRANCHES_KEY)
        .await?
        .unwrap_or(true))
}

pub(crate) async fn read(
    store: &dyn Store,
    owner: &OwnerId,
    account_name: Option<&str>,
) -> tidebreak_core::Result<GitSourceControlSettings> {
    let branch_prefix_mode = read_value(store, owner, BRANCH_PREFIX_MODE_KEY)
        .await?
        .unwrap_or_default();
    let custom_branch_prefix = read_value(store, owner, CUSTOM_BRANCH_PREFIX_KEY).await?;
    let account_prefix = account_name
        .map(slugify)
        .filter(|value| !value.is_empty())
        .map(|value| format!("{value}/"));
    let effective_branch_prefix = match branch_prefix_mode {
        BranchPrefixMode::Account => account_prefix
            .clone()
            .unwrap_or_else(|| "tidebreak/".to_owned()),
        BranchPrefixMode::Custom => custom_branch_prefix
            .clone()
            .unwrap_or_else(|| "tidebreak/".to_owned()),
        BranchPrefixMode::None => String::new(),
    };
    Ok(GitSourceControlSettings {
        auto_rename_branches: auto_rename_branches(store, owner).await?,
        branch_prefix_mode,
        custom_branch_prefix,
        account_prefix,
        effective_branch_prefix,
    })
}

pub(crate) async fn write_auto_rename_branches(
    store: &dyn Store,
    owner: &OwnerId,
    enabled: bool,
) -> tidebreak_core::Result<()> {
    store
        .set_setting(
            &user_setting_key(owner, AUTO_RENAME_BRANCHES_KEY),
            &serde_json::json!(enabled),
        )
        .await
}

pub(crate) async fn write_branch_prefix_mode(
    store: &dyn Store,
    owner: &OwnerId,
    mode: BranchPrefixMode,
) -> tidebreak_core::Result<()> {
    store
        .set_setting(
            &user_setting_key(owner, BRANCH_PREFIX_MODE_KEY),
            &serde_json::json!(mode),
        )
        .await
}

pub(crate) async fn write_custom_branch_prefix(
    store: &dyn Store,
    owner: &OwnerId,
    prefix: Option<&str>,
) -> tidebreak_core::Result<()> {
    store
        .set_setting(
            &user_setting_key(owner, CUSTOM_BRANCH_PREFIX_KEY),
            &prefix
                .map(|value| serde_json::Value::String(value.to_owned()))
                .unwrap_or(serde_json::Value::Null),
        )
        .await
}

/// Normalize a custom prefix and reject names Git cannot use as a ref prefix.
pub(crate) fn normalize_custom_prefix(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('/');
    if value.is_empty() || value.len() > 120 || value.contains("@{") || value.contains("..") {
        return None;
    }
    if value.bytes().any(|byte| {
        byte <= b' '
            || byte == 0x7f
            || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    }) {
        return None;
    }
    if value.split('/').any(|segment| {
        segment.is_empty()
            || segment.starts_with('.')
            || segment.ends_with('.')
            || segment.ends_with(".lock")
    }) {
        return None;
    }
    Some(format!("{value}/"))
}

#[cfg(test)]
mod tests {
    use super::normalize_custom_prefix;

    #[test]
    fn custom_prefixes_follow_git_ref_rules() {
        assert_eq!(
            normalize_custom_prefix(" team/alex/ "),
            Some("team/alex/".into())
        );
        for invalid in [
            "",
            "/",
            ".hidden",
            "team//alex",
            "team..alex",
            "bad name",
            "x.lock",
        ] {
            assert_eq!(normalize_custom_prefix(invalid), None, "{invalid}");
        }
    }
}
