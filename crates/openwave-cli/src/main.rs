//! OpenWave CLI — the headless daemon and MCP server.
//!
//! `openwave serve` boots the in-process HTTP/WebSocket surface on a loopback
//! port and prints the address and per-launch bearer token it minted, so a local
//! client (the desktop shell, or a script) can connect. Configuration comes from
//! the environment via [`Config::from_env`] (`OPENWAVE_PROFILE`,
//! `OPENWAVE_DATA_DIR`, `OPENWAVE_CONTAINER_EXECUTION_ENABLED`, and
//! `OPENWAVE_CONTAINER_IMAGE`); the model API key comes from
//! `ANTHROPIC_API_KEY`, and `OPENWAVE_MCP_CONFIG` may name an external
//! stdio-server configuration file.
//!
//! `openwave mcp <workspace>` serves the built-in read-only filesystem tools over
//! MCP stdio, confined to the explicit workspace directory.
//!
//! `openwave rehome-secrets` rewrites the profile's stored credentials so their
//! keychain items belong to the running binary's code signature, which is what
//! stops macOS asking for credentials an earlier build created.
//!
//! `openwave tui [--chat <id> | --new]` runs an interactive terminal chat: it
//! boots the same in-process server as `serve` and drives it over the loopback
//! HTTP+WS API. With neither flag it opens a picker over the existing chats, so
//! a session can be resumed without knowing its id; `--new` skips straight to a
//! fresh chat, and `--chat` resumes one by id.
//!
//! `openwave -p "<prompt>"` runs one turn without a terminal: same engine, no
//! terminal. stdout carries the assistant's text (or, with
//! `--output-format json`, the turn's event stream as NDJSON), and the exit
//! status says whether the turn completed. `--permission-mode` sets the chat's
//! permission mode for the run, and under `--output-format json` a driving
//! process answers approvals, plans, and questions over stdin — see
//! [`print::protocol`].
//!
//! `openwave provider|model|settings|mcp-server …` configure the profile the
//! same way the desktop's settings pages do — over the server's own routes.
//! See [`setup`]; secrets are read from stdin or a named environment variable,
//! never from a command-line argument.
//!
//! Every client command above embeds its own server by default. `--server
//! <url>` (or `OPENWAVE_SERVER_URL`) makes it a pure client of one that is
//! already running instead, with the bearer token coming from
//! `OPENWAVE_SERVER_TOKEN` — see [`connect`]. That is how a second process
//! reaches a data directory a desktop app or daemon already owns; two processes
//! embedding servers over one data directory is refused.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use openwave_core::{AgentError, ChatId, Config, ListDir, ReadFile, Result, ToolCtx, ToolRegistry};

mod api;
mod connect;
mod print;
mod setup;
mod tui;

use print::OutputFormat;
use setup::{Command as SetupCommand, SecretSource};

const USAGE: &str = "\
usage: openwave serve
       openwave mcp <workspace>
       openwave rehome-secrets
       openwave tui [--chat <id> | --new]
       openwave -p <prompt> [--chat <id>] [--output-format text|json]
                  [--permission-mode ask|auto|allow|plan]

       openwave provider list
       openwave provider set-key <kind> [--from-env <var>]
       openwave provider remove-key <kind>
       openwave model list
       openwave model roles
       openwave model select <key|auto> [--role <role>]
       openwave settings show
       openwave settings web-search select <provider|off>
       openwave settings web-search set-key <provider> [--from-env <var>]
       openwave settings web-search remove-key <provider>
       openwave settings exec select <provider|off>
       openwave settings exec set-key <provider> [--from-env <var>]
       openwave settings exec remove-key <provider>
       openwave mcp-server list
       openwave mcp-server add <name> (--command <cmd> [--arg <a>]… | --url <url>)
                  [--env-from <var>]… [--cwd <dir>] [--bearer-token-env <var>]
                  [--timeout-ms <ms>] [--disabled]
       openwave mcp-server remove <name>

The setup commands take --output-format text|json. A key is read from stdin, or
from the environment variable named by --from-env — never from an argument,
which every process on the machine can read.

tui, -p, and the setup commands also take --server <url> [--server-token-env
<var>], which talks to a server that is already running instead of embedding
one. The token comes from OPENWAVE_SERVER_TOKEN, or from the named variable;
it is never an argument either.";

#[tokio::main]
async fn main() {
    match run().await {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            // Print the error's `Display` (e.g. "configuration error: …"), not
            // the `Debug` form the stdlib `Termination` impl would show.
            eprintln!("openwave: {error}");
            std::process::exit(1);
        }
    }
}

/// Dispatch one command, returning the process exit status. Only print mode
/// reports anything but `0`, since only it distinguishes a failure of the
/// command from a failure of the work the command drove.
async fn run() -> Result<i32> {
    let (args, server_flags) = take_server_flags(std::env::args_os().skip(1).collect());
    let mut args = args.into_iter();
    match args.next().as_deref() {
        // Default to `serve` so a bare `openwave` runs the daemon.
        None => {
            server_flags.refuse("serve");
            serve().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("serve") => {
            server_flags.refuse("serve");
            if args.next().is_some() {
                usage_error("serve does not accept arguments");
            }
            serve().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("mcp") => {
            server_flags.refuse("mcp");
            let Some(workspace) = args.next() else {
                usage_error("mcp requires a workspace path");
            };
            if args.next().is_some() {
                usage_error("mcp accepts exactly one workspace path");
            }
            serve_mcp(workspace.into()).await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("rehome-secrets") => {
            server_flags.refuse("rehome-secrets");
            if args.next().is_some() {
                usage_error("rehome-secrets does not accept arguments");
            }
            rehome_secrets().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("tui") => {
            let mut open = None;
            while let Some(flag) = args.next() {
                if flag == OsStr::new("--chat") {
                    let Some(id) = args.next() else {
                        usage_error("tui --chat requires a chat id");
                    };
                    let Ok(chat) = ChatId::from_str(&id.to_string_lossy()) else {
                        usage_error("tui --chat expects a chat UUID");
                    };
                    if open.is_some() {
                        usage_error("tui takes either --chat <id> or --new, not both");
                    }
                    open = Some(tui::Open::Chat(chat));
                } else if flag == OsStr::new("--new") {
                    if open.is_some() {
                        usage_error("tui takes either --chat <id> or --new, not both");
                    }
                    open = Some(tui::Open::New);
                } else {
                    usage_error("tui accepts only --chat <id> or --new");
                }
            }
            tui::run(open.unwrap_or(tui::Open::Pick), server_flags.resolve()?)
                .await
                .map(|()| 0)
        }
        Some(command) if command == OsStr::new("-p") || command == OsStr::new("--print") => {
            let Some(prompt) = args.next() else {
                usage_error("-p requires a prompt");
            };
            let Some(prompt) = prompt.to_str().map(str::to_owned) else {
                usage_error("-p expects a UTF-8 prompt");
            };
            let mut chat = None;
            let mut format = OutputFormat::Text;
            let mut permission_mode = None;
            while let Some(flag) = args.next() {
                if flag == OsStr::new("--chat") {
                    let Some(id) = args.next() else {
                        usage_error("--chat requires a chat id");
                    };
                    match ChatId::from_str(&id.to_string_lossy()) {
                        Ok(id) => chat = Some(id),
                        Err(_) => usage_error("--chat expects a chat UUID"),
                    }
                } else if flag == OsStr::new("--output-format") {
                    let Some(value) = args.next() else {
                        usage_error("--output-format requires text or json");
                    };
                    match OutputFormat::parse(&value.to_string_lossy()) {
                        Some(value) => format = value,
                        None => usage_error("--output-format expects text or json"),
                    }
                } else if flag == OsStr::new("--permission-mode") {
                    let Some(value) = args.next() else {
                        usage_error("--permission-mode requires ask, auto, allow, or plan");
                    };
                    // The wire tokens are the chat's own permission-mode
                    // vocabulary; the CLI adds nothing to it.
                    match value.to_string_lossy().as_ref() {
                        mode @ ("ask" | "auto" | "allow" | "plan") => {
                            permission_mode = Some(mode.to_owned());
                        }
                        _ => usage_error("--permission-mode expects ask, auto, allow, or plan"),
                    }
                } else {
                    usage_error(&format!("unknown print-mode argument {flag:?}"));
                }
            }
            print::run(
                prompt,
                chat,
                format,
                permission_mode,
                server_flags.resolve()?,
            )
            .await
        }
        Some(command)
            if command == OsStr::new("provider")
                || command == OsStr::new("model")
                || command == OsStr::new("settings")
                || command == OsStr::new("mcp-server") =>
        {
            let family = command.to_string_lossy().into_owned();
            let (command, format) = parse_setup(&family, text_args(args));
            setup::run(command, format, server_flags.resolve()?)
                .await
                .map(|()| 0)
        }
        Some(other) => {
            usage_error(&format!("unknown command {other:?}"));
        }
    }
}

/// The `--server` / `--server-token-env` pair, lifted out of the arguments.
struct ServerFlags {
    url: Option<String>,
    token_env: Option<String>,
}

impl ServerFlags {
    /// Turn the flags plus the environment into the choice to embed or attach.
    fn resolve(self) -> Result<connect::Server> {
        connect::Server::resolve(self.url, self.token_env)
    }

    /// Refuse the flags on a command that has no client to point elsewhere.
    ///
    /// Only the explicit flag is an error. `OPENWAVE_SERVER_URL` is ambient —
    /// a shell that exports it so its `-p` runs attach must still be able to
    /// start a daemon.
    fn refuse(&self, command: &str) {
        if self.url.is_some() || self.token_env.is_some() {
            usage_error(&format!(
                "{command} runs a server rather than connecting to one, so it takes no --server"
            ));
        }
    }
}

/// Pull `--server <url>` and `--server-token-env <var>` out of the arguments
/// wherever they appear, leaving the rest for the per-command parsers.
///
/// A pre-pass rather than an option on each parser: the flags apply to every
/// client command, and no command takes a value beginning with `--` (each
/// parser rejects one), so nothing else can legitimately be spelled this way.
fn take_server_flags(args: Vec<OsString>) -> (Vec<OsString>, ServerFlags) {
    let mut flags = ServerFlags {
        url: None,
        token_env: None,
    };
    let mut rest = Vec::with_capacity(args.len());
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let slot = if arg == OsStr::new("--server") {
            &mut flags.url
        } else if arg == OsStr::new("--server-token-env") {
            &mut flags.token_env
        } else {
            rest.push(arg);
            continue;
        };
        let name = arg.to_string_lossy().into_owned();
        match args.next() {
            Some(value) => match value.into_string() {
                Ok(value) if !value.starts_with("--") => *slot = Some(value),
                _ => usage_error(&format!("{name} requires a value")),
            },
            None => usage_error(&format!("{name} requires a value")),
        }
    }
    (rest, flags)
}

/// The remaining arguments as UTF-8. Setup arguments are provider names, model
/// keys, URLs, and variable names; a path that is not valid UTF-8 is refused
/// here rather than silently mangled.
fn text_args(args: impl Iterator<Item = OsString>) -> Vec<String> {
    args.map(|arg| {
        arg.into_string()
            .unwrap_or_else(|arg| usage_error(&format!("{arg:?} is not valid UTF-8")))
    })
    .collect()
}

/// A cursor over one setup subcommand's arguments.
///
/// The setup families share a shape — a verb, positional names, then flags —
/// so they share a reader rather than each re-deriving "the next argument, or
/// a usage error naming the flag that wanted it".
struct Cursor {
    args: Vec<String>,
    at: usize,
}

impl Cursor {
    fn new(args: Vec<String>) -> Self {
        Self { args, at: 0 }
    }

    fn next(&mut self) -> Option<String> {
        let value = self.args.get(self.at).cloned();
        if value.is_some() {
            self.at += 1;
        }
        value
    }

    /// The next argument, which must be there and must not be another flag.
    fn value(&mut self, flag: &str) -> String {
        match self.next() {
            Some(value) if !value.starts_with("--") => value,
            _ => usage_error(&format!("{flag} requires a value")),
        }
    }

    /// The next positional argument, named for the error message.
    fn positional(&mut self, what: &str) -> String {
        match self.next() {
            Some(value) if !value.starts_with("--") => value,
            _ => usage_error(&format!("expected {what}")),
        }
    }
}

/// Parse one `provider`/`model`/`settings`/`mcp-server` invocation.
fn parse_setup(family: &str, args: Vec<String>) -> (SetupCommand, OutputFormat) {
    let mut cursor = Cursor::new(args);
    let verb = cursor.positional(&format!("a {family} subcommand"));
    let command = match (family, verb.as_str()) {
        ("provider", "list") => SetupCommand::ProviderList,
        ("provider", "set-key") => SetupCommand::ProviderSetKey {
            kind: cursor.positional("a provider kind"),
            secret: parse_secret_source(&mut cursor),
        },
        ("provider", "remove-key") => SetupCommand::ProviderRemoveKey {
            kind: cursor.positional("a provider kind"),
        },
        ("model", "list") => SetupCommand::ModelList,
        ("model", "roles") => SetupCommand::ModelRoles,
        ("model", "select") => {
            let selection = cursor.positional("a model key, or `auto`");
            let mut role = "chat".to_owned();
            let mut format = None;
            while let Some(flag) = cursor.next() {
                match flag.as_str() {
                    "--role" => role = cursor.value("--role"),
                    "--output-format" => format = Some(parse_format(cursor.value(flag.as_str()))),
                    other => usage_error(&format!("unknown model select argument {other:?}")),
                }
            }
            return (
                SetupCommand::ModelSelect {
                    role,
                    selection: (selection != "auto").then_some(selection),
                },
                format.unwrap_or(OutputFormat::Text),
            );
        }
        ("settings", "show") => SetupCommand::SettingsShow,
        ("settings", "web-search") => match cursor.positional("a web-search subcommand").as_str() {
            "select" => SetupCommand::WebSearchSelect {
                provider: parse_selection(cursor.positional("a web-search provider, or `off`")),
            },
            "set-key" => SetupCommand::WebSearchSetKey {
                provider: cursor.positional("a web-search provider"),
                secret: parse_secret_source(&mut cursor),
            },
            "remove-key" => SetupCommand::WebSearchRemoveKey {
                provider: cursor.positional("a web-search provider"),
            },
            other => usage_error(&format!("unknown settings web-search subcommand {other:?}")),
        },
        ("settings", "exec") => match cursor.positional("an exec subcommand").as_str() {
            "select" => SetupCommand::ExecSelect {
                provider: parse_selection(cursor.positional("an execution provider, or `off`")),
            },
            "set-key" => SetupCommand::ExecSetKey {
                provider: cursor.positional("an execution provider"),
                secret: parse_secret_source(&mut cursor),
            },
            "remove-key" => SetupCommand::ExecRemoveKey {
                provider: cursor.positional("an execution provider"),
            },
            other => usage_error(&format!("unknown settings exec subcommand {other:?}")),
        },
        ("mcp-server", "list") => SetupCommand::McpList,
        ("mcp-server", "add") => SetupCommand::McpAdd {
            definition: parse_mcp_definition(&mut cursor),
        },
        ("mcp-server", "remove") => SetupCommand::McpRemove {
            name: cursor.positional("an MCP server name"),
        },
        (family, other) => usage_error(&format!("unknown {family} subcommand {other:?}")),
    };
    let format = parse_trailing_format(&mut cursor, &format!("{family} {verb}"));
    (command, format)
}

/// `--from-env <var>` names an environment variable holding the secret;
/// without it the secret is read from stdin. A value never rides argv.
fn parse_secret_source(cursor: &mut Cursor) -> SecretSource {
    match cursor.next().as_deref() {
        None => SecretSource::Stdin,
        Some("--from-env") => SecretSource::Env(cursor.value("--from-env")),
        Some("--output-format") => {
            // Put it back for the trailing-flag pass.
            cursor.at -= 1;
            SecretSource::Stdin
        }
        Some(other) => usage_error(&format!(
            "unknown argument {other:?}; a key is read from stdin or --from-env <var>"
        )),
    }
}

/// A positional selection where `off` (or `none`) clears the setting.
fn parse_selection(value: String) -> Option<String> {
    (value != "off" && value != "none").then_some(value)
}

/// The trailing `--output-format`, the only flag every setup command shares.
fn parse_trailing_format(cursor: &mut Cursor, context: &str) -> OutputFormat {
    let mut format = OutputFormat::Text;
    while let Some(flag) = cursor.next() {
        match flag.as_str() {
            "--output-format" => format = parse_format(cursor.value("--output-format")),
            other => usage_error(&format!("unexpected argument {other:?} after {context}")),
        }
    }
    format
}

fn parse_format(value: String) -> OutputFormat {
    match OutputFormat::parse(&value) {
        Some(format) => format,
        None => usage_error("--output-format expects text or json"),
    }
}

/// Build one MCP server definition from flags, in the shape
/// `PUT /mcp/servers` takes. Values the server keeps out of its definitions —
/// environment values, bearer tokens — are named here, never given.
fn parse_mcp_definition(cursor: &mut Cursor) -> serde_json::Value {
    let name = cursor.positional("an MCP server name");
    let mut definition = serde_json::json!({ "name": name });
    let mut args: Vec<String> = Vec::new();
    let mut env_from: Vec<String> = Vec::new();
    let mut transports = 0;
    while let Some(flag) = cursor.next() {
        match flag.as_str() {
            "--command" => {
                transports += 1;
                definition["command"] = cursor.value("--command").into();
            }
            "--arg" => args.push(cursor.value("--arg")),
            "--env-from" => env_from.push(cursor.value("--env-from")),
            "--cwd" => definition["cwd"] = cursor.value("--cwd").into(),
            "--url" => {
                transports += 1;
                definition["url"] = cursor.value("--url").into();
            }
            "--bearer-token-env" => {
                definition["bearer_token_env"] = cursor.value("--bearer-token-env").into();
            }
            "--gateway-endpoint" => {
                transports += 1;
                definition["gateway_endpoint"] = cursor.value("--gateway-endpoint").into();
            }
            "--timeout-ms" => {
                let value = cursor.value("--timeout-ms");
                let Ok(timeout) = value.parse::<u64>() else {
                    usage_error("--timeout-ms expects a whole number of milliseconds");
                };
                definition["request_timeout_ms"] = timeout.into();
            }
            "--disabled" => definition["enabled"] = false.into(),
            "--output-format" => {
                // Belongs to the trailing pass; hand it back.
                cursor.at -= 1;
                break;
            }
            other => usage_error(&format!("unknown mcp-server add argument {other:?}")),
        }
    }
    if transports != 1 {
        usage_error("mcp-server add takes exactly one of --command, --url, or --gateway-endpoint");
    }
    if !args.is_empty() {
        definition["args"] = args.into();
    }
    if !env_from.is_empty() {
        definition["env_from"] = env_from.into();
    }
    definition
}

fn usage_error(message: &str) -> ! {
    eprintln!("openwave: {message}\n\n{USAGE}");
    std::process::exit(2);
}

/// Configuration for the profile this build talks to.
///
/// Debug builds keep their own keychain service, matching the desktop's
/// dev/release split: a dev daemon must not mutate release secret state.
fn profile_config() -> Result<Config> {
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut config = Config::from_env()?;
    #[cfg(debug_assertions)]
    {
        config.keychain_service = Some("openwave.dev".into());
    }
    Ok(config)
}

/// Bind the server and run its accept loop, announcing where to reach it.
async fn serve() -> Result<()> {
    let config = profile_config()?;
    // Tracing events land in `logs/openwave.log` under the profile data dir
    // (plus stderr in debug builds); see `openwave_server::logging`.
    openwave_server::logging::init_logging(&config.data_dir);
    let server = openwave_server::bind_configured(config).await?;
    // The address and token are the client's entry point: the parent process that
    // launched the daemon reads them from stdout to connect. The token is a secret,
    // so an integrator should capture this process's stdout directly (a piped
    // child) rather than run the daemon under a logging supervisor.
    println!("openwave: listening on http://{}", server.local_addr());
    println!("openwave: token {}", server.token());
    server.serve().await
}

/// Rewrite each stored credential so the item belongs to this binary's code
/// signature.
///
/// macOS keeps prompting for credentials an earlier, differently signed build
/// created — see [`openwave_server::secret_rehome`] for why an approval given at
/// that prompt does not survive the next rebuild. Run this through Cargo
/// (`cargo run -p openwave-cli -- rehome-secrets`) so the dev signing runner
/// applies; each credential asks for access once more, and then stops asking.
async fn rehome_secrets() -> Result<()> {
    use openwave_server::secret_rehome::RehomeOutcome;

    let config = profile_config()?;
    let mut touched = 0usize;
    let mut lost = 0usize;
    for (key, outcome) in openwave_server::rehome_configured_secrets(&config).await? {
        match outcome {
            RehomeOutcome::Absent => {}
            RehomeOutcome::Rehomed => {
                touched += 1;
                println!("openwave: re-homed {key}");
            }
            RehomeOutcome::Skipped(reason) => {
                touched += 1;
                eprintln!("openwave: left {key} as it was — {reason}");
            }
            RehomeOutcome::Lost(reason) => {
                touched += 1;
                lost += 1;
                eprintln!("openwave: lost {key} — {reason}; store this credential again");
            }
        }
    }
    if touched == 0 {
        println!("openwave: no stored credentials to re-home");
    }
    if lost > 0 {
        return Err(AgentError::msg(format!(
            "{lost} credential(s) were removed but could not be stored again"
        )));
    }
    Ok(())
}

/// Serve the built-in read-only filesystem tools over MCP stdio.
async fn serve_mcp(workspace: PathBuf) -> Result<()> {
    let ctx = ToolCtx::try_new_legacy_workspace(ChatId::new(), None, workspace.clone()).map_err(
        |error| {
            AgentError::config(format!(
                "could not open MCP workspace {}: {error}",
                workspace.display()
            ))
        },
    )?;

    let tools = Arc::new(
        ToolRegistry::new()
            .with(Box::new(ReadFile))
            .with(Box::new(ListDir)),
    );
    let server = openwave_mcp::McpServer::new(tools, ctx);
    openwave_mcp::serve_stdio(server)
        .await
        .map_err(|error| AgentError::msg(format!("MCP stdio error: {error}")))
}
