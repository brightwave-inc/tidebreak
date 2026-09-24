//! Validation for MCP server definitions.

use std::collections::HashSet;

use tidebreak_core::{AgentError, Result};
use tidebreak_mcp::MAX_SERVER_NAME_BYTES;

use super::oauth::OAuthNeed;
use super::types::*;

pub(super) fn validate_servers(servers: &[McpServerDefinition]) -> Result<()> {
    if servers.len() > MAX_SERVERS {
        return Err(AgentError::config(format!(
            "MCP config contains more than {MAX_SERVERS} servers"
        )));
    }
    let mut names = HashSet::new();
    for server in servers {
        validate_server(server)?;
        if !names.insert(server.name.clone()) {
            return Err(server_error(&server.name, "server name is duplicated"));
        }
    }
    Ok(())
}

/// Every check [`validate_servers`] makes that concerns one server alone.
///
/// The loader runs this per saved record, so one invalid record is skipped
/// with its reason instead of failing the whole set.
pub(super) fn validate_server(server: &McpServerDefinition) -> Result<()> {
    validate_name(&server.name)?;
    match (&server.command, &server.url, &server.gateway_endpoint) {
        (None, None, None) => {
            return Err(server_error(
                &server.name,
                "must configure a command, a url, or a gateway endpoint",
            ));
        }
        (Some(command), None, None) => {
            validate_process_string(&server.name, "command", command)?;
            if command.is_empty() {
                return Err(server_error(&server.name, "command must not be empty"));
            }
            if server.bearer_token_env.is_some() {
                return Err(server_error(
                    &server.name,
                    "bearer_token_env applies only to url servers",
                ));
            }
            validate_no_http_fields(server)?;
        }
        (None, Some(url), None) => {
            validate_process_string(&server.name, "url", url)?;
            tidebreak_mcp::validate_http_url_with_credentials(url, sends_credentials(server))
                .map_err(|error| server_error(&server.name, error))?;
            validate_no_process_fields(server)?;
            let authentications = [
                server.bearer_token_env.is_some(),
                server.bearer_token_stored,
                server.oauth,
            ]
            .into_iter()
            .filter(|chosen| *chosen)
            .count();
            if authentications > 1 {
                return Err(server_error(
                    &server.name,
                    "choose one way to authenticate: oauth, bearer_token_env, or \
                     bearer_token_stored; an OAuth server obtains its bearer through \
                     sign-in, not a static token",
                ));
            }
            if let Some(bearer_name) = &server.bearer_token_env {
                validate_environment_name(&server.name, bearer_name)?;
            }
            validate_http_credentials(server)?;
        }
        (None, None, Some(slug)) => {
            validate_gateway_endpoint_slug(&server.name, slug)?;
            validate_no_process_fields(server)?;
            if server.bearer_token_env.is_some() {
                return Err(server_error(
                    &server.name,
                    "bearer_token_env applies only to url servers; a gateway \
                     endpoint's bearer comes from the signed-in session",
                ));
            }
            validate_no_http_fields(server)?;
        }
        _ => {
            return Err(server_error(
                &server.name,
                "must configure exactly one of command, url, or gateway endpoint",
            ));
        }
    }
    if server.oauth && server.url.is_none() {
        return Err(server_error(
            &server.name,
            "oauth applies only to url servers",
        ));
    }
    if server.args.len() > MAX_ARGS {
        return Err(server_error(
            &server.name,
            format!("must not contain more than {MAX_ARGS} arguments"),
        ));
    }
    for argument in &server.args {
        validate_process_string(&server.name, "argument", argument)?;
    }
    if server.env.len().saturating_add(server.env_from.len()) > MAX_ENVIRONMENT_VARIABLES {
        return Err(server_error(
            &server.name,
            format!("must not contain more than {MAX_ENVIRONMENT_VARIABLES} environment variables"),
        ));
    }
    let mut environment_names = HashSet::new();
    for key in &server.env {
        validate_environment_name(&server.name, key)?;
        environment_names.insert(key.as_str());
    }
    for (key, value) in &server.env_values {
        if !server.env.contains(key) {
            return Err(server_error(
                &server.name,
                format!("environment value {key:?} names no configured variable"),
            ));
        }
        validate_process_string(&server.name, "environment value", value)?;
    }
    for key in &server.env_from {
        validate_environment_name(&server.name, key)?;
        if !environment_names.insert(key) {
            return Err(server_error(
                &server.name,
                format!("environment variable {key:?} is configured more than once"),
            ));
        }
    }
    if let Some(path) = server.cwd.as_ref().and_then(|path| path.to_str()) {
        validate_process_string(&server.name, "working directory", path)?;
    }
    if !(1..=MAX_REQUEST_TIMEOUT_MS).contains(&server.request_timeout_ms) {
        return Err(server_error(
            &server.name,
            format!("request_timeout_ms must be between 1 and {MAX_REQUEST_TIMEOUT_MS}"),
        ));
    }
    Ok(())
}

/// Why a validation error refused one server, without the server's name in
/// front: the Connected apps list shows the name beside it. Validation
/// messages name fields and environment variable names, never a value.
pub(super) fn validation_reason(name: &str, error: &AgentError) -> String {
    let message = match error {
        AgentError::Config(message) | AgentError::Message(message) => message.as_str(),
        _ => return "Its saved settings are not valid.".to_string(),
    };
    let prefix = format!("invalid external MCP server {name:?}: ");
    let detail = message.strip_prefix(&prefix).unwrap_or(message);
    let detail = detail.strip_prefix("MCP client error: ").unwrap_or(detail);
    format!("Its saved settings are not valid: {detail}.")
}

/// Remote transports (url and gateway endpoint) never spawn a child, so no
/// process field may accompany them.
pub(super) fn validate_no_process_fields(server: &McpServerDefinition) -> Result<()> {
    if !server.args.is_empty() {
        return Err(server_error(
            &server.name,
            "args apply only to command servers",
        ));
    }
    if !server.env.is_empty() || !server.env_from.is_empty() {
        return Err(server_error(
            &server.name,
            "process environment applies only to command servers",
        ));
    }
    if server.cwd.is_some() {
        return Err(server_error(
            &server.name,
            "cwd applies only to command servers",
        ));
    }
    Ok(())
}

/// Whether a url server sends a credential with its requests: a bearer from
/// either source, an OAuth token, or a custom header, whose value may be an
/// API key. A server that does must use https unless its URL names a literal
/// loopback address.
pub(super) fn sends_credentials(server: &McpServerDefinition) -> bool {
    server.bearer_token_env.is_some()
        || server.bearer_token_stored
        || server.oauth
        || !server.headers.is_empty()
}

/// A stored bearer and custom headers belong to url servers only.
fn validate_no_http_fields(server: &McpServerDefinition) -> Result<()> {
    if server.bearer_token_stored || server.bearer_token_value.is_some() {
        return Err(server_error(
            &server.name,
            "a stored bearer token applies only to url servers",
        ));
    }
    if !server.headers.is_empty() || !server.header_values.is_empty() {
        return Err(server_error(
            &server.name,
            "headers apply only to url servers",
        ));
    }
    Ok(())
}

/// Header names the transport owns or that describe one connection rather
/// than the request, lowercase. A custom header may name none of them.
const REFUSED_HEADERS: &[&str] = &[
    // Hop-by-hop fields (RFC 9110 §7.6.1) describe a single connection.
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    // Framing and routing belong to the request the transport builds.
    "content-encoding",
    "content-length",
    "expect",
    "host",
    // Proxy credentials go to a proxy, not the server.
    "proxy-authenticate",
    "proxy-authorization",
    // The transport sets these itself.
    "accept",
    "content-type",
    "mcp-protocol-version",
    "mcp-session-id",
];

/// A stored bearer token and custom headers: bounded, header-safe, and never
/// echoed. Errors name a field or a header name, never a value.
fn validate_http_credentials(server: &McpServerDefinition) -> Result<()> {
    if let Some(value) = &server.bearer_token_value {
        if !server.bearer_token_stored {
            return Err(server_error(
                &server.name,
                "bearer_token_value applies only with bearer_token_stored",
            ));
        }
        validate_credential_value(&server.name, "the bearer token", value)?;
        let lower = value.trim_start().to_ascii_lowercase();
        if lower.starts_with("bearer ") {
            return Err(server_error(
                &server.name,
                "enter the bearer token without the \"Bearer\" prefix",
            ));
        }
    }
    if server.headers.len() > MAX_HEADERS {
        return Err(server_error(
            &server.name,
            format!("must not send more than {MAX_HEADERS} custom headers"),
        ));
    }
    let mut lowercase = HashSet::new();
    for name in &server.headers {
        validate_header_name(&server.name, name)?;
        if !lowercase.insert(name.to_ascii_lowercase()) {
            return Err(server_error(
                &server.name,
                format!("header {name:?} is configured more than once"),
            ));
        }
    }
    for (name, value) in &server.header_values {
        if !server.headers.contains(name) {
            return Err(server_error(
                &server.name,
                format!("header value {name:?} names no configured header"),
            ));
        }
        validate_credential_value(
            &server.name,
            &format!("the value of header {name:?}"),
            value,
        )?;
    }
    Ok(())
}

fn validate_header_name(server_name: &str, name: &str) -> Result<()> {
    // RFC 9110 §5.6.2 `token` characters.
    let token = !name.is_empty()
        && name.len() <= MAX_HEADER_NAME_BYTES
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
        });
    if !token {
        return Err(server_error(
            server_name,
            format!(
                "header name {name:?} is not a valid HTTP field name of at most \
                 {MAX_HEADER_NAME_BYTES} characters"
            ),
        ));
    }
    let lower = name.to_ascii_lowercase();
    if lower == "authorization" {
        return Err(server_error(
            server_name,
            "set the bearer token under authentication instead of an Authorization header",
        ));
    }
    if REFUSED_HEADERS.contains(&lower.as_str()) {
        return Err(server_error(
            server_name,
            format!("header {name:?} is not allowed: the connection sets it itself"),
        ));
    }
    Ok(())
}

/// A value sent in a header: not empty, at most
/// [`MAX_CREDENTIAL_VALUE_BYTES`], and visible ASCII, space, or tab only, so
/// it can never smuggle header syntax. `what` names the value in the error.
fn validate_credential_value(server_name: &str, what: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(server_error(
            server_name,
            format!("{what} must not be empty"),
        ));
    }
    if value.len() > MAX_CREDENTIAL_VALUE_BYTES {
        return Err(server_error(
            server_name,
            format!("{what} exceeds {MAX_CREDENTIAL_VALUE_BYTES} bytes"),
        ));
    }
    if !value
        .bytes()
        .all(|byte| byte == b'\t' || (b' '..=b'~').contains(&byte))
    {
        return Err(server_error(
            server_name,
            format!("{what} must be visible ASCII text"),
        ));
    }
    Ok(())
}

/// The gateway's endpoint-slug contract, checked when the configuration is
/// saved rather than when a connection first resolves it. The contract
/// itself lives in one place — the connector that embeds the slug into the
/// request path and token resource.
pub(super) fn validate_gateway_endpoint_slug(server_name: &str, slug: &str) -> Result<()> {
    crate::connectors::validate_mcp_endpoint_slug(slug).map_err(|_| {
        server_error(
            server_name,
            "gateway endpoint must be 1-127 ASCII letters, digits, '_' or '-'",
        )
    })
}

pub(super) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAX_SERVER_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(server_error(
            name,
            "name must contain only ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

pub(super) fn validate_process_string(name: &str, field: &str, value: &str) -> Result<()> {
    if value.len() > MAX_PROCESS_STRING_BYTES {
        return Err(server_error(
            name,
            format!("{field} exceeds {MAX_PROCESS_STRING_BYTES} bytes"),
        ));
    }
    if value.contains('\0') {
        return Err(server_error(name, format!("{field} must not contain NUL")));
    }
    Ok(())
}

pub(super) fn validate_environment_name(server_name: &str, name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAX_ENVIRONMENT_NAME_BYTES
        || name.contains('=')
        || name.contains('\0')
    {
        return Err(server_error(
            server_name,
            format!("invalid environment variable name {name:?}"),
        ));
    }
    Ok(())
}

/// The user-facing reason a server failed to connect.
///
/// Gateway mounts keep the sign-in / entitlement / reachability split: those
/// failures happen before or beside the wire, and the messages must not name
/// a resolved URL. Manual stdio and HTTP servers classify the transport
/// failure (DNS, TLS, HTTP status, protocol, timeout, missing environment,
/// process launch) and include the concrete detail. Messages interpolate
/// configured *names*, a DNS host, an HTTP status line, a TLS reason, a
/// bounded first-bytes quote, or a first stderr line — never a URL, token,
/// or unbounded upstream body.
pub(super) fn connection_diagnostic(
    definition: &McpServerDefinition,
    error: &AgentError,
) -> String {
    if definition.gateway_endpoint.is_some() {
        if crate::connectors::is_sign_in_required(error) {
            return "Sign in to the model gateway to reconnect this server.".to_string();
        }
        if matches!(error, AgentError::Config(_)) {
            return "Could not get access to this gateway endpoint. Check your \
                    entitlements for it."
                .to_string();
        }
        return "Could not connect to this gateway endpoint. Check that it is reachable \
                and allows this kind of access."
            .to_string();
    }
    if let Some(name) = missing_parent_environment(definition) {
        return missing_environment_diagnostic(
            name,
            definition.bearer_token_env.as_deref() == Some(name),
        );
    }
    let detail = classified_transport_detail(error);
    if let Some(detail) = detail {
        return detail;
    }
    if definition.url.is_some() {
        return "Could not connect to this server. Check its URL and credentials.".to_string();
    }
    "Could not initialize this server. Check its executable, arguments, and working directory."
        .to_string()
}

/// Why retrying a failed connection cannot help until something outside the
/// supervisor changes, if that is so.
///
/// A signed-out gateway mount fails the same way until the next sign-in, and
/// a server whose parent environment variable is missing fails the same way
/// until Tidebreak restarts. Every other failure may clear on its own, such as
/// a network outage or a server that is still starting, so it keeps retrying.
pub(super) fn reconnect_park(
    definition: &McpServerDefinition,
    error: &AgentError,
) -> Option<ReconnectPark> {
    if crate::connectors::is_sign_in_required(error) {
        return Some(if definition.gateway_endpoint.is_some() {
            ReconnectPark::SignIn
        } else {
            ReconnectPark::Authorization
        });
    }
    if definition.gateway_endpoint.is_none() && missing_parent_environment(definition).is_some() {
        return Some(ReconnectPark::Configuration);
    }
    // A value the credential store does not hold stays missing until someone
    // enters it.
    if definition.gateway_endpoint.is_none() && is_missing_stored_value(error) {
        return Some(ReconnectPark::Configuration);
    }
    None
}

/// Whether a connection failed because the credential store lacks a stored
/// bearer token or header value the definition declares.
fn is_missing_stored_value(error: &AgentError) -> bool {
    matches!(error, AgentError::Config(message) | AgentError::Message(message)
        if message.starts_with(NOT_STORED))
}

/// The diagnostic for a failed connection, given what it taught the runtime
/// about OAuth. A server that asks for a sign-in says so instead of reporting
/// the `401` it answered with.
pub(super) fn failure_diagnostic(
    definition: &McpServerDefinition,
    error: &AgentError,
    oauth: Option<&OAuthNeed>,
) -> String {
    match oauth {
        Some(need) => need.diagnostic(),
        None => connection_diagnostic(definition, error),
    }
}

/// [`reconnect_park`], given what the failure taught the runtime about OAuth.
/// A server that asks for a sign-in cannot connect until someone signs in or
/// changes it, so the supervisor stops retrying it. A sign-in service that
/// did not answer is temporary and keeps the usual backoff.
pub(super) fn failure_park(
    definition: &McpServerDefinition,
    error: &AgentError,
    oauth: Option<&OAuthNeed>,
) -> Option<ReconnectPark> {
    match oauth.and_then(OAuthNeed::park) {
        Some(park) => Some(park),
        None => reconnect_park(definition, error),
    }
}

/// The first parent environment variable the definition reads that this
/// process does not have.
fn missing_parent_environment(definition: &McpServerDefinition) -> Option<&str> {
    definition
        .env_from
        .iter()
        .chain(&definition.bearer_token_env)
        .find(|name| std::env::var_os(name).is_none())
        .map(String::as_str)
}

fn missing_environment_diagnostic(name: &str, bearer: bool) -> String {
    if bearer {
        format!(
            "Bearer-token environment variable {name:?} is not set in the environment \
             Tidebreak reads. Export it in the shell you start Tidebreak from, then \
             restart Tidebreak. Tidebreak does not read a .env file or this form for \
             the token value."
        )
    } else {
        format!(
            "Parent environment variable {name:?} is not set in the environment \
             Tidebreak reads. Export it in the shell you start Tidebreak from, then \
             restart Tidebreak."
        )
    }
}

fn classified_transport_detail(error: &AgentError) -> Option<String> {
    let raw = match error {
        AgentError::Config(message) | AgentError::Message(message) => message.as_str(),
        _ => return None,
    };
    let raw = raw.strip_prefix("MCP client error: ").unwrap_or(raw);
    const PREFIXES: &[&str] = &[
        "DNS resolution failed",
        "TLS handshake failed",
        "Authentication failed",
        "Wrong path",
        "Server error",
        "HTTP status",
        "Protocol negotiation failed",
        "Timed out after",
        "Process failed to launch",
        "Command not found:",
        "Not executable:",
        "Permission denied:",
        "Relative executable path",
        // A stored bearer token or header value this computer does not hold.
        NOT_STORED,
        // A token refresh the sign-in service did not answer: temporary, and
        // worded for the person by the OAuth connector.
        "Sign-in service unavailable",
    ];
    PREFIXES
        .iter()
        .any(|prefix| raw.starts_with(prefix))
        .then(|| raw.to_string())
}

pub(super) fn server_error(name: &str, message: impl std::fmt::Display) -> AgentError {
    AgentError::config(format!("invalid external MCP server {name:?}: {message}"))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    fn http_server(url: &str, bearer_token_env: Option<&str>) -> McpServerDefinition {
        McpServerDefinition {
            name: "docs".to_string(),
            command: None,
            args: Vec::new(),
            env: BTreeSet::new(),
            env_values: BTreeMap::new(),
            env_from: Vec::new(),
            cwd: None,
            url: Some(url.to_string()),
            bearer_token_env: bearer_token_env.map(str::to_string),
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

    #[test]
    fn remote_cleartext_http_rejects_environment_bearers_at_validation() {
        assert!(validate_servers(&[http_server("http://remote.example/mcp", None)]).is_ok());
        assert!(
            validate_servers(&[http_server("http://127.0.0.1:9000/mcp", Some("MCP_TOKEN"))])
                .is_ok()
        );

        let error =
            validate_servers(&[http_server("http://remote.example/mcp", Some("MCP_TOKEN"))])
                .expect_err("remote cleartext bearer configuration must be rejected");
        assert!(error.to_string().contains("must use https"), "{error}");
    }
}
