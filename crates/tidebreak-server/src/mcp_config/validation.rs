//! Validation for MCP server definitions.

use std::collections::HashSet;

use tidebreak_core::{AgentError, Result};
use tidebreak_mcp::MAX_SERVER_NAME_BYTES;

use super::types::*;

pub(super) fn validate_servers(servers: &[McpServerDefinition]) -> Result<()> {
    if servers.len() > MAX_SERVERS {
        return Err(AgentError::config(format!(
            "MCP config contains more than {MAX_SERVERS} servers"
        )));
    }
    let mut names = HashSet::new();
    for server in servers {
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
            }
            (None, Some(url), None) => {
                validate_process_string(&server.name, "url", url)?;
                tidebreak_mcp::validate_http_url_with_credentials(
                    url,
                    server.bearer_token_env.is_some(),
                )
                .map_err(|error| server_error(&server.name, error))?;
                validate_no_process_fields(server)?;
                if let Some(bearer_name) = &server.bearer_token_env {
                    validate_environment_name(&server.name, bearer_name)?;
                }
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
            }
            _ => {
                return Err(server_error(
                    &server.name,
                    "must configure exactly one of command, url, or gateway endpoint",
                ));
            }
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
                format!(
                    "must not contain more than {MAX_ENVIRONMENT_VARIABLES} environment variables"
                ),
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
        if !names.insert(server.name.clone()) {
            return Err(server_error(&server.name, "server name is duplicated"));
        }
    }
    Ok(())
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
/// Every returned string is fixed or interpolates a configured *name* only —
/// never URL, token, or upstream error text. For gateway mounts the split
/// follows where the failure happened: sign-in state, then resolution/token
/// exchange (these fail as [`AgentError::Config`] inside the connectors and
/// gateway runtime, before any wire I/O), then the wire itself (tidebreak-mcp
/// failures arrive as non-`Config` classes).
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
    if let Some(name) = definition
        .env_from
        .iter()
        .chain(&definition.bearer_token_env)
        .find(|name| std::env::var_os(name).is_none())
    {
        return format!("Required parent environment variable {name:?} is not set.");
    }
    if definition.url.is_some() {
        return "Could not connect to this server. Check its URL and credentials.".to_string();
    }
    "Could not initialize this server. Check its executable, arguments, and working directory."
        .to_string()
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
