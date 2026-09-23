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
//!
//! Before an attached command does anything else, it reads the server's
//! `GET /version` and compares the API level with the range this build reads.
//! A server outside that range is refused with a sentence that says which side
//! to update, rather than with a decode error halfway through the command. A
//! server that predates the route says nothing, and is attached as before. A
//! server that does not answer within [`VERSION_CHECK_TIMEOUT`] is reported as
//! unresponsive, so the check never adds its wait to the command's own.

use std::path::PathBuf;
use tidebreak_core::{AgentError, Result};

use crate::api::client::{validated_server_base_url, Client};
use crate::api::wire::{compatibility, Compatibility};

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
        /// Data directory whose `listen.json` should be re-read after a
        /// dropped connection. Explicit `--server` attachments leave this
        /// unset because their endpoint is fixed by the caller.
        listen_data_dir: Option<PathBuf>,
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
                listen_data_dir: Some(config.data_dir),
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
            listen_data_dir: None,
        })
    }
}

/// Normalize `--server` into the base every route is formatted against.
///
/// Deliberately strict about the scheme: the event socket is derived from this
/// string, and a bare `127.0.0.1:8080` would silently produce a URL nothing can
/// connect to.
fn base_url(value: &str) -> Result<String> {
    validated_server_base_url(value)
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
                listen_data_dir,
            } => {
                let client = Client::attach_with_reconnect_source(
                    base.clone(),
                    token,
                    local_import_token.as_deref(),
                    listen_data_dir.clone(),
                )?;
                require_compatible_server(&client).await?;
                Ok(Self {
                    client,
                    serve: None,
                    client_executor_token: None,
                })
            }
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

/// How long an attach waits for the version check before it reports the
/// server as unresponsive.
const VERSION_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Refuse a server whose API level this build does not read.
///
/// Only a definite answer refuses on version grounds. A server that says
/// nothing about its version, or refuses the request, passes here and meets
/// the command's own first request instead.
async fn require_compatible_server(client: &Client) -> Result<()> {
    require_compatible_server_within(client, VERSION_CHECK_TIMEOUT).await
}

async fn require_compatible_server_within(
    client: &Client,
    timeout: std::time::Duration,
) -> Result<()> {
    let answer = client.server_version(timeout).await?;
    match version_refusal(&compatibility(answer.as_ref())) {
        Some(refusal) => Err(AgentError::msg(refusal)),
        None => Ok(()),
    }
}

/// What a person reads when this build and the server disagree about the API
/// level, or `None` when they agree.
///
/// The server's release is the one number both sides know. A client at that
/// release or later reads the server's level, so naming it is always a safe
/// thing to ask for.
fn version_refusal(compatibility: &Compatibility) -> Option<String> {
    match compatibility {
        Compatibility::Compatible => None,
        Compatibility::ClientTooOld { server_version } => Some(format!(
            "This server runs Tidebreak {server_version}. Update Tidebreak to \
             {server_version} or later to connect."
        )),
        Compatibility::ServerTooOld { server_version } => Some(format!(
            "This server runs Tidebreak {server_version}, which this version of \
             Tidebreak no longer supports. Update the server to connect."
        )),
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
            base_url("http://127.0.0.2:8080/").unwrap(),
            "http://127.0.0.2:8080"
        );
        assert_eq!(
            base_url("http://localhost:8080/").unwrap(),
            "http://localhost:8080"
        );
        assert_eq!(base_url("http://[::1]:8080/").unwrap(), "http://[::1]:8080");
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
        assert!(base_url("http://box.local:9000")
            .expect_err("remote cleartext must be refused")
            .to_string()
            .contains("https"));
        assert!(
            base_url("https://user:password@box.local:9000").is_err(),
            "credentials belong in the token environment variable"
        );

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

    /// Serve `response` to every request on a loopback port, and return the
    /// attach choice that reaches it.
    async fn serve(response: String) -> Server {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let response = response.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        Server::Attach {
            base,
            token: "token".to_owned(),
            local_import_token: None,
            listen_data_dir: None,
        }
    }

    fn answer(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A server a level ahead is refused before the command runs, with the
    /// sentence that says what to update.
    #[tokio::test]
    async fn attaching_to_a_newer_server_says_to_update() {
        let newer = tidebreak_server::wire::API_LEVEL + 1;
        let server = serve(answer(
            "200 OK",
            &format!(r#"{{"version":"9.4.0","api_level":{newer}}}"#),
        ))
        .await;
        let Err(error) = Session::open(&server).await else {
            panic!("a server this build cannot read must be refused");
        };
        assert_eq!(
            error.to_string(),
            "This server runs Tidebreak 9.4.0. Update Tidebreak to 9.4.0 or later to connect."
        );
    }

    /// Today's servers answer `/version` with `404`, and attaching to one
    /// works exactly as it did before the check existed.
    #[tokio::test]
    async fn a_server_without_the_version_route_still_attaches() {
        for response in [
            answer(
                "404 Not Found",
                r#"{"kind":"not_found","message":"no route"}"#,
            ),
            answer("200 OK", "<!doctype html>"),
        ] {
            let server = serve(response).await;
            assert!(Session::open(&server).await.is_ok());
        }
        let current = serde_json::to_string(&tidebreak_server::wire::ServerVersion::current())
            .expect("the version serializes");
        let server = serve(answer("200 OK", &current)).await;
        assert!(Session::open(&server).await.is_ok());
    }

    /// A release the terminal could not print is no answer, so the attach
    /// goes ahead and nothing the server wrote there reaches stderr.
    #[tokio::test]
    async fn an_unprintable_release_is_treated_as_no_answer() {
        let newer = tidebreak_server::wire::API_LEVEL + 1;
        for version in ["\u{1b}[1;31m9.4.0", "9.4.0 from https://evil.example"] {
            let body = serde_json::json!({ "version": version, "api_level": newer }).to_string();
            let server = serve(answer("200 OK", &body)).await;
            assert!(Session::open(&server).await.is_ok(), "{version:?}");
        }
    }

    /// An unresponsive server costs the check's own wait and no more: the
    /// attach ends there, instead of going on to a request with no limit.
    #[tokio::test]
    async fn an_unresponsive_server_is_reported_after_one_wait() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                open.push(stream);
            }
        });
        let client = Client::attach(base.clone(), "token").unwrap();
        let started = std::time::Instant::now();
        let error =
            require_compatible_server_within(&client, std::time::Duration::from_millis(300))
                .await
                .expect_err("a server that never answers is reported");
        assert!(
            error
                .to_string()
                .contains(&format!("{base} did not answer")),
            "{error}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        held.abort();
    }

    #[test]
    fn an_older_server_is_named_as_the_side_to_update() {
        assert_eq!(
            version_refusal(&Compatibility::ServerTooOld {
                server_version: "0.9.0".to_owned()
            })
            .as_deref(),
            Some(
                "This server runs Tidebreak 0.9.0, which this version of Tidebreak no longer \
                 supports. Update the server to connect."
            )
        );
        assert_eq!(version_refusal(&Compatibility::Compatible), None);
    }
}
