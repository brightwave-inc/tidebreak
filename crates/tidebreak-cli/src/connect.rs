//! Attach or embed — how a command reaches a server.
//!
//! Every client command (`-p` and the setup families) needs an
//! `tidebreak-server` to talk to, and there are two honest ways to get one. By
//! default the CLI **embeds**: it binds the server in-process over its own data
//! directory, which is the right shape for a script or an agent trying things
//! out in isolation. With `--server <url>` (or `TIDEBREAK_SERVER_URL`) it
//! **attaches** instead, becoming a pure HTTP+WS client of an already-running
//! `tidebreak serve` — the same client the desktop webview is. `--attach` is
//! the same attach, but the URL and token come from `{data_dir}/listen.json`
//! that the running server published (desktop or `serve`), so the token never
//! rides argv — see [`docs/decisions/0012-data-dir-listen-endpoint.md`].
//!
//! Attaching is what a second process on one data directory must do. A data
//! directory belongs to exactly one server process (`tidebreak-server` holds an
//! advisory lock on it for the life of the process), so pointing a second
//! embedding CLI at a directory the desktop or a running daemon already owns is
//! refused rather than allowed to race the database.
//!
//! The token never rides argv. It comes from `listen.json` under `--attach`,
//! from `TIDEBREAK_SERVER_TOKEN`, or from the variable `--server-token-env`
//! names — a command line is readable by every process on the machine and lands
//! in shell history, and a per-launch bearer token is full authority over the
//! profile.

use tidebreak_core::{AgentError, Result};

use crate::api::client::Client;

/// Names the server to attach to instead of embedding one.
pub const SERVER_URL_ENV: &str = "TIDEBREAK_SERVER_URL";
/// Holds the bearer token for that server, unless `--server-token-env` names
/// another variable.
pub const SERVER_TOKEN_ENV: &str = "TIDEBREAK_SERVER_TOKEN";

/// Where a command's server comes from.
pub enum Server {
    /// Bind one in-process over the configured data directory (the default).
    Embed,
    /// Talk to one that is already running.
    Attach {
        base: String,
        token: String,
        local_import_token: Option<String>,
    },
}

impl Server {
    /// Resolve the choice from `--attach` / `--server` / `--server-token-env`
    /// and the environment. `--server` wins over `TIDEBREAK_SERVER_URL`.
    /// `--attach` and `--server` together are a mistake.
    pub fn resolve(
        url_flag: Option<String>,
        token_env: Option<String>,
        attach: bool,
    ) -> Result<Self> {
        if attach {
            if url_flag.is_some()
                || std::env::var(SERVER_URL_ENV).is_ok_and(|v| !v.trim().is_empty())
            {
                return Err(AgentError::config(
                    "--attach reads {data_dir}/listen.json; do not also pass \
                     --server or set TIDEBREAK_SERVER_URL",
                ));
            }
            if token_env.is_some() {
                return Err(AgentError::config(
                    "--attach supplies the token from listen.json; \
                     --server-token-env is only for --server",
                ));
            }
            let config = crate::profile_config()?;
            let endpoint =
                tidebreak_server::listen_endpoint::ListenEndpoint::read(&config.data_dir)?;
            let base = base_url(&endpoint.base_url)?;
            return Ok(Self::Attach {
                base,
                token: endpoint.token,
                local_import_token: Some(endpoint.local_import_token),
            });
        }
        let url = match url_flag {
            Some(url) => Some(url),
            None => std::env::var(SERVER_URL_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty()),
        };
        let Some(url) = url else {
            if let Some(var) = token_env {
                return Err(AgentError::config(format!(
                    "--server-token-env {var} names a token for a server to attach to, \
                     but no --server <url> (or {SERVER_URL_ENV}) was given"
                )));
            }
            return Ok(Self::Embed);
        };
        let base = base_url(&url)?;
        let var = token_env.as_deref().unwrap_or(SERVER_TOKEN_ENV);
        let token = std::env::var(var)
            .ok()
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                AgentError::config(format!(
                    "{var} is not set; attaching to {base} needs the bearer token that \
                     server printed at startup (or use --attach to read listen.json)"
                ))
            })?;
        Ok(Self::Attach {
            base,
            token,
            local_import_token: None,
        })
    }
}

/// Normalize `--server` into the base every route is formatted against.
///
/// Deliberately strict about the scheme: the event socket is derived from this
/// string, and a bare `127.0.0.1:8080` would silently produce a URL nothing can
/// connect to.
fn base_url(value: &str) -> Result<String> {
    let value = value.trim();
    let rest = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .ok_or_else(|| {
            AgentError::config(format!(
                "--server expects an http:// or https:// URL, got {value:?}"
            ))
        })?;
    if rest.is_empty() || rest.starts_with('/') {
        return Err(AgentError::config(format!(
            "--server URL {value:?} has no host"
        )));
    }
    if rest.contains('?') || rest.contains('#') {
        return Err(AgentError::config(format!(
            "--server expects a base URL without a query or fragment, got {value:?}"
        )));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

/// A live client plus, when embedding, the engine keeping it answering.
///
/// Dropping the session aborts the embedded accept loop; dropping the `Server`
/// it owns aborts the background workers with it. Attach mode owns nothing —
/// the process on the other end keeps running when this one exits.
pub struct Session {
    client: Client,
    serve: Option<tokio::task::JoinHandle<Result<()>>>,
    client_executor_token: Option<String>,
}

impl Session {
    /// Bind or attach, whichever `server` asks for.
    pub async fn open(server: &Server) -> Result<Self> {
        match server {
            Server::Embed => {
                let config = crate::profile_config()?;
                // stdout belongs to the command's output, so logs are file-only.
                tidebreak_server::logging::init_logging_file_only(&config.data_dir);
                let server = tidebreak_server::bind_configured(config).await?;
                let client = Client::new(server.local_addr(), server.token())?;
                let client_executor_token = server.client_executor_token().to_owned();
                Ok(Self {
                    client,
                    serve: Some(tokio::spawn(server.serve())),
                    client_executor_token: Some(client_executor_token),
                })
            }
            // Nothing local is touched in attach mode beyond the optional
            // listen.json read that produced this choice: no log file, no
            // keychain. This process is only a client.
            Server::Attach {
                base,
                token,
                local_import_token,
            } => Ok(Self {
                client: Client::attach_with_local_import(
                    base.clone(),
                    token,
                    local_import_token.as_deref(),
                )?,
                serve: None,
                client_executor_token: None,
            }),
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The second per-launch credential, for the routes that execute a
    /// client-owned tool call — present only when this process is the server.
    ///
    /// Attaching deliberately gets nothing. That credential is the native-only
    /// boundary: it says "I am the trusted surface for this server", and a
    /// client that merely holds a bearer token is not, no matter which process
    /// started it. The bearer is all `--server` / `--attach` convey, and there
    /// is no flag to hand over the executor token — so an attached run cannot
    /// execute a client tool call on somebody else's server, which is the point.
    pub fn client_executor_token(&self) -> Option<&str> {
        self.client_executor_token.as_deref()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(serve) = &self.serve {
            serve.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parsing rules worth stating: a base URL is normalized, junk is
    /// refused before a request is built against it, and a token variable
    /// without a server is a mistake rather than a silent embed.
    #[test]
    fn a_server_url_is_normalized_or_refused() {
        assert_eq!(
            base_url("http://127.0.0.1:8080/").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            base_url(" https://box.local:9000 ").unwrap(),
            "https://box.local:9000"
        );
        assert!(
            base_url("127.0.0.1:8080").is_err(),
            "the scheme is required"
        );
        assert!(base_url("http://").is_err(), "a host is required");
        assert!(base_url("http://host/?a=1").is_err(), "no query string");

        std::env::remove_var(SERVER_URL_ENV);
        let Err(error) = Server::resolve(None, Some("SOME_VAR".to_owned()), false) else {
            panic!("a token variable alone is not enough to attach");
        };
        assert!(
            error.to_string().contains("--server"),
            "error should name the missing flag: {error}"
        );
    }

    #[test]
    fn attach_flag_conflicts_with_server_url() {
        let Err(error) = Server::resolve(Some("http://127.0.0.1:1".into()), None, true) else {
            panic!("--attach and --server together must fail");
        };
        assert!(
            error.to_string().contains("--attach"),
            "error should name the conflict: {error}"
        );
    }
}
