//! Bundled MCP servers, from an installed plugin to the live MCP runtime.
//!
//! A plugin published in the Agent Plugins format (<https://agent-plugins.org>)
//! may ship an `mcp.json` declaring servers the client is expected to run on
//! its behalf. The importer validates that file and retains a canonical copy
//! beside the installed package; this module turns the retained copy into the
//! [`McpServerDefinition`]s the runtime already knows how to connect,
//! supervise, and project.
//!
//! Three properties shape the design:
//!
//! * **Derived, never a record.** A plugin's servers are recomputed from the
//!   installed tree and the plugin's enable flag every time the set is
//!   reconciled. They are not `mcp_server` connected-app records, are not
//!   persisted, and cannot be edited or deleted through `PUT /mcp/servers` —
//!   the plugin's own install/uninstall and enable flag are the only controls.
//! * **The plugin never chooses its runtime name.** `mcp.json` server keys are
//!   up to 64 bytes and may contain `.`; a runtime namespace is 32 bytes of
//!   `[A-Za-z0-9_-]` because it has to fit inside `mcp__{server}__{tool}`. The
//!   derivation below is deterministic, and on any collision the plugin's
//!   server is dropped rather than displacing what is already configured.
//! * **A bad entry costs one server.** An unsupported transport, a working
//!   directory that escapes the root, a name already taken — each one takes
//!   that entry out and leaves the plugin's other servers, and every other
//!   plugin, running.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openwave_code_execution::{
    expand_plugin_placeholders, McpTransport, PLUGIN_DATA_VARIABLE, PLUGIN_ROOT_VARIABLE,
};
use openwave_core::Store;

use crate::mcp_config::{CwdAnchor, McpServerDefinition, PluginLaunch, DEFAULT_REQUEST_TIMEOUT_MS};

/// Longest runtime server name, mirroring [`openwave_mcp::MAX_SERVER_NAME_BYTES`].
const MAX_RUNTIME_NAME_BYTES: usize = openwave_mcp::MAX_SERVER_NAME_BYTES;

/// Bytes of the derived name kept when a name has to be shortened, leaving
/// room for the `-` and the six hex digits that make the short form stable and
/// distinct.
const TRUNCATED_NAME_BYTES: usize = MAX_RUNTIME_NAME_BYTES - 7;

/// The diagnostic a `sse` entry carries. The specification keeps that
/// transport optional and this client does not implement it, so the entry is
/// surfaced disabled — with the reason — rather than silently missing.
pub(crate) const SSE_UNSUPPORTED_DIAGNOSTIC: &str =
    "This plugin server uses the legacy SSE transport, which OpenWave does not implement.";

/// One installed, enabled plugin's bundled MCP configuration, with the two
/// directories its servers are launched against.
pub(crate) struct PluginMcpSource {
    pub(crate) plugin: String,
    /// Absolute, resolved package root — the value of `PLUGIN_ROOT`.
    pub(crate) root: PathBuf,
    /// Client-managed writable directory — the value of `PLUGIN_DATA`.
    pub(crate) data: PathBuf,
    pub(crate) config: openwave_code_execution::PluginMcpConfig,
}

/// One bundled server that did not make it into the runtime set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkippedPluginServer {
    pub(crate) plugin: String,
    pub(crate) server: String,
    pub(crate) reason: String,
}

/// The seam the MCP runtime reads plugin-sourced servers through.
///
/// A trait rather than a direct dependency because the runtime is assembled
/// before the code-execution provider exists, and because embeddings without
/// one (tests, headless servers with no plugin tree) should contribute no
/// plugin servers rather than fail.
#[async_trait::async_trait]
pub(crate) trait PluginMcpCatalog: Send + Sync {
    /// Every installed, enabled plugin's bundled configuration, read live.
    async fn sources(&self) -> Vec<PluginMcpSource>;
}

/// The production catalog: the installed plugin tree, the stored enable flags,
/// and the app data directory the `PLUGIN_DATA` directories live under.
pub(crate) struct InstalledPluginMcpCatalog {
    exec: Arc<crate::code_execution::ConfiguredCodeExecutionProvider>,
    store: Arc<dyn Store>,
    data_root: PathBuf,
}

impl InstalledPluginMcpCatalog {
    pub(crate) fn new(
        exec: Arc<crate::code_execution::ConfiguredCodeExecutionProvider>,
        store: Arc<dyn Store>,
        data_root: PathBuf,
    ) -> Self {
        Self {
            exec,
            store,
            data_root,
        }
    }
}

#[async_trait::async_trait]
impl PluginMcpCatalog for InstalledPluginMcpCatalog {
    async fn sources(&self) -> Vec<PluginMcpSource> {
        let flags = crate::plugin_state::read_plugin_enable_state(&*self.store).await;
        let installed = self.exec.installed_plugin_mcp();
        // Uninstalling a plugin is removing its directory, so the data
        // directory of a plugin that is no longer installed is orphaned. It is
        // pruned against the full installed list — not the enabled one — so
        // switching a plugin off keeps its state for when it comes back.
        let live: BTreeSet<String> = self
            .exec
            .installed_plugins()
            .into_iter()
            .map(|package| package.name)
            .collect();
        prune_orphan_plugin_data(&self.data_root, &live);

        let mut sources = Vec::new();
        for (package, root, config) in installed {
            if !flags.plugin_enabled(&package.name) {
                continue;
            }
            let data = self.data_root.join(&package.name);
            // Created before anything launches: the specification requires the
            // directory to exist when a server starts, and a server that has
            // to create its own state directory cannot rely on it.
            if let Err(error) = std::fs::create_dir_all(&data) {
                tracing::warn!(
                    plugin = %package.name,
                    "plugin data directory could not be created; not starting its MCP servers: \
                     {error}"
                );
                continue;
            }
            let Ok(data) = std::fs::canonicalize(&data) else {
                continue;
            };
            sources.push(PluginMcpSource {
                plugin: package.name,
                root,
                data,
                config,
            });
        }
        sources
    }
}

/// Delete every `PLUGIN_DATA` directory whose plugin is no longer installed.
///
/// Best effort throughout: a directory that will not read or will not delete
/// is left alone and warned about. Nothing here can remove data for a plugin
/// that is present, which is what makes reinstalling or updating a package
/// preserve its state.
fn prune_orphan_plugin_data(data_root: &Path, installed: &BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(data_root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if installed.contains(&name) {
            continue;
        }
        let path = entry.path();
        let directory = path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
        if !directory {
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(&path) {
            tracing::warn!("could not remove orphaned plugin data {}: {error}", name);
        }
    }
}

/// The runtime namespace a plugin's server mounts under.
///
/// `{plugin}-{server}` with every character outside `[A-Za-z0-9_-]` folded to
/// `-`, which covers the `.` both grammars admit and the runtime name does
/// not. A name that does not fit the 32-byte namespace limit is truncated and
/// given six hex digits of a SHA-256 over the exact `{plugin}/{server}` pair,
/// so two long names that share a prefix stay distinct and every name is
/// stable across restarts.
///
/// This is deliberately not collision-*proof*: folding `.` to `-` can make two
/// distinct pairs agree. Collisions are resolved by dropping, not by
/// disambiguating, so a plugin can never take a namespace by picking a name
/// that collides with one already in use.
#[must_use]
pub(crate) fn runtime_server_name(plugin: &str, server: &str) -> String {
    use sha2::Digest as _;

    let folded: String = format!("{plugin}-{server}")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if folded.len() <= MAX_RUNTIME_NAME_BYTES {
        return folded;
    }
    let digest = sha2::Sha256::digest(format!("{plugin}/{server}").as_bytes());
    // Folded names are ASCII by construction, so byte slicing is safe.
    format!(
        "{}-{:02x}{:02x}{:02x}",
        &folded[..TRUNCATED_NAME_BYTES],
        digest[0],
        digest[1],
        digest[2]
    )
}

/// Turn every source's bundled configuration into runtime definitions.
///
/// `taken` is the set of namespaces already spoken for — user-configured
/// servers and gateway mounts — and grows as plugin servers are accepted, so
/// the first claimant in (plugin, server) order wins and everything after it
/// is skipped. Sources are processed in the order given; the caller sorts them
/// by plugin name so the outcome does not depend on directory iteration order.
pub(crate) fn derive_definitions(
    sources: &[PluginMcpSource],
    taken: &HashSet<String>,
) -> (Vec<McpServerDefinition>, Vec<SkippedPluginServer>) {
    let mut taken = taken.clone();
    let mut definitions = Vec::new();
    let mut skipped = Vec::new();
    for source in sources {
        let root = source.root.to_string_lossy();
        let data = source.data.to_string_lossy();
        for server in &source.config.servers {
            let name = runtime_server_name(&source.plugin, &server.name);
            if !taken.insert(name.clone()) {
                skipped.push(SkippedPluginServer {
                    plugin: source.plugin.clone(),
                    server: server.name.clone(),
                    reason: format!(
                        "the MCP server namespace {name:?} it derives is already in use"
                    ),
                });
                continue;
            }
            definitions.push(definition_for(source, server, &name, &root, &data));
        }
    }
    (definitions, skipped)
}

fn definition_for(
    source: &PluginMcpSource,
    server: &openwave_code_execution::McpServer,
    name: &str,
    root: &str,
    data: &str,
) -> McpServerDefinition {
    let base = McpServerDefinition {
        name: name.to_owned(),
        command: None,
        args: Vec::new(),
        // Deliberately empty: `env` names entries whose values live in the
        // secret store, and a plugin's environment is literal package data
        // that must never be written there. The values ride on `launch`.
        env: BTreeSet::new(),
        env_values: BTreeMap::new(),
        env_from: Vec::new(),
        cwd: None,
        url: None,
        bearer_token_env: None,
        gateway_endpoint: None,
        request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        enabled: true,
        plugin: Some(source.plugin.clone()),
        launch: None,
    };
    let launch = PluginLaunch {
        root: source.root.clone(),
        data: source.data.clone(),
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        cwd_anchor: CwdAnchor::Root,
        disabled_reason: None,
    };
    match &server.transport {
        McpTransport::Stdio(stdio) => {
            let (cwd, cwd_anchor) =
                resolve_cwd(stdio.cwd.as_deref(), &source.root, &source.data, root, data);
            McpServerDefinition {
                command: Some(stdio.command.clone()),
                args: stdio
                    .args
                    .iter()
                    .map(|argument| expand_plugin_placeholders(argument, root, data))
                    .collect(),
                cwd: Some(cwd),
                launch: Some(Box::new(PluginLaunch {
                    env: stdio
                        .env
                        .iter()
                        .map(|(key, value)| {
                            (key.clone(), expand_plugin_placeholders(value, root, data))
                        })
                        .collect(),
                    cwd_anchor,
                    ..launch
                })),
                ..base
            }
        }
        McpTransport::StreamableHttp(http) => McpServerDefinition {
            url: Some(http.url.clone()),
            launch: Some(Box::new(PluginLaunch {
                headers: http.headers.clone(),
                ..launch
            })),
            ..base
        },
        McpTransport::Sse(http) => McpServerDefinition {
            url: Some(http.url.clone()),
            enabled: false,
            launch: Some(Box::new(PluginLaunch {
                disabled_reason: Some(SSE_UNSUPPORTED_DIAGNOSTIC.to_owned()),
                ..launch
            })),
            ..base
        },
    }
}

/// The absolute working directory a stdio server starts in, and which of the
/// two client-provided roots it is anchored to.
///
/// Resolved by joining onto the anchor rather than by textual expansion,
/// because a `./`-relative `cwd` is specified as *plugin-root*-relative — and
/// a relative path handed to the child would be resolved against whatever
/// directory the host process happens to be in. Absent means the plugin root.
/// Anything outside the three admitted shapes cannot reach here (import
/// validation rejects it, and the retained file is re-parsed on load), so it
/// falls through to plain expansion anchored on the root, which the
/// containment check at launch then judges.
fn resolve_cwd(
    cwd: Option<&str>,
    root_path: &Path,
    data_path: &Path,
    root: &str,
    data: &str,
) -> (PathBuf, CwdAnchor) {
    let join = |base: &Path, rest: &str| base.join(rest.trim_start_matches('/'));
    let Some(cwd) = cwd else {
        return (root_path.to_owned(), CwdAnchor::Root);
    };
    if let Some(rest) = cwd.strip_prefix(&format!("${{{PLUGIN_DATA_VARIABLE}}}")) {
        return (join(data_path, rest), CwdAnchor::Data);
    }
    if let Some(rest) = cwd.strip_prefix(&format!("${{{PLUGIN_ROOT_VARIABLE}}}")) {
        return (join(root_path, rest), CwdAnchor::Root);
    }
    if let Some(rest) = cwd.strip_prefix("./") {
        return (join(root_path, rest), CwdAnchor::Root);
    }
    (
        PathBuf::from(expand_plugin_placeholders(cwd, root, data)),
        CwdAnchor::Root,
    )
}

/// The program a plugin's stdio server is launched as.
///
/// A `./`-relative command names a file inside the package, so it is resolved
/// against the plugin root here rather than left for the child to resolve. On
/// Unix the child would resolve it against its *working directory*, which is
/// the wrong answer the moment a server configures a `cwd` — and a
/// `${PLUGIN_DATA}`-anchored one puts it outside the package entirely. On
/// Windows a relative program resolves against the host process's directory,
/// which is never right. A bare name is left alone and gets exactly the
/// platform resolution every other stdio server's command gets.
pub(crate) fn resolve_command(command: &str, root: &Path) -> Result<PathBuf, &'static str> {
    let Some(relative) = command.strip_prefix("./") else {
        return Ok(PathBuf::from(command));
    };
    let resolved = std::fs::canonicalize(root.join(relative))
        .map_err(|_| "plugin MCP server executable was not found inside the plugin")?;
    // Canonicalization resolved every symlink, so this is the real file the
    // child would execute — a link pointing out of the package is caught here
    // rather than launched.
    if !contained_in(&resolved, root) {
        return Err("plugin MCP server executable is not inside the plugin root");
    }
    Ok(resolved)
}

/// Whether `candidate` stays inside `root` once both are resolved.
///
/// Checked again at connect time even though the retained `mcp.json` was
/// validated textually at import: the textual rule cannot see a symlink, and
/// the file on disk could have been edited since. A candidate that will not
/// canonicalize — because it does not exist — fails the check, which is the
/// right answer for a working directory a child is about to be started in.
pub(crate) fn contained_in(candidate: &Path, root: &Path) -> bool {
    let (Ok(candidate), Ok(root)) = (
        std::fs::canonicalize(candidate),
        std::fs::canonicalize(root),
    ) else {
        return false;
    };
    candidate.starts_with(&root)
}

/// The directory a plugin's child is started in, verified against the root it
/// is anchored to.
///
/// The two anchors are treated differently on purpose. The package tree is
/// immutable once installed, so a `${PLUGIN_ROOT}`-anchored directory that is
/// not there is a broken package and fails. The data tree is *client-managed*
/// — it is handed to the plugin empty — so a `${PLUGIN_DATA}`-anchored
/// subdirectory is created here; a first launch must not fail because the
/// server has not had a chance to create the directory it was told to start
/// in. Containment is then checked either way, after canonicalization, so a
/// symlink planted inside either tree cannot walk the child out of it.
pub(crate) fn working_directory(launch: &PluginLaunch, cwd: &Path) -> Result<(), &'static str> {
    let anchor = match launch.cwd_anchor {
        CwdAnchor::Root => &launch.root,
        CwdAnchor::Data => {
            std::fs::create_dir_all(cwd)
                .map_err(|_| "plugin MCP server working directory could not be created")?;
            &launch.data
        }
    };
    if !contained_in(cwd, anchor) {
        return Err(
            "plugin MCP server working directory is not inside the plugin root or its data \
             directory",
        );
    }
    Ok(())
}

/// The environment a plugin's child is launched with, in specification order:
/// the configured overlay first, then the two reserved variables.
///
/// The reserved names are set last so they cannot be overridden. Configuration
/// naming either of them is already rejected at import, so this ordering has
/// no observable effect today — it is the property the specification states,
/// kept where the launch happens rather than inferred from a parser two crates
/// away.
#[must_use]
pub(crate) fn launch_environment(launch: &PluginLaunch) -> BTreeMap<String, String> {
    let mut environment = launch.env.clone();
    environment.insert(
        PLUGIN_ROOT_VARIABLE.to_owned(),
        launch.root.to_string_lossy().into_owned(),
    );
    environment.insert(
        PLUGIN_DATA_VARIABLE.to_owned(),
        launch.data.to_string_lossy().into_owned(),
    );
    environment
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_code_execution::{McpHttpServer, McpServer, McpStdioServer, PluginMcpConfig};

    fn source(plugin: &str, servers: Vec<McpServer>) -> PluginMcpSource {
        PluginMcpSource {
            plugin: plugin.to_owned(),
            root: PathBuf::from("/pkg").join(plugin),
            data: PathBuf::from("/data").join(plugin),
            config: PluginMcpConfig { servers },
        }
    }

    fn stdio(name: &str) -> McpServer {
        McpServer {
            name: name.to_owned(),
            transport: McpTransport::Stdio(McpStdioServer {
                command: "serve".to_owned(),
                args: vec!["--root".to_owned(), "${PLUGIN_ROOT}/x".to_owned()],
                env: BTreeMap::from([("MODE".to_owned(), "${PLUGIN_DATA}".to_owned())]),
                cwd: None,
            }),
        }
    }

    /// Contract: a plugin server never displaces a namespace someone else
    /// already holds — a user-configured server or another plugin's — and the
    /// drop is reported rather than silent. This is the rule that keeps
    /// installing a plugin from hijacking `mcp__github__*`.
    #[test]
    fn a_namespace_already_in_use_drops_the_plugin_server() {
        let taken = HashSet::from(["notes-search".to_owned()]);
        let sources = vec![
            source("notes", vec![stdio("search")]),
            source("other", vec![stdio("search")]),
        ];
        let (definitions, skipped) = derive_definitions(&sources, &taken);

        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            ["other-search"]
        );
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].plugin, "notes");
        assert!(skipped[0].reason.contains("notes-search"));

        // Two plugins claiming the same derived namespace: the first in the
        // given order keeps it, the second is dropped.
        let (definitions, skipped) = derive_definitions(
            &[
                source("a", vec![stdio("b-c")]),
                source("a-b", vec![stdio("c")]),
            ],
            &HashSet::new(),
        );
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "a-b-c");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].plugin, "a-b");
    }

    /// Contract: what a derived definition carries — expanded arguments and
    /// environment, the reserved variables set last, the legacy transport
    /// present but disabled with a reason, and a `.` in either name folded
    /// into the runtime namespace grammar.
    #[test]
    fn derived_definitions_carry_the_launch_material_the_specification_requires() {
        let sources = vec![source(
            "read.me",
            vec![
                stdio("a.b"),
                McpServer {
                    name: "legacy".to_owned(),
                    transport: McpTransport::Sse(McpHttpServer {
                        url: "http://localhost:7331/sse".to_owned(),
                        headers: BTreeMap::new(),
                    }),
                },
                McpServer {
                    name: "remote".to_owned(),
                    transport: McpTransport::StreamableHttp(McpHttpServer {
                        url: "https://mcp.example.com/v1".to_owned(),
                        headers: BTreeMap::from([("x-client".to_owned(), "ow".to_owned())]),
                    }),
                },
                McpServer {
                    name: "stateful".to_owned(),
                    transport: McpTransport::Stdio(McpStdioServer {
                        command: "serve".to_owned(),
                        args: Vec::new(),
                        env: BTreeMap::new(),
                        cwd: Some("${PLUGIN_DATA}/state".to_owned()),
                    }),
                },
            ],
        )];
        let (definitions, skipped) = derive_definitions(&sources, &HashSet::new());
        assert!(skipped.is_empty());

        let stdio = &definitions[0];
        assert_eq!(stdio.name, "read-me-a-b");
        assert_eq!(stdio.command.as_deref(), Some("serve"));
        assert_eq!(stdio.args, ["--root", "/pkg/read.me/x"]);
        // Absent `cwd` anchors on the plugin root.
        assert_eq!(stdio.cwd.as_deref(), Some(Path::new("/pkg/read.me")));
        // Literal values stay off the definition, which is what the secret
        // store and every projection read.
        assert!(stdio.env.is_empty());
        let launch = stdio.launch.as_ref().unwrap();
        assert_eq!(launch.env["MODE"], "/data/read.me");
        let environment = launch_environment(launch);
        assert_eq!(environment[PLUGIN_ROOT_VARIABLE], "/pkg/read.me");
        assert_eq!(environment[PLUGIN_DATA_VARIABLE], "/data/read.me");

        let legacy = &definitions[1];
        assert!(!legacy.enabled);
        assert_eq!(
            legacy.launch.as_ref().unwrap().disabled_reason.as_deref(),
            Some(SSE_UNSUPPORTED_DIAGNOSTIC)
        );

        let remote = &definitions[2];
        assert!(remote.enabled);
        assert_eq!(remote.launch.as_ref().unwrap().headers["x-client"], "ow");

        // A `${PLUGIN_DATA}`-anchored working directory resolves under the
        // data tree and remembers that it is anchored there, so the launch
        // check judges it against the root it actually named. Judging it
        // against the package root — the only anchor `cwd` had before — would
        // make every server that keeps state in its own directory unable to
        // start.
        let stateful = &definitions[3];
        assert_eq!(
            stateful.cwd.as_deref(),
            Some(Path::new("/data/read.me/state"))
        );
        assert_eq!(
            stateful.launch.as_ref().unwrap().cwd_anchor,
            crate::mcp_config::CwdAnchor::Data
        );
    }

    /// Contract: a name that cannot fit the namespace limit is shortened
    /// deterministically, and two long names sharing a prefix stay distinct.
    #[test]
    fn long_names_shorten_to_a_stable_distinct_namespace() {
        let long = "a".repeat(40);
        let first = runtime_server_name(&long, "one-server-name");
        let second = runtime_server_name(&long, "two-server-name");
        assert_eq!(first.len(), MAX_RUNTIME_NAME_BYTES);
        assert_ne!(first, second);
        assert_eq!(first, runtime_server_name(&long, "one-server-name"));
        assert!(first
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')));
    }
}
