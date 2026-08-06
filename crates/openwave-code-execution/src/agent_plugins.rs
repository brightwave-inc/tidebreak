//! The Agent Plugins packaging format (<https://agent-plugins.org>), v1.0.0.
//!
//! A plugin published in that format is a directory whose root carries a
//! `plugin.json` manifest, with components discovered at fixed locations —
//! skills under `skills/`, one directory each. This module is the reader for
//! that manifest: it validates the closed schema the specification defines and
//! hands back the few facts OpenWave's own [`crate::plugins`] representation
//! needs, so the importer can convert a standard package into the internal
//! `PLUGIN.md` shape the loaders already understand.
//!
//! Two properties of the specification drive the shape of this code:
//!
//! * **`$schema` selects the validation rules.** It is required, and a client
//!   that does not implement the identifier it names must reject the plugin
//!   rather than guess. Schemas are never fetched while loading.
//! * **Failures are graded.** Only two manifest violations are non-fatal —
//!   unknown top-level fields, and an `extensions` value that is not an
//!   object — and both are reported and ignored. Everything else rejects the
//!   plugin whole, so a package never loads in a shape its author did not
//!   describe.
//!
//! Client-specific data rides in `extensions` under reverse-domain
//! namespaces. OpenWave reads [`OPENWAVE_EXTENSION_NAMESPACE`] and ignores
//! every other namespace without inspecting it, which is what the
//! specification requires of a client that does not implement one. Because
//! that namespace is ours, a malformed value inside it is reported and ignored
//! rather than fatal: the plugin still describes itself correctly to every
//! other client.

use crate::plugins::{is_valid_plugin_router_preamble, PluginCategory};

/// The manifest file at the root of a plugin published in the standard format.
pub const AGENT_PLUGIN_MANIFEST_FILE: &str = "plugin.json";

/// The fixed location standard-format skills are discovered at.
pub const AGENT_PLUGIN_SKILLS_DIR: &str = "skills";

/// The specification version this client implements.
pub const AGENT_PLUGIN_SPEC_VERSION: &str = "1.0.0";

/// The only `$schema` identifier [`parse_agent_plugin_manifest`] accepts.
pub const AGENT_PLUGIN_SCHEMA_ID: &str =
    "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";

/// The reverse-domain namespace OpenWave's own manifest data lives under.
pub const OPENWAVE_EXTENSION_NAMESPACE: &str = "io.brightwave.openwave";

const MAX_NAME_CHARS: usize = 64;

/// The manifest fields OpenWave acts on, after validation.
///
/// Metadata the specification type-checks but this client has no consumer for
/// — `version`, `author`, `homepage`, `repository`, `license`, `keywords` — is
/// validated and dropped rather than carried into a representation nothing
/// renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPluginManifest {
    /// The package name, in the specification's grammar (which admits `.`,
    /// unlike OpenWave's own slug).
    pub name: String,
    /// Free-form description, unbounded and unsanitized as the specification
    /// leaves it. Callers rendering it must bound it themselves.
    pub description: Option<String>,
    /// `category` from the OpenWave extension namespace.
    pub category: Option<PluginCategory>,
    /// `router-preamble` from the OpenWave extension namespace, already
    /// checked against the same one-line rule the internal parser applies.
    pub router_preamble: Option<String>,
}

/// One manifest field that was reported and ignored instead of rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredManifestField {
    /// Pointer-ish location of the field, e.g. `extensions` or `keywords`.
    pub field: String,
    pub reason: String,
}

/// A manifest that validated, with the non-fatal violations found along the
/// way. The specification recommends surfacing these, so they are returned
/// rather than logged and dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAgentPluginManifest {
    pub manifest: AgentPluginManifest,
    pub ignored: Vec<IgnoredManifestField>,
}

/// Why a `plugin.json` was rejected whole.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid plugin.json: {0}")]
pub struct AgentPluginParseError(String);

fn invalid(reason: impl Into<String>) -> AgentPluginParseError {
    AgentPluginParseError(reason.into())
}

/// Whether `name` matches the specification's package-name grammar: 1–64
/// characters of `a-z`, `0-9`, `-`, and `.`, alphanumeric at both ends, with
/// no `--` and no `..`.
///
/// Written by hand rather than as a pattern because the published schema
/// expresses the repeat prohibitions with lookahead, which the `regex` crate
/// deliberately does not support.
#[must_use]
pub fn is_valid_agent_plugin_name(name: &str) -> bool {
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_NAME_CHARS
        && bytes
            .iter()
            .all(|byte| alphanumeric(*byte) || matches!(byte, b'-' | b'.'))
        && bytes.first().is_some_and(|byte| alphanumeric(*byte))
        && bytes.last().is_some_and(|byte| alphanumeric(*byte))
        && !name.contains("--")
        && !name.contains("..")
}

/// Parse and validate one `plugin.json` against the v1.0.0 schema.
///
/// The manifest is a closed object: `$schema`, `name`, `version`,
/// `description`, `author`, `homepage`, `repository`, `license`, `keywords`,
/// and `extensions` are the whole vocabulary. `$schema` and `name` are
/// required. Optional metadata is checked by JSON type only — a version that
/// is not SemVer, a license that is not an SPDX identifier, or a URL this
/// client cannot parse are all valid manifests, and rejecting them would make
/// this client stricter than the format it claims to read.
pub fn parse_agent_plugin_manifest(
    source: &str,
) -> Result<ParsedAgentPluginManifest, AgentPluginParseError> {
    let value: serde_json::Value = serde_json::from_str(source)
        .map_err(|error| invalid(format!("not valid JSON: {error}")))?;
    let serde_json::Value::Object(object) = value else {
        return Err(invalid("manifest is not a JSON object"));
    };

    let schema = object
        .get("$schema")
        .ok_or_else(|| invalid("missing '$schema'"))?
        .as_str()
        .ok_or_else(|| invalid("'$schema' is not a string"))?;
    if schema != AGENT_PLUGIN_SCHEMA_ID {
        return Err(invalid(format!(
            "'$schema' names {schema:?}, which is not the supported \
             Agent Plugins {AGENT_PLUGIN_SPEC_VERSION} manifest schema"
        )));
    }

    let name = object
        .get("name")
        .ok_or_else(|| invalid("missing 'name'"))?
        .as_str()
        .ok_or_else(|| invalid("'name' is not a string"))?;
    if !is_valid_agent_plugin_name(name) {
        return Err(invalid(format!(
            "'name' does not match the package-name grammar: {name:?}"
        )));
    }

    let mut ignored = Vec::new();
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "$schema"
                | "name"
                | "version"
                | "description"
                | "author"
                | "homepage"
                | "repository"
                | "license"
                | "keywords"
                | "extensions"
        ) {
            ignored.push(IgnoredManifestField {
                field: key.clone(),
                reason: format!(
                    "field is not part of the Agent Plugins {AGENT_PLUGIN_SPEC_VERSION} \
                     manifest schema"
                ),
            });
        }
    }

    for key in [
        "version",
        "description",
        "homepage",
        "repository",
        "license",
    ] {
        if let Some(value) = object.get(key) {
            if !value.is_string() {
                return Err(invalid(format!("'{key}' is not a string")));
            }
        }
    }
    if let Some(keywords) = object.get("keywords") {
        let keywords = keywords
            .as_array()
            .ok_or_else(|| invalid("'keywords' is not an array"))?;
        if !keywords.iter().all(serde_json::Value::is_string) {
            return Err(invalid("'keywords' contains a non-string entry"));
        }
    }
    if let Some(author) = object.get("author") {
        let author = author
            .as_object()
            .ok_or_else(|| invalid("'author' is not an object"))?;
        for (key, value) in author {
            if !matches!(key.as_str(), "name" | "email" | "url") {
                return Err(invalid(format!("'author' has unknown field {key:?}")));
            }
            if !value.is_string() {
                return Err(invalid(format!("'author.{key}' is not a string")));
            }
        }
    }

    let mut category = None;
    let mut router_preamble = None;
    match object.get("extensions") {
        None => {}
        // The one shape the specification downgrades to a warning: a client
        // that cannot read the extension block still loads the components.
        Some(value) if !value.is_object() => ignored.push(IgnoredManifestField {
            field: "extensions".to_owned(),
            reason: "value is not an object".to_owned(),
        }),
        Some(value) => {
            let extensions = value.as_object().expect("checked above");
            for (namespace, value) in extensions {
                if !value.is_object() {
                    return Err(invalid(format!(
                        "'extensions.{namespace}' is not an object"
                    )));
                }
            }
            // Every other namespace is ignored without inspecting it, which
            // is exactly what the specification asks of a client that does
            // not implement one.
            if let Some(ours) = extensions
                .get(OPENWAVE_EXTENSION_NAMESPACE)
                .and_then(serde_json::Value::as_object)
            {
                for (key, value) in ours {
                    let field = format!("extensions.{OPENWAVE_EXTENSION_NAMESPACE}.{key}");
                    match key.as_str() {
                        "category" => match value.as_str().and_then(PluginCategory::parse) {
                            Some(parsed) => category = Some(parsed),
                            None => ignored.push(IgnoredManifestField {
                                field,
                                reason: "value is not one of the supported categories".to_owned(),
                            }),
                        },
                        "router-preamble" => match value.as_str() {
                            Some(preamble) if is_valid_plugin_router_preamble(preamble) => {
                                router_preamble = Some(preamble.to_owned());
                            }
                            _ => ignored.push(IgnoredManifestField {
                                field,
                                reason: "value is not one bounded printable line".to_owned(),
                            }),
                        },
                        _ => ignored.push(IgnoredManifestField {
                            field,
                            reason: "field is not read by this client".to_owned(),
                        }),
                    }
                }
            }
        }
    }

    Ok(ParsedAgentPluginManifest {
        manifest: AgentPluginManifest {
            name: name.to_owned(),
            description: object
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            category,
            router_preamble,
        },
        ignored,
    })
}

/// The bundled MCP server configuration file, at the package root.
pub const AGENT_PLUGIN_MCP_FILE: &str = "mcp.json";

/// The only `mcp.json` `$schema` identifier this client accepts. Both schemas
/// share the specification version, and a file whose version disagrees with the
/// manifest's disables MCP for that plugin alone.
pub const AGENT_PLUGIN_MCP_SCHEMA_ID: &str =
    "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

/// The absolute, resolved plugin root, provided to a launched server.
pub const PLUGIN_ROOT_VARIABLE: &str = "PLUGIN_ROOT";
/// The client-managed writable data directory, provided to a launched server.
pub const PLUGIN_DATA_VARIABLE: &str = "PLUGIN_DATA";

const MAX_MCP_SERVERS: usize = 16;
const MAX_MCP_SERVER_NAME_BYTES: usize = 64;
const MAX_MCP_STRING_BYTES: usize = 2_048;
const MAX_MCP_LIST_ENTRIES: usize = 64;

/// A `stdio` server: a subprocess the client launches from the plugin.
///
/// `${PLUGIN_ROOT}` and `${PLUGIN_DATA}` are expanded in `args`, `env` values,
/// and `cwd` — never in `command` — in a single non-recursive textual pass, so
/// replacement text is never rescanned and no other placeholder-like text is
/// touched. That expansion happens when a server is actually launched; nothing
/// here starts a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStdioServer {
    /// One executable token: a bare program name resolved by the platform, or
    /// a `./`-relative path inside the plugin. Never expanded.
    pub command: String,
    pub args: Vec<String>,
    /// Literal package data, not a secret mechanism — the specification is
    /// explicit that these values are visible. Never log them.
    pub env: std::collections::BTreeMap<String, String>,
    /// Absent means the plugin root. When present it is `./`-relative or
    /// rooted at one of the two reserved variables, and stays contained inside
    /// that root after expansion.
    pub cwd: Option<String>,
}

/// A `streamable-http` or legacy `sse` server: a remote endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpHttpServer {
    /// Absolute HTTP(S) URL with no userinfo and no fragment; plain HTTP only
    /// for loopback. Never expanded.
    pub url: String,
    /// Literal headers, unique case-insensitively. Visible package data, like
    /// `env`: never log the values.
    pub headers: std::collections::BTreeMap<String, String>,
}

/// How a configured server is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    Stdio(McpStdioServer),
    StreamableHttp(McpHttpServer),
    /// The legacy transport the specification keeps optional.
    Sse(McpHttpServer),
}

impl McpTransport {
    fn type_name(&self) -> &'static str {
        match self {
            Self::Stdio(_) => "stdio",
            Self::StreamableHttp(_) => "streamable-http",
            Self::Sse(_) => "sse",
        }
    }
}

/// One validated server entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub transport: McpTransport,
}

/// A plugin's validated MCP configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginMcpConfig {
    /// Valid entries in name order. Empty is a legitimate configuration.
    pub servers: Vec<McpServer>,
}

/// One server entry that was dropped, with the reason it did not validate.
///
/// A reason never quotes a header value or an `env` value: those are visible
/// package data the specification tells clients not to treat as secrets, which
/// is exactly why they should not be copied into logs and API responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedMcpServer {
    pub name: String,
    pub reason: String,
}

/// A configuration that loaded, with the entries dropped along the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPluginMcpConfig {
    pub config: PluginMcpConfig,
    pub skipped: Vec<SkippedMcpServer>,
}

/// Why a whole `mcp.json` was rejected. The specification grades this as
/// disabling MCP for that plugin only: every other component still loads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid mcp.json: {0}")]
pub struct McpConfigError(String);

fn invalid_mcp(reason: impl Into<String>) -> McpConfigError {
    McpConfigError(reason.into())
}

/// Parse and validate one `mcp.json`.
///
/// Failures are graded the way §7.2.2 grades them. A file that is not JSON, or
/// whose `$schema` this client does not implement, or whose top level is not
/// exactly `$schema` and `mcpServers`, disables MCP for the plugin — the
/// returned `Err`. A single entry that does not validate is skipped and
/// reported, leaving its siblings alone.
pub fn parse_plugin_mcp_config(source: &str) -> Result<ParsedPluginMcpConfig, McpConfigError> {
    let value: serde_json::Value = serde_json::from_str(source)
        .map_err(|error| invalid_mcp(format!("not valid JSON: {error}")))?;
    let serde_json::Value::Object(object) = value else {
        return Err(invalid_mcp("configuration is not a JSON object"));
    };
    if let Some(unknown) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "$schema" | "mcpServers"))
    {
        return Err(invalid_mcp(format!("unknown top-level field {unknown:?}")));
    }
    let schema = object
        .get("$schema")
        .ok_or_else(|| invalid_mcp("missing '$schema'"))?
        .as_str()
        .ok_or_else(|| invalid_mcp("'$schema' is not a string"))?;
    if schema != AGENT_PLUGIN_MCP_SCHEMA_ID {
        return Err(invalid_mcp(format!(
            "'$schema' names {schema:?}, which is not the Agent Plugins \
             {AGENT_PLUGIN_SPEC_VERSION} MCP schema this plugin targets"
        )));
    }
    let servers = object
        .get("mcpServers")
        .ok_or_else(|| invalid_mcp("missing 'mcpServers'"))?
        .as_object()
        .ok_or_else(|| invalid_mcp("'mcpServers' is not an object"))?;
    if servers.len() > MAX_MCP_SERVERS {
        return Err(invalid_mcp("'mcpServers' declares too many servers"));
    }

    let mut parsed = Vec::new();
    let mut skipped = Vec::new();
    for (name, entry) in servers {
        let reported = bounded_server_name(name);
        if !is_valid_mcp_server_name(name) {
            skipped.push(SkippedMcpServer {
                name: reported,
                reason: "server name is not one bounded printable token".to_owned(),
            });
            continue;
        }
        match parse_mcp_server(entry) {
            Ok(transport) => parsed.push(McpServer {
                name: name.clone(),
                transport,
            }),
            Err(reason) => skipped.push(SkippedMcpServer {
                name: reported,
                reason,
            }),
        }
    }
    parsed.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(ParsedPluginMcpConfig {
        config: PluginMcpConfig { servers: parsed },
        skipped,
    })
}

fn bounded_server_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_control())
        .take(MAX_MCP_SERVER_NAME_BYTES)
        .collect()
}

fn is_valid_mcp_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_MCP_SERVER_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Validate one entry of `mcpServers`. The three transports are a closed union
/// selected by `type`; an unknown field, an unknown `type`, or a field that
/// belongs to another variant invalidates this entry and only this entry.
fn parse_mcp_server(entry: &serde_json::Value) -> Result<McpTransport, String> {
    let entry = entry.as_object().ok_or("entry is not an object")?;
    let transport = entry
        .get("type")
        .ok_or("entry has no 'type'")?
        .as_str()
        .ok_or("'type' is not a string")?;
    let allowed: &[&str] = match transport {
        "stdio" => &["type", "command", "args", "env", "cwd"],
        "streamable-http" | "sse" => &["type", "url", "headers"],
        other => return Err(format!("transport {other:?} is not one this client reads")),
    };
    if let Some(unknown) = entry.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "field {unknown:?} does not belong to a {transport:?} entry"
        ));
    }
    if transport == "stdio" {
        return Ok(McpTransport::Stdio(parse_stdio_server(entry)?));
    }
    let http = parse_http_server(entry)?;
    Ok(if transport == "sse" {
        McpTransport::Sse(http)
    } else {
        McpTransport::StreamableHttp(http)
    })
}

fn parse_stdio_server(
    entry: &serde_json::Map<String, serde_json::Value>,
) -> Result<McpStdioServer, String> {
    let command = entry
        .get("command")
        .ok_or("stdio entry has no 'command'")?
        .as_str()
        .ok_or("'command' is not a string")?;
    if !is_valid_command_token(command) {
        return Err("'command' is not a single executable token inside the plugin".to_owned());
    }
    let mut args = Vec::new();
    if let Some(value) = entry.get("args") {
        let listed = value.as_array().ok_or("'args' is not an array")?;
        if listed.len() > MAX_MCP_LIST_ENTRIES {
            return Err("'args' has too many entries".to_owned());
        }
        for argument in listed {
            let argument = argument.as_str().ok_or("'args' has a non-string entry")?;
            if argument.len() > MAX_MCP_STRING_BYTES {
                return Err("'args' has an entry beyond the byte limit".to_owned());
            }
            args.push(argument.to_owned());
        }
    }
    let mut env = std::collections::BTreeMap::new();
    if let Some(value) = entry.get("env") {
        let listed = value.as_object().ok_or("'env' is not an object")?;
        if listed.len() > MAX_MCP_LIST_ENTRIES {
            return Err("'env' has too many entries".to_owned());
        }
        for (key, value) in listed {
            if !is_valid_env_name(key) {
                return Err("'env' names a variable that is not a portable name".to_owned());
            }
            // The client sets both reserved variables last, so an entry naming
            // one is describing an effect it cannot have.
            if key == PLUGIN_ROOT_VARIABLE || key == PLUGIN_DATA_VARIABLE {
                return Err(format!("'env' may not set the reserved {key} variable"));
            }
            let value = value.as_str().ok_or("'env' has a non-string value")?;
            if value.len() > MAX_MCP_STRING_BYTES {
                return Err("'env' has a value beyond the byte limit".to_owned());
            }
            env.insert(key.clone(), value.to_owned());
        }
    }
    let cwd = match entry.get("cwd") {
        None => None,
        Some(value) => {
            let cwd = value.as_str().ok_or("'cwd' is not a string")?;
            if !is_contained_working_directory(cwd) {
                return Err("'cwd' is not contained in the plugin root or its data \
                            directory"
                    .to_owned());
            }
            Some(cwd.to_owned())
        }
    };
    Ok(McpStdioServer {
        command: command.to_owned(),
        args,
        env,
        cwd,
    })
}

fn parse_http_server(
    entry: &serde_json::Map<String, serde_json::Value>,
) -> Result<McpHttpServer, String> {
    let url = entry
        .get("url")
        .ok_or("entry has no 'url'")?
        .as_str()
        .ok_or("'url' is not a string")?;
    let parsed = url::Url::parse(url).map_err(|_| "'url' is not an absolute URL".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("'url' is not an HTTP or HTTPS endpoint".to_owned());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("'url' carries userinfo".to_owned());
    }
    if parsed.fragment().is_some() {
        return Err("'url' carries a fragment".to_owned());
    }
    if parsed.scheme() == "http" && !is_loopback_host(&parsed) {
        return Err("plain HTTP is only allowed for a loopback endpoint".to_owned());
    }
    if url.len() > MAX_MCP_STRING_BYTES {
        return Err("'url' exceeds the byte limit".to_owned());
    }
    let mut headers = std::collections::BTreeMap::new();
    if let Some(value) = entry.get("headers") {
        let listed = value.as_object().ok_or("'headers' is not an object")?;
        if listed.len() > MAX_MCP_LIST_ENTRIES {
            return Err("'headers' has too many entries".to_owned());
        }
        for (name, value) in listed {
            if !is_valid_header_name(name) {
                return Err("'headers' has a name that is not an HTTP field name".to_owned());
            }
            let lowercase = name.to_ascii_lowercase();
            let value = value.as_str().ok_or("'headers' has a non-string value")?;
            if value.len() > MAX_MCP_STRING_BYTES || !is_valid_header_value(value) {
                return Err("'headers' has a value that is not a valid field value".to_owned());
            }
            if headers.insert(lowercase, value.to_owned()).is_some() {
                return Err("'headers' names the same field twice".to_owned());
            }
        }
    }
    Ok(McpHttpServer {
        url: url.to_owned(),
        headers,
    })
}

fn is_loopback_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// Whether `command` is the single executable token the specification allows:
/// a bare program name the platform resolves, or a `./`-relative path that
/// stays inside the plugin. No placeholder is expanded in a command, so one
/// appearing here would be launched literally and is refused instead.
fn is_valid_command_token(command: &str) -> bool {
    if command.is_empty()
        || command.len() > MAX_MCP_STRING_BYTES
        || command.contains("${")
        || command.chars().any(char::is_whitespace)
        || command.chars().any(char::is_control)
        || command.contains('\\')
    {
        return false;
    }
    match command.strip_prefix("./") {
        Some(relative) => is_contained_relative_path(relative),
        None => !command.contains('/') && command != "." && command != "..",
    }
}

/// Whether `cwd` names a directory inside one of the two roots it may be
/// rooted at. Containment is checked on the path that remains after the
/// placeholder, which is what expansion will produce.
fn is_contained_working_directory(cwd: &str) -> bool {
    if cwd.len() > MAX_MCP_STRING_BYTES || cwd.chars().any(char::is_control) {
        return false;
    }
    for variable in [PLUGIN_ROOT_VARIABLE, PLUGIN_DATA_VARIABLE] {
        if let Some(rest) = cwd.strip_prefix(&format!("${{{variable}}}")) {
            return rest.is_empty()
                || rest
                    .strip_prefix('/')
                    .is_some_and(is_contained_relative_path);
        }
    }
    cwd.strip_prefix("./")
        .is_some_and(is_contained_relative_path)
}

fn is_contained_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains("${")
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn is_valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.as_bytes()[0].is_ascii_digit()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// An HTTP field name is a token: the `tchar` set from RFC 9110.
fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn is_valid_header_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte == b'\t' || (0x20..0x7f).contains(&byte))
}

/// Render a validated configuration back as `mcp.json`.
///
/// The installed copy is regenerated rather than copied so that only entries
/// that passed validation are ever written, and so the file the loader reads
/// back is the one this client produced.
#[must_use]
pub fn canonical_mcp_config(config: &PluginMcpConfig) -> String {
    let mut servers = serde_json::Map::new();
    for server in &config.servers {
        let mut entry = serde_json::Map::new();
        entry.insert(
            "type".to_owned(),
            server.transport.type_name().to_owned().into(),
        );
        match &server.transport {
            McpTransport::Stdio(stdio) => {
                entry.insert("command".to_owned(), stdio.command.clone().into());
                if !stdio.args.is_empty() {
                    entry.insert("args".to_owned(), stdio.args.clone().into());
                }
                if !stdio.env.is_empty() {
                    entry.insert(
                        "env".to_owned(),
                        serde_json::Value::Object(
                            stdio
                                .env
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone().into()))
                                .collect(),
                        ),
                    );
                }
                if let Some(cwd) = &stdio.cwd {
                    entry.insert("cwd".to_owned(), cwd.clone().into());
                }
            }
            McpTransport::StreamableHttp(http) | McpTransport::Sse(http) => {
                entry.insert("url".to_owned(), http.url.clone().into());
                if !http.headers.is_empty() {
                    entry.insert(
                        "headers".to_owned(),
                        serde_json::Value::Object(
                            http.headers
                                .iter()
                                .map(|(name, value)| (name.clone(), value.clone().into()))
                                .collect(),
                        ),
                    );
                }
            }
        }
        servers.insert(server.name.clone(), serde_json::Value::Object(entry));
    }
    let document = serde_json::json!({
        "$schema": AGENT_PLUGIN_MCP_SCHEMA_ID,
        "mcpServers": serde_json::Value::Object(servers),
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&document).expect("a validated configuration serializes")
    )
}

/// Read the `mcp.json` an import retained beside a plugin's manifest.
///
/// The retained file goes through the same parser the importer used, so a
/// hand-edited copy cannot widen what a plugin claims. Anything unreadable or
/// invalid is a plugin with no MCP configuration, warned about and otherwise
/// ignored — the plugin's skills are unaffected.
#[must_use]
pub fn load_plugin_mcp_config(directory: &std::path::Path) -> Option<PluginMcpConfig> {
    let path = directory.join(AGENT_PLUGIN_MCP_FILE);
    let regular_file = path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
    if !regular_file {
        return None;
    }
    let source = match std::fs::read_to_string(&path) {
        Ok(source) if source.len() <= crate::MAX_WORKSPACE_FILE_BYTES => source,
        Ok(_) => {
            tracing::warn!(
                "plugin MCP config {} exceeds the byte limit",
                path.display()
            );
            return None;
        }
        Err(error) => {
            tracing::warn!(
                "plugin MCP config {} is unreadable: {error}",
                path.display()
            );
            return None;
        }
    };
    match parse_plugin_mcp_config(&source) {
        Ok(parsed) => {
            for skipped in &parsed.skipped {
                tracing::warn!(
                    "plugin MCP server {:?} in {} was skipped: {}",
                    skipped.name,
                    path.display(),
                    skipped.reason
                );
            }
            Some(parsed.config)
        }
        Err(error) => {
            tracing::warn!("plugin MCP config {} is invalid: {error}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(body: &str) -> String {
        format!("{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA_ID}\", \"name\": \"reporting\"{body}}}")
    }

    /// Contract: the two documented non-fatal violations keep the plugin
    /// loadable, every other schema violation rejects it, and metadata the
    /// specification only type-checks is never second-guessed.
    #[test]
    fn manifest_violations_are_graded_the_way_the_specification_grades_them() {
        let parsed = parse_agent_plugin_manifest(&manifest(
            ", \"version\": \"not.a.semver-at-all\", \"license\": \"whatever\", \
             \"homepage\": \"not a url\", \"keywords\": [\"a\"], \
             \"author\": {\"name\": \"A\"}, \"surprise\": 1, \"extensions\": 7",
        ))
        .expect("non-fatal violations keep the plugin loadable");
        assert_eq!(
            parsed
                .ignored
                .iter()
                .map(|entry| entry.field.as_str())
                .collect::<Vec<_>>(),
            ["surprise", "extensions"]
        );

        for (case, source) in [
            ("missing $schema", "{\"name\": \"reporting\"}".to_owned()),
            (
                "unsupported $schema",
                "{\"$schema\": \"https://agent-plugins.org/schemas/9.9.9/plugin.schema.json\", \
                 \"name\": \"reporting\"}"
                    .to_owned(),
            ),
            (
                "missing name",
                format!("{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA_ID}\"}}"),
            ),
            ("mistyped description", manifest(", \"description\": 5")),
            ("mistyped keywords", manifest(", \"keywords\": \"a\"")),
            (
                "unknown author field",
                manifest(", \"author\": {\"handle\": \"a\"}"),
            ),
            (
                "non-object namespace",
                manifest(", \"extensions\": {\"com.example\": 3}"),
            ),
            ("not an object", "[]".to_owned()),
        ] {
            assert!(
                parse_agent_plugin_manifest(&source).is_err(),
                "{case} should reject the plugin"
            );
        }

        for name in ["a", "read.me", "a-b.c-9"] {
            assert!(is_valid_agent_plugin_name(name), "{name} should be valid");
        }
        for name in ["", "-a", "a-", "a--b", "a..b", "A", "a_b", &"a".repeat(65)] {
            assert!(
                !is_valid_agent_plugin_name(name),
                "{name} should be invalid"
            );
        }
    }

    /// Contract: our own namespace is read, other namespaces are passed over
    /// untouched, and a value we cannot use inside our namespace is reported
    /// and ignored rather than sinking a package that is valid for everyone
    /// else.
    #[test]
    fn the_openwave_extension_namespace_is_read_and_others_are_left_alone() {
        let parsed = parse_agent_plugin_manifest(&manifest(&format!(
            ", \"extensions\": {{\
               \"com.example.client\": {{\"anything\": [1, 2]}}, \
               \"{OPENWAVE_EXTENSION_NAMESPACE}\": {{\
                 \"category\": \"data\", \
                 \"router-preamble\": \"Pick by the report the user asked for.\"}}}}"
        )))
        .unwrap();
        assert!(parsed.ignored.is_empty());
        assert_eq!(parsed.manifest.category, Some(PluginCategory::Data));
        assert_eq!(
            parsed.manifest.router_preamble.as_deref(),
            Some("Pick by the report the user asked for.")
        );

        let parsed = parse_agent_plugin_manifest(&manifest(&format!(
            ", \"extensions\": {{\"{OPENWAVE_EXTENSION_NAMESPACE}\": {{\
               \"category\": \"wizardry\", \"router-preamble\": \"\", \"future\": 1}}}}"
        )))
        .expect("a value we cannot use is not fatal");
        assert_eq!(parsed.manifest.category, None);
        assert_eq!(parsed.manifest.router_preamble, None);
        assert_eq!(parsed.ignored.len(), 3);
    }

    fn mcp(servers: &str) -> String {
        format!("{{\"$schema\": \"{AGENT_PLUGIN_MCP_SCHEMA_ID}\", \"mcpServers\": {servers}}}")
    }

    /// Contract: an MCP configuration fails narrowly. A bad entry is dropped
    /// and reported while its siblings connect; only a top-level problem
    /// disables the plugin's MCP configuration entirely. Every rule asserted
    /// here is one that would otherwise let a package launch a process or
    /// reach an endpoint it was not supposed to.
    #[test]
    fn mcp_entries_fail_one_at_a_time_and_the_file_fails_as_a_whole() {
        let parsed = parse_plugin_mcp_config(&mcp("{\
              \"local\": {\"type\": \"stdio\", \"command\": \"./bin/serve\", \
                \"args\": [\"--root\", \"${PLUGIN_ROOT}\"], \
                \"env\": {\"MODE\": \"read-only\"}, \"cwd\": \"${PLUGIN_DATA}/state\"}, \
              \"remote\": {\"type\": \"streamable-http\", \
                \"url\": \"https://mcp.example.com/v1\", \
                \"headers\": {\"X-Client\": \"openwave\"}}, \
              \"loopback\": {\"type\": \"sse\", \"url\": \"http://localhost:7331/sse\"}, \
              \"stray-field\": {\"type\": \"stdio\", \"command\": \"serve\", \"retries\": 3}, \
              \"cross-variant\": {\"type\": \"stdio\", \"command\": \"serve\", \
                \"url\": \"https://mcp.example.com\"}, \
              \"unknown-transport\": {\"type\": \"websocket\", \
                \"url\": \"wss://mcp.example.com\"}, \
              \"expanded-command\": {\"type\": \"stdio\", \
                \"command\": \"${PLUGIN_ROOT}/bin/serve\"}, \
              \"escaping-cwd\": {\"type\": \"stdio\", \"command\": \"serve\", \
                \"cwd\": \"../../etc\"}, \
              \"reserved-env\": {\"type\": \"stdio\", \"command\": \"serve\", \
                \"env\": {\"PLUGIN_ROOT\": \"/tmp\"}}, \
              \"plain-http\": {\"type\": \"streamable-http\", \
                \"url\": \"http://mcp.example.com/v1\"}, \
              \"userinfo\": {\"type\": \"streamable-http\", \
                \"url\": \"https://user:pass@mcp.example.com/v1\"}, \
              \"duplicate-header\": {\"type\": \"streamable-http\", \
                \"url\": \"https://mcp.example.com/v1\", \
                \"headers\": {\"X-Client\": \"a\", \"x-client\": \"b\"}}}"))
        .expect("a file whose entries are bad is still a valid file");
        assert_eq!(
            parsed
                .config
                .servers
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>(),
            ["local", "loopback", "remote"]
        );
        let mut skipped = parsed
            .skipped
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>();
        skipped.sort_unstable();
        assert_eq!(
            skipped,
            [
                "cross-variant",
                "duplicate-header",
                "escaping-cwd",
                "expanded-command",
                "plain-http",
                "reserved-env",
                "stray-field",
                "unknown-transport",
                "userinfo",
            ]
        );
        // A reason is renderable and never quotes a header or env value.
        assert!(parsed
            .skipped
            .iter()
            .all(|server| !server.reason.contains("/tmp") && !server.reason.contains('\n')));

        // The retained file is regenerated from what validated, and reading it
        // back yields the same configuration.
        let round_tripped = parse_plugin_mcp_config(&canonical_mcp_config(&parsed.config))
            .expect("the canonical form is valid")
            .config;
        assert_eq!(round_tripped, parsed.config);

        for (case, source) in [
            ("not JSON", "{".to_owned()),
            ("missing schema", "{\"mcpServers\": {}}".to_owned()),
            (
                "schema version the manifest does not target",
                "{\"$schema\": \"https://agent-plugins.org/schemas/9.9.9/mcp.schema.json\", \
                  \"mcpServers\": {}}"
                    .to_owned(),
            ),
            (
                "unknown top-level field",
                format!(
                    "{{\"$schema\": \"{AGENT_PLUGIN_MCP_SCHEMA_ID}\", \"mcpServers\": {{}}, \
                      \"timeout\": 5}}"
                ),
            ),
            ("mcpServers is not an object", mcp("[]")),
        ] {
            assert!(
                parse_plugin_mcp_config(&source).is_err(),
                "{case} should disable MCP for the plugin"
            );
        }

        // An empty server map is a valid configuration that claims nothing.
        assert!(parse_plugin_mcp_config(&mcp("{}"))
            .unwrap()
            .config
            .servers
            .is_empty());
    }
}
