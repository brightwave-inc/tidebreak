//! The curated list of MCP servers Tidebreak has exercised end to end.
//!
//! Any MCP server can be mounted, and nothing here gates one: the list is a
//! label, not a policy. A configured server that matches an entry is shown as
//! *tested*; everything else is shown as *community* — usable, honestly
//! marked. `docs/mcp-tested-servers.md` states what a curated entry claims and
//! how a server earns one.
//!
//! Recognition is deliberately narrow. A stdio entry matches the executable's
//! file stem plus the leading arguments that select MCP mode, and an HTTP
//! entry matches the URL's exact `scheme://authority`. Anything looser would
//! let an unrelated server inherit the badge.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The curated-registry entry a configured server matched, projected to the
/// renderer beside the server's health.
///
/// Presence *is* the tier: a server with a curation is "tested", a server
/// without one is "community". One field cannot disagree with itself the way
/// a separate boolean and a separate record could.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct McpCuration {
    /// The curated list's own name for the server, which need not match the
    /// namespace the reader configured it under.
    pub display_name: String,
    /// `YYYY-MM-DD` the entry was last exercised end to end.
    pub tested_on: String,
    /// One sentence on what was exercised, for the reader deciding how much
    /// the badge is worth.
    pub notes: String,
}

/// How a curated entry recognises a stdio definition.
#[derive(Debug, Clone, Copy)]
struct CuratedCommand {
    /// The executable's file stem, compared case-insensitively so a path, a
    /// bare name, and a Windows `.exe` all land on the same entry.
    program: &'static str,
    /// Leading arguments that select the server's MCP mode, compared exactly.
    /// Empty for a program that speaks MCP and nothing else.
    args_prefix: &'static [&'static str],
}

/// One curated server: what we call it, how to recognise it, and when we last
/// exercised it.
struct CuratedMcpServer {
    display_name: &'static str,
    /// Set when the entry describes a local stdio server.
    stdio: Option<CuratedCommand>,
    /// Set when the entry describes a remote HTTP server: the exact
    /// lowercase `scheme://authority` its URL must carry. Compared whole, so
    /// a userinfo prefix (`https://curated.example@evil.example/`) cannot
    /// borrow a curated origin.
    url_origin: Option<&'static str>,
    tested_on: &'static str,
    notes: &'static str,
}

/// The curated list.
///
/// Short on purpose. An entry says a person mounted that server in Tidebreak
/// and drove its auth, tool schemas, streaming, and approval previews — not
/// that the server is popular or that its vendor is trusted. Adding one is an
/// editorial act with a date attached; see `docs/mcp-tested-servers.md`.
const CURATED_MCP_SERVERS: &[CuratedMcpServer] = &[CuratedMcpServer {
    display_name: "Tidebreak workspace tools",
    stdio: Some(CuratedCommand {
        program: "tidebreak",
        args_prefix: &["mcp"],
    }),
    url_origin: None,
    tested_on: "2026-08-05",
    notes: "Tidebreak's own read-only workspace server over stdio. The \
            workspace's integration tests mount it through the same \
            configuration path the desktop uses, so discovery, tool schemas, \
            and the approval gate are exercised on every run.",
}];

/// The curated entry this definition matches, if any.
///
/// Takes the recognisable parts rather than the definition so the matcher
/// stays independent of how a definition is spelled.
pub(crate) fn curation_for(
    command: Option<&str>,
    args: &[String],
    url: Option<&str>,
) -> Option<McpCuration> {
    CURATED_MCP_SERVERS
        .iter()
        .find(|entry| entry.matches(command, args, url))
        .map(|entry| McpCuration {
            display_name: entry.display_name.to_string(),
            tested_on: entry.tested_on.to_string(),
            notes: entry.notes.to_string(),
        })
}

impl CuratedMcpServer {
    fn matches(&self, command: Option<&str>, args: &[String], url: Option<&str>) -> bool {
        self.matches_stdio(command, args) || self.matches_url(url)
    }

    fn matches_stdio(&self, command: Option<&str>, args: &[String]) -> bool {
        let (Some(entry), Some(command)) = (self.stdio, command) else {
            return false;
        };
        let Some(stem) = Path::new(command)
            .file_stem()
            .and_then(|stem| stem.to_str())
        else {
            return false;
        };
        stem.eq_ignore_ascii_case(entry.program)
            && args.len() >= entry.args_prefix.len()
            && args
                .iter()
                .zip(entry.args_prefix)
                .all(|(configured, expected)| configured == expected)
    }

    fn matches_url(&self, url: Option<&str>) -> bool {
        let (Some(origin), Some(url)) = (self.url_origin, url) else {
            return false;
        };
        origin_of(url).is_some_and(|configured| configured == origin)
    }
}

/// The lowercase `scheme://authority` of an absolute URL.
///
/// Hand-rolled because this surface has no URL parser and needs no more than
/// the prefix before the path. Scheme and authority are both
/// case-insensitive; everything after the authority is ignored.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The badge is a claim about a specific server, so an entry that cannot
    /// recognise one — or recognises one by a pattern too loose to identify
    /// it — would put "tested" on the wrong row. Every field the matcher and
    /// the renderer read is checked here because the list is hand-edited and
    /// nothing else looks at it.
    #[test]
    fn every_curated_entry_can_only_match_the_server_it_names() {
        for entry in CURATED_MCP_SERVERS {
            let label = entry.display_name;
            assert!(
                entry.stdio.is_some() != entry.url_origin.is_some(),
                "{label} must declare exactly one transport pattern"
            );
            if let Some(stdio) = entry.stdio {
                assert!(!stdio.program.is_empty(), "{label} has an empty program");
            }
            if let Some(origin) = entry.url_origin {
                assert_eq!(
                    origin_of(origin).as_deref(),
                    Some(origin),
                    "{label} must declare a lowercase scheme://authority origin"
                );
            }
            assert!(
                entry.tested_on.len() == 10
                    && entry
                        .tested_on
                        .chars()
                        .enumerate()
                        .all(|(index, character)| if index == 4 || index == 7 {
                            character == '-'
                        } else {
                            character.is_ascii_digit()
                        }),
                "{label} must carry a YYYY-MM-DD tested date"
            );
            assert!(!entry.notes.is_empty(), "{label} must say what was tested");
        }

        let args = vec!["mcp".to_string(), "/workspace".to_string()];
        assert!(curation_for(Some("/usr/local/bin/tidebreak"), &args, None).is_some());
        // The subcommand is what makes it an MCP server; `tidebreak serve` is
        // the same binary and is not one.
        assert!(curation_for(Some("tidebreak"), &["serve".to_string()], None).is_none());
        assert!(curation_for(Some("tidebreak-fork"), &args, None).is_none());
        assert!(curation_for(None, &[], Some("https://example.invalid/mcp")).is_none());
    }
}
