//! The directory of well-known remote MCP servers that Settings offers to add.
//!
//! The entries live in `mcp_directory.json`, one server per line, so adding or
//! correcting one is a one-line change. Each entry is an endpoint its vendor
//! hosts and publishes in its own documentation, and `docs` names that page.
//!
//! The directory decides nothing at runtime. Adding an entry saves an ordinary
//! HTTP server definition: the server signs in through the same OAuth flow as
//! any remote server that answers `401` (`mcp_config::oauth`), or reads a
//! token from an environment variable. Tidebreak holds no OAuth app for any of
//! them. The tier is not the directory's to claim either: an entry shows
//! "Tested" only when the curated list (`mcp_curated`) matches its URL.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::mcp_config::{McpServerDefinition, DEFAULT_REQUEST_TIMEOUT_MS};
use crate::mcp_curated::{curation_for, McpCuration};

/// The data file, compiled in: no network fetch and no background refresh.
const DIRECTORY_JSON: &str = include_str!("mcp_directory.json");

/// How a directory server signs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind")]
pub enum McpDirectorySignIn {
    /// The server asks for an OAuth sign-in, and Tidebreak runs it in the
    /// browser once the server is added.
    #[serde(rename = "oauth")]
    OAuth,
    /// The server takes a token as a bearer. Tidebreak reads it from
    /// `variable` in the environment it starts with.
    #[serde(rename = "token")]
    Token { variable: String },
    /// The server needs no sign-in.
    #[serde(rename = "none")]
    NoSignIn,
}

/// One directory server, as Settings lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct McpDirectoryEntry {
    /// Stable id. Adding the server names it, and the added server is named
    /// after it.
    pub id: String,
    /// The vendor's name for the server.
    pub name: String,
    /// The endpoint, exactly as the vendor's documentation gives it.
    pub url: String,
    /// One sentence on what the server lets you do.
    pub description: String,
    pub sign_in: McpDirectorySignIn,
    /// The vendor page that publishes `url`.
    pub docs_url: String,
    /// The curated-list entry for this server, when Tidebreak has driven it
    /// end to end. `null` otherwise, and Settings then shows no tier for it.
    pub curated: Option<McpCuration>,
}

/// `GET /mcp/directory`: every directory server, in the data file's order.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct McpDirectory {
    pub servers: Vec<McpDirectoryEntry>,
}

/// One line of the data file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryRecord {
    id: String,
    name: String,
    url: String,
    description: String,
    sign_in: McpDirectorySignIn,
    docs: String,
}

static DIRECTORY: LazyLock<Vec<McpDirectoryEntry>> = LazyLock::new(|| {
    parse(DIRECTORY_JSON).unwrap_or_else(|error| {
        // The unit tests parse the same file, so this is unreachable in a
        // build that passed them; an empty directory beats a panic.
        tracing::error!("the MCP server directory does not parse: {error}");
        Vec::new()
    })
});

fn parse(json: &str) -> serde_json::Result<Vec<McpDirectoryEntry>> {
    let records: Vec<DirectoryRecord> = serde_json::from_str(json)?;
    Ok(records
        .into_iter()
        .map(|record| McpDirectoryEntry {
            curated: curation_for(None, &[], Some(&record.url)),
            id: record.id,
            name: record.name,
            url: record.url,
            description: record.description,
            sign_in: record.sign_in,
            docs_url: record.docs,
        })
        .collect())
}

/// Every directory server.
pub fn directory() -> McpDirectory {
    McpDirectory {
        servers: DIRECTORY.clone(),
    }
}

/// The directory server with this id.
pub fn entry(id: &str) -> Option<McpDirectoryEntry> {
    DIRECTORY.iter().find(|entry| entry.id == id).cloned()
}

impl McpDirectoryEntry {
    /// The definition adding this server saves: an HTTP server named after
    /// the entry, reading its token variable when it takes one.
    ///
    /// It is not marked as OAuth. A server that answers `401` with OAuth
    /// metadata gets the sign-in either way, and an unmarked definition is
    /// what an import of the same URL saves, so both fingerprint the same.
    pub fn definition(&self) -> McpServerDefinition {
        McpServerDefinition {
            name: self.id.clone(),
            command: None,
            args: Vec::new(),
            env: BTreeSet::new(),
            env_values: BTreeMap::new(),
            env_from: Vec::new(),
            cwd: None,
            url: Some(self.url.clone()),
            bearer_token_env: match &self.sign_in {
                McpDirectorySignIn::Token { variable } => Some(variable.clone()),
                McpDirectorySignIn::OAuth | McpDirectorySignIn::NoSignIn => None,
            },
            bearer_token_stored: false,
            bearer_token_value: None,
            headers: BTreeSet::new(),
            header_values: BTreeMap::new(),
            oauth: false,
            gateway_endpoint: None,
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            enabled: true,
            plugin: None,
            launch: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The data file is hand-edited and nothing else reads it before a
    /// person does, so every rule a reader relies on is checked here: it
    /// parses with no unknown keys, each entry says where its URL comes from
    /// and what the server does, and the list reads in one predictable order.
    /// That each entry also makes a valid server definition is checked in
    /// `mcp_config`, which owns the validation.
    #[test]
    fn the_directory_file_lists_sourced_https_servers_in_name_order() {
        let servers = parse(DIRECTORY_JSON).expect("the directory file parses");
        assert!(
            (15..=25).contains(&servers.len()),
            "the directory lists 15 to 25 servers, not {}",
            servers.len()
        );
        let mut ids = BTreeSet::new();
        let mut urls = BTreeSet::new();
        for entry in &servers {
            let label = &entry.id;
            assert!(ids.insert(entry.id.clone()), "{label} is listed twice");
            assert!(urls.insert(entry.url.clone()), "{label} repeats a URL");
            assert!(
                entry.url.starts_with("https://"),
                "{label} must use https: {}",
                entry.url
            );
            assert!(
                entry.docs_url.starts_with("https://"),
                "{label} must cite an https page for its URL"
            );
            assert!(!entry.name.trim().is_empty(), "{label} has no name");
            assert!(
                entry.description.ends_with('.') && entry.description.chars().count() <= 80,
                "{label} needs one short sentence of description"
            );
            if let McpDirectorySignIn::Token { variable } = &entry.sign_in {
                assert!(
                    !variable.is_empty()
                        && variable.bytes().all(|byte| byte.is_ascii_uppercase()
                            || byte.is_ascii_digit()
                            || byte == b'_'),
                    "{label} names an unusual token variable: {variable}"
                );
            }
        }
        let names: Vec<String> = servers
            .iter()
            .map(|entry| entry.name.to_lowercase())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "keep the directory in name order");
    }

    /// Adding a server saves a plain HTTP definition: its token variable when
    /// it takes one, and no OAuth flag, so it matches an import of the same
    /// URL.
    #[test]
    fn a_directory_server_saves_as_a_plain_http_definition() {
        let servers = parse(DIRECTORY_JSON).unwrap();
        for entry in &servers {
            let definition = entry.definition();
            assert_eq!(definition.name, entry.id);
            assert_eq!(definition.url.as_deref(), Some(entry.url.as_str()));
            assert!(!definition.oauth);
            assert!(definition.enabled);
            match &entry.sign_in {
                McpDirectorySignIn::Token { variable } => {
                    assert_eq!(
                        definition.bearer_token_env.as_deref(),
                        Some(variable.as_str())
                    );
                }
                McpDirectorySignIn::OAuth | McpDirectorySignIn::NoSignIn => {
                    assert!(definition.bearer_token_env.is_none());
                }
            }
        }
        assert_eq!(entry(&servers[0].id).as_ref(), Some(&servers[0]));
        assert!(entry("not-in-the-directory").is_none());
    }

    /// The directory never claims a tier on its own: an entry is tested only
    /// when the curated list recognizes its URL, the same rule a configured
    /// server's row follows.
    #[test]
    fn a_tier_comes_only_from_the_curated_list() {
        for entry in parse(DIRECTORY_JSON).unwrap() {
            assert_eq!(entry.curated, curation_for(None, &[], Some(&entry.url)));
        }
        let listed = parse(
            r#"[{"id":"a","name":"A","url":"https://mcp.a.example/mcp","description":"A.","sign_in":{"kind":"none"},"docs":"https://a.example/mcp"}]"#,
        )
        .unwrap();
        assert!(listed[0].curated.is_none());
        assert!(parse(
            r#"[{"id":"a","name":"A","url":"https://a.example","description":"A.","sign_in":{"kind":"none"},"docs":"https://a.example","tested":true}]"#,
        )
        .is_err());
    }
}
