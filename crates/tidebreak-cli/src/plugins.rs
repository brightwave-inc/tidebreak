//! `tidebreak plugins install` — pin an instruction-only plugin from Git.
//!
//! The command is a thin client of `POST /plugins/install`. The repository
//! URL must be public HTTPS, and the revision must be a tag or a full commit
//! SHA: the importer never follows a moving branch.

use std::ffi::{OsStr, OsString};

use serde::Deserialize;
use tidebreak_core::{AgentError, Result};

use crate::api::client::Client;
use crate::print::OutputFormat;

/// One parsed `tidebreak plugins` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Install {
        url: String,
        revision: String,
        format: OutputFormat,
    },
}

/// Hand-rolled argument parsing, matching the rest of the CLI.
pub fn parse(mut args: impl Iterator<Item = OsString>) -> std::result::Result<Command, String> {
    let Some(subcommand) = args.next() else {
        return Err("plugins requires install".to_owned());
    };
    if subcommand != OsStr::new("install") {
        return Err(format!(
            "unknown plugins subcommand {}",
            subcommand.to_string_lossy()
        ));
    }
    let mut url = None;
    let mut revision = None;
    let mut format = None;
    while let Some(argument) = args.next() {
        if argument == OsStr::new("--git") {
            let Some(value) = args.next() else {
                return Err("plugins install --git requires a repository URL".to_owned());
            };
            if url.is_some() {
                return Err("plugins install accepts one --git".to_owned());
            }
            url = Some(os_to_utf8(&value, "--git")?);
        } else if argument == OsStr::new("--ref") {
            let Some(value) = args.next() else {
                return Err("plugins install --ref requires a tag or commit SHA".to_owned());
            };
            if revision.is_some() {
                return Err("plugins install accepts one --ref".to_owned());
            }
            revision = Some(os_to_utf8(&value, "--ref")?);
        } else if argument == OsStr::new("--json") {
            if format.is_some() {
                return Err("plugins install accepts one output format".to_owned());
            }
            format = Some(OutputFormat::Json);
        } else if argument == OsStr::new("--output-format") {
            let Some(value) = args.next() else {
                return Err("plugins install --output-format requires text or json".to_owned());
            };
            if format.is_some() {
                return Err("plugins install accepts one output format".to_owned());
            }
            format = Some(parse_format(&value)?);
        } else {
            return Err(format!("unexpected plugins install argument {argument:?}"));
        }
    }
    let Some(url) = url else {
        return Err("plugins install requires --git <url>".to_owned());
    };
    let Some(revision) = revision else {
        return Err("plugins install requires --ref <tag-or-sha>".to_owned());
    };
    validate_git_url(&url)?;
    if revision.trim().is_empty() {
        return Err("plugins install --ref requires a tag or commit SHA".to_owned());
    }
    Ok(Command::Install {
        url,
        revision,
        format: format.unwrap_or(OutputFormat::Text),
    })
}

fn os_to_utf8(value: &OsStr, flag: &str) -> std::result::Result<String, String> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{flag} expects UTF-8"))
}

fn parse_format(value: &OsStr) -> std::result::Result<OutputFormat, String> {
    match value.to_str() {
        Some("text") => Ok(OutputFormat::Text),
        Some("json") => Ok(OutputFormat::Json),
        _ => Err("plugins install --output-format expects text or json".to_owned()),
    }
}

/// Public HTTPS only. A moving branch is refused by the server; this check
/// stops a non-HTTPS URL before the request.
pub fn validate_git_url(url: &str) -> std::result::Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| "use an https repository URL".to_owned())?;
    if parsed.scheme() != "https" {
        return Err("use an https repository URL".to_owned());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the repository URL must not include credentials".to_owned());
    }
    if parsed.host_str().is_none() {
        return Err("use an https repository URL".to_owned());
    }
    Ok(())
}

/// Run one plugins command against the profile's server.
pub async fn run(command: Command, server: crate::connect::Server) -> Result<()> {
    let session = crate::connect::Session::open(&server).await?;
    execute(session.client(), command).await
}

async fn execute(client: &Client, command: Command) -> Result<()> {
    match command {
        Command::Install {
            url,
            revision,
            format,
        } => {
            let outcome = client
                .install_plugin(&url, &revision)
                .await
                .map_err(map_install_error)?;
            if format == OutputFormat::Json {
                println!("{}", install_document(&outcome)?);
                return Ok(());
            }
            print_text(&outcome);
            Ok(())
        }
    }
}

/// The `--json` document for an install: the server's outcome plus the
/// CLI's `schema_version`, like every other JSON document the CLI prints.
fn install_document(outcome: &serde_json::Value) -> Result<serde_json::Value> {
    crate::json_output::document(outcome)
}

#[derive(Debug, Deserialize)]
struct InstallOutcome {
    plugin: String,
    revision: String,
    #[serde(default)]
    skipped: Vec<SkippedMember>,
}

#[derive(Debug, Deserialize)]
struct SkippedMember {
    path: String,
    reason: String,
}

fn print_text(value: &serde_json::Value) {
    let Ok(outcome) = serde_json::from_value::<InstallOutcome>(value.clone()) else {
        println!("{value}");
        return;
    };
    println!("Installed {} at {}.", outcome.plugin, outcome.revision);
    if outcome.skipped.is_empty() {
        return;
    }
    println!(
        "Skipped {} member{}:",
        outcome.skipped.len(),
        if outcome.skipped.len() == 1 { "" } else { "s" }
    );
    for member in outcome.skipped {
        println!("  {}: {}", member.path, member.reason);
    }
}

fn map_install_error(error: AgentError) -> AgentError {
    let text = error.to_string();
    AgentError::msg(plain_install_error(&text))
}

/// Map the server's kinded error into a short sentence.
pub fn plain_install_error(raw: &str) -> String {
    let lowered = raw.to_ascii_lowercase();
    if lowered.contains("plugin_source_invalid") || lowered.contains("plugin source is invalid") {
        if lowered.contains("revision") || lowered.contains("tag or full commit") {
            return "Use a tag or a full commit SHA, not a branch.".to_owned();
        }
        if lowered.contains("https") || lowered.contains("scheme") {
            return "Use an https repository URL.".to_owned();
        }
        return "That repository URL is not valid.".to_owned();
    }
    if lowered.contains("plugin_source_unavailable")
        || lowered.contains("could not be fetched")
        || lowered.contains("did not resolve")
    {
        if lowered.contains("did not resolve") || lowered.contains("timed out") {
            return "Could not reach that host.".to_owned();
        }
        if lowered.contains("404") || lowered.contains("not found") {
            return "That tag or commit was not found.".to_owned();
        }
        return "Could not fetch that repository.".to_owned();
    }
    if lowered.contains("plugin_invalid") || lowered.contains("plugin content is invalid") {
        return "No recognizable plugin was found in that repository.".to_owned();
    }
    if lowered.contains("plugin_conflict") || lowered.contains("already") {
        return "A plugin with that name is already installed.".to_owned();
    }
    if lowered.contains("plugin_install_unavailable") {
        return "Plugin install needs code execution.".to_owned();
    }
    raw.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_line(args: &[&str]) -> std::result::Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn the_install_document_carries_the_schema_version() {
        let outcome = serde_json::json!({
            "plugin": "meeting-notes",
            "revision": "v1.0.0",
            "skipped": [],
        });
        let document = install_document(&outcome).expect("encode");
        assert_eq!(
            document[crate::json_output::SCHEMA_VERSION_KEY],
            crate::json_output::SCHEMA_VERSION
        );
        assert_eq!(document["plugin"], "meeting-notes");
        assert_eq!(document["revision"], "v1.0.0");
    }

    #[test]
    fn install_requires_git_and_ref() {
        let err = parse_line(&["install"]).unwrap_err();
        assert!(err.contains("--git"), "{err}");
        let err = parse_line(&["install", "--git", "https://github.com/acme/notes"]).unwrap_err();
        assert!(err.contains("--ref"), "{err}");
    }

    #[test]
    fn install_refuses_http() {
        let err = parse_line(&[
            "install",
            "--git",
            "http://github.com/acme/notes",
            "--ref",
            "v1.0.0",
        ])
        .unwrap_err();
        assert!(err.contains("https"), "{err}");
    }

    #[test]
    fn install_parses_json_flag() {
        let command = parse_line(&[
            "install",
            "--git",
            "https://github.com/acme/notes",
            "--ref",
            "v1.0.0",
            "--json",
        ])
        .unwrap();
        assert_eq!(
            command,
            Command::Install {
                url: "https://github.com/acme/notes".to_owned(),
                revision: "v1.0.0".to_owned(),
                format: OutputFormat::Json,
            }
        );
    }

    #[test]
    fn plain_errors_cover_the_known_kinds() {
        assert_eq!(
            plain_install_error("request failed (400): plugin_source_invalid: not https"),
            "Use an https repository URL."
        );
        assert_eq!(
            plain_install_error(
                "request failed (422): plugin_source_unavailable: source host did not resolve"
            ),
            "Could not reach that host."
        );
        assert_eq!(
            plain_install_error("request failed (422): plugin_invalid: no plugin.json"),
            "No recognizable plugin was found in that repository."
        );
        assert_eq!(
            plain_install_error("request failed (409): plugin_conflict: already installed"),
            "A plugin with that name is already installed."
        );
    }
}
