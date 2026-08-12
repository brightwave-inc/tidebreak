//! OpenWave CLI — the headless daemon and MCP server.
//!
//! `openwave serve` boots the in-process HTTP/WebSocket surface and prints the
//! address it bound, so a local client (the desktop shell, or a script) can
//! connect. It binds a loopback, ephemeral port by default, and prints the
//! per-launch bearer token it minted alongside the address — except on the
//! self-host profile, where that token authenticates nobody and the address
//! may be set with `OPENWAVE_LISTEN_ADDR`. Configuration comes from
//! the environment via [`Config::from_env`] (`OPENWAVE_PROFILE`,
//! `OPENWAVE_DATA_DIR`, `OPENWAVE_CONTAINER_EXECUTION_ENABLED`,
//! `OPENWAVE_CONTAINER_IMAGE`, and `OPENWAVE_LISTEN_ADDR`); the model API key
//! comes from `ANTHROPIC_API_KEY`, and `OPENWAVE_MCP_CONFIG` may name an
//! external stdio-server configuration file.
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
//! `openwave output list|show|revisions|export <chat> …` reads a conversation's
//! outputs and writes one to a path, and `openwave attach <chat> <file>` puts a
//! local file into a conversation. Both drive the same server routes the desktop
//! does.
//!
//! `openwave -p "<prompt>"` runs one turn without a terminal: same engine, no
//! terminal. stdout carries the assistant's text (or, with
//! `--output-format json`, the turn's event stream as NDJSON), and the exit
//! status says whether the turn completed. `--permission-mode` sets the chat's
//! permission mode for the run, `--model` pins the chat's model selection
//! before the turn, and under `--output-format json` a driving process answers
//! approvals, plans, and questions over stdin — see [`print::protocol`].
//!
//! `openwave provider|model|settings|mcp-server …` configure the profile the
//! same way the desktop's settings pages do — over the server's own routes.
//! See [`setup`]; secrets are read from stdin or a named environment variable,
//! never from a command-line argument.
//!
//! `openwave folder connect|list|disconnect` is the headless equivalent of the
//! desktop's folder picker: an operator records standing consent for a host
//! folder, which the broker stamps as operator configuration. It is deliberate
//! provisioning only — nothing here answers a folder request an agent made
//! during a turn. See [`folder`]. Unlike the client commands it works on local
//! broker state and the local product store (not via `--server`/`--attach`): it
//! opens them beside a running `serve`/desktop rather than embedding a second
//! server, so a live profile can be provisioned without stopping the daemon.
//!
//! Once a folder is connected, the tools that read it are executed by whichever
//! process owns that broker state — `serve`, or the engine `-p` embeds. See
//! [`folder_executor`].
//!
//! Every client command above embeds its own server by default. `--server
//! <url>` (or `OPENWAVE_SERVER_URL`) makes it a pure client of one that is
//! already running instead, with the bearer token coming from
//! `OPENWAVE_SERVER_TOKEN` — see [`connect`]. `--attach` is the same attach
//! using `{OPENWAVE_DATA_DIR}/listen.json` the running server wrote, so the
//! token never rides argv (desktop or `serve`). That is how a second process
//! reaches a data directory a desktop app or daemon already owns; two processes
//! embedding servers over one data directory is refused.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use openwave_core::{
    AgentError, ChatId, Config, ListDir, Profile, ReadFile, Result, ToolCtx, ToolRegistry, TurnId,
};

mod api;
mod connect;
mod folder;
mod folder_executor;
mod outputs;
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
                  [--model <key>]

       openwave output list <chat> [--output-format text|json]
       openwave output show <chat> <output> [--revision <id>] [--output-format text|json]
       openwave output revisions <chat> <output> [--output-format text|json]
       openwave output export <chat> <output> <path> [--revision <id>] [--output-format text|json]
       openwave attach <chat> <file>

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
       openwave chat list
       openwave chat create
       openwave chat delete <chat>
       openwave chat steer <chat> <turn> <text...>
       openwave agent-run list <chat>
       openwave agent-run show <chat> <run>
       openwave agent-run cancel <chat> <run>

       openwave folder connect <path> --chat <id> [--output-format text|json]
       openwave folder list [--chat <id>] [--output-format text|json]
       openwave folder disconnect <path-or-root-id> --chat <id> [--output-format text|json]

The setup commands, the output family, and the folder commands take
--output-format text|json. A key is read from stdin, or from the environment
variable named by --from-env — never from an argument, which every process on
the machine can read.

tui, -p, output, attach, and the setup commands also take --server <url>
[--server-token-env <var>] or --attach, which talks to a server that is already
running instead of embedding one. --attach reads {OPENWAVE_DATA_DIR}/listen.json
(written by serve and the desktop). With --server the token comes from
OPENWAVE_SERVER_TOKEN, or from the named variable; it is never an argument
either. The folder commands do not take --server/--attach: they provision local
host consent in this machine's own broker and product store, and they can run
while serve or the desktop already owns the data directory.";

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
        Some(command) if command == OsStr::new("output") => {
            output_command(&mut args, server_flags.resolve()?)
                .await
                .map(|()| 0)
        }
        Some(command) if command == OsStr::new("attach") => {
            let Some(chat) = args.next() else {
                usage_error("attach requires a chat id");
            };
            let Ok(chat) = ChatId::from_str(&chat.to_string_lossy()) else {
                usage_error("attach expects a chat UUID");
            };
            let Some(file) = args.next() else {
                usage_error("attach requires a file path");
            };
            if args.next().is_some() {
                usage_error("attach accepts exactly one chat id and one file path");
            }
            outputs::attach(chat, file.into(), server_flags.resolve()?)
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
            let mut model = None;
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
                } else if flag == OsStr::new("--model") {
                    let Some(value) = args.next() else {
                        usage_error("--model requires a catalog key");
                    };
                    let Some(value) = value.to_str().map(str::to_owned) else {
                        usage_error("--model expects a UTF-8 catalog key");
                    };
                    if value.is_empty() || value.starts_with("--") {
                        usage_error("--model requires a catalog key");
                    }
                    model = Some(value);
                } else {
                    usage_error(&format!("unknown print-mode argument {flag:?}"));
                }
            }
            print::run(
                prompt,
                chat,
                format,
                permission_mode,
                model,
                server_flags.resolve()?,
            )
            .await
        }
        Some(command)
            if command == OsStr::new("provider")
                || command == OsStr::new("model")
                || command == OsStr::new("settings")
                || command == OsStr::new("mcp-server")
                || command == OsStr::new("chat")
                || command == OsStr::new("agent-run") =>
        {
            let family = command.to_string_lossy().into_owned();
            let (command, format) = parse_setup(&family, text_args(args));
            setup::run(command, format, server_flags.resolve()?)
                .await
                .map(|()| 0)
        }
        // Folder consent is host-machine state: the broker's own state file and
        // this profile's data directory. There is nothing to point at another
        // server, and pointing at one would say the grant lands somewhere it
        // does not — so `--server` is refused rather than ignored.
        Some(command) if command == OsStr::new("folder") => {
            server_flags.refuse("folder");
            match folder::parse(args) {
                Ok(command) => folder::run(command).await.map(|()| 0),
                Err(message) => usage_error(&message),
            }
        }
        Some(other) => {
            usage_error(&format!("unknown command {other:?}"));
        }
    }
}

/// The `--server` / `--server-token-env` / `--attach` choice, lifted out of
/// the arguments.
struct ServerFlags {
    url: Option<String>,
    token_env: Option<String>,
    attach: bool,
}

impl ServerFlags {
    /// Turn the flags plus the environment into the choice to embed or attach.
    fn resolve(self) -> Result<connect::Server> {
        connect::Server::resolve(self.url, self.token_env, self.attach)
    }

    /// Refuse the flags on a command that has no client to point elsewhere.
    ///
    /// Only the explicit flag is an error. `OPENWAVE_SERVER_URL` is ambient —
    /// a shell that exports it so its `-p` runs attach must still be able to
    /// start a daemon.
    fn refuse(&self, command: &str) {
        if self.url.is_some() || self.token_env.is_some() || self.attach {
            usage_error(&format!(
                "{command} runs a server rather than connecting to one, so it takes no --server/--attach"
            ));
        }
    }
}

/// Pull `--server <url>`, `--server-token-env <var>`, and `--attach` out of
/// the arguments wherever they appear, leaving the rest for the per-command
/// parsers.
///
/// A pre-pass rather than an option on each parser: the flags apply to every
/// client command, and no command takes a value beginning with `--` (each
/// parser rejects one), so nothing else can legitimately be spelled this way.
fn take_server_flags(args: Vec<OsString>) -> (Vec<OsString>, ServerFlags) {
    let mut flags = ServerFlags {
        url: None,
        token_env: None,
        attach: false,
    };
    let mut rest = Vec::with_capacity(args.len());
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == OsStr::new("--attach") {
            if flags.attach {
                usage_error("--attach given more than once");
            }
            flags.attach = true;
            continue;
        }
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

/// Parse one `provider`/`model`/`settings`/`mcp-server`/`chat`/`agent-run`
/// invocation.
fn parse_setup(family: &str, args: Vec<String>) -> (SetupCommand, OutputFormat) {
    let mut cursor = Cursor::new(args);
    let verb = cursor.positional(&format!("a {family} subcommand"));
    let command = match (family, verb.as_str()) {
        ("chat", "list") => SetupCommand::ChatList,
        ("chat", "create") => SetupCommand::ChatCreate,
        ("chat", "delete") => {
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            SetupCommand::ChatDelete { chat }
        }
        ("chat", "steer") => {
            // `turn` is the durable turn identity from the chat event stream
            // (not an agent-run id). Remaining positionals are the steer text;
            // `--output-format` may appear anywhere after the turn id.
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            let turn = parse_turn_id(&cursor.positional("a turn id"));
            let mut content_parts = Vec::new();
            let mut format = OutputFormat::Text;
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--output-format" => {
                        format = parse_format(cursor.value("--output-format"));
                    }
                    other if other.starts_with("--") => {
                        usage_error(&format!("unknown chat steer argument {other:?}"));
                    }
                    other => content_parts.push(other.to_owned()),
                }
            }
            if content_parts.is_empty() {
                usage_error("expected steer text after the turn id");
            }
            return (
                SetupCommand::ChatSteer {
                    chat,
                    turn,
                    content: content_parts.join(" "),
                },
                format,
            );
        }
        ("agent-run", "list") => {
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            SetupCommand::AgentRunList { chat }
        }
        ("agent-run", "show") => {
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            let run = parse_agent_run_id(&cursor.positional("an agent-run id"));
            SetupCommand::AgentRunShow { chat, run }
        }
        ("agent-run", "cancel") => {
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            let run = parse_agent_run_id(&cursor.positional("an agent-run id"));
            SetupCommand::AgentRunCancel { chat, run }
        }
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

fn parse_chat_id(value: &str) -> ChatId {
    ChatId::from_str(value).unwrap_or_else(|_| usage_error("expected a chat UUID"))
}

fn parse_turn_id(value: &str) -> TurnId {
    TurnId::from_str(value).unwrap_or_else(|_| usage_error("expected a turn UUID"))
}

fn parse_agent_run_id(value: &str) -> openwave_core::AgentRunId {
    openwave_core::AgentRunId::from_str(value)
        .unwrap_or_else(|_| usage_error("expected an agent-run UUID"))
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

/// Parse and run one `openwave output …` subcommand.
///
/// Positional chat and output ids, matching `openwave attach <chat> <file>`;
/// `--revision <id>` names an exact version instead of the current one, and
/// `--output-format text|json` is the same opt-in the setup family uses.
async fn output_command(
    args: &mut impl Iterator<Item = OsString>,
    server: connect::Server,
) -> Result<()> {
    let subcommand = args.next().unwrap_or_default();
    let chat = match args.next() {
        Some(chat) => match ChatId::from_str(&chat.to_string_lossy()) {
            Ok(chat) => chat,
            Err(_) => usage_error("output commands expect a chat UUID"),
        },
        None => usage_error("output commands require a chat id"),
    };

    if subcommand == OsStr::new("list") {
        let (_, format) = parse_output_trailing_flags(args, /*allow_revision=*/ false);
        return outputs::run(outputs::Command::List { chat }, format, server).await;
    }

    let output = match args.next() {
        Some(output) => match openwave_core::OutputId::from_str(&output.to_string_lossy()) {
            Ok(output) => output,
            Err(_) => usage_error("output commands expect an output UUID"),
        },
        None => usage_error("output commands require an output id"),
    };

    // `export` takes its destination before the flags, so it is read here
    // rather than inside the flag loop.
    let destination = if subcommand == OsStr::new("export") {
        match args.next() {
            Some(path) => Some(std::path::PathBuf::from(path)),
            None => usage_error("output export requires a destination path"),
        }
    } else {
        None
    };

    let allow_revision = subcommand == OsStr::new("show") || subcommand == OsStr::new("export");
    let (revision, format) = parse_output_trailing_flags(args, allow_revision);

    let command = if subcommand == OsStr::new("show") {
        outputs::Command::Show {
            chat,
            output,
            revision,
        }
    } else if subcommand == OsStr::new("revisions") {
        outputs::Command::Revisions { chat, output }
    } else if subcommand == OsStr::new("export") {
        outputs::Command::Export {
            chat,
            output,
            revision,
            destination: destination.expect("export always reads a destination above"),
        }
    } else {
        usage_error("output accepts list, show, revisions, or export");
    };
    outputs::run(command, format, server).await
}

/// Shared flag loop for the output family: optional `--revision` (show/export)
/// and the trailing `--output-format` every verb accepts.
fn parse_output_trailing_flags(
    args: &mut impl Iterator<Item = OsString>,
    allow_revision: bool,
) -> (Option<openwave_core::OutputRevisionId>, OutputFormat) {
    let mut revision = None;
    let mut format = OutputFormat::Text;
    while let Some(flag) = args.next() {
        if flag == OsStr::new("--revision") {
            if !allow_revision {
                usage_error(&format!("unknown output argument {flag:?}"));
            }
            let Some(id) = args.next() else {
                usage_error("--revision requires a revision id");
            };
            match openwave_core::OutputRevisionId::from_str(&id.to_string_lossy()) {
                Ok(id) => revision = Some(id),
                Err(_) => usage_error("--revision expects a revision UUID"),
            }
        } else if flag == OsStr::new("--output-format") {
            let Some(value) = args.next() else {
                usage_error("--output-format requires text or json");
            };
            match OutputFormat::parse(&value.to_string_lossy()) {
                Some(value) => format = value,
                None => usage_error("--output-format expects text or json"),
            }
        } else {
            usage_error(&format!("unknown output argument {flag:?}"));
        }
    }
    (revision, format)
}

fn usage_error(message: &str) -> ! {
    eprintln!("openwave: {message}\n\n{USAGE}");
    std::process::exit(2);
}

/// Configuration for the profile this build talks to.
///
/// Debug builds keep their own keychain service, matching the desktop's
/// dev/release split: a dev daemon must not mutate release secret state.
pub(crate) fn profile_config() -> Result<Config> {
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut config = Config::from_env()?;
    #[cfg(debug_assertions)]
    {
        // `OPENWAVE_KEYCHAIN_SERVICE` lets a headless rig point a debug daemon
        // at a scratch keychain service. A freshly re-linked binary reading the
        // shared `openwave.dev` items trips the macOS ACL prompt, which blocks
        // a session with no UI forever; a scratch service starts empty and
        // every item it creates is owned by this binary, so nothing prompts.
        config.keychain_service = Some(
            std::env::var("OPENWAVE_KEYCHAIN_SERVICE")
                .ok()
                .filter(|service| !service.is_empty())
                .unwrap_or_else(|| "openwave.dev".into()),
        );
    }
    Ok(config)
}

/// Bind the server and run its accept loop, announcing where to reach it.
async fn serve() -> Result<()> {
    let config = profile_config()?;
    let profile = config.profile;
    let data_dir = config.data_dir.clone();
    // Tracing events land in `logs/openwave.log` under the profile data dir
    // (plus stderr in debug builds); see `openwave_server::logging`.
    openwave_server::logging::init_logging(&config.data_dir);
    let server = openwave_server::bind_configured(config).await?;
    // The daemon is the trusted client for its own connected-folder tool calls:
    // it holds this machine's broker state, so a turn that reads a folder an
    // operator connected is executed here rather than parked for a shell that
    // does not exist. It is the same executor `openwave -p` runs, over every
    // conversation this credential can see. See [`folder_executor`].
    let folder_executor = folder_executor::FolderExecutor::new(
        api::client::Client::new(server.local_addr(), server.token())?,
        Some(server.client_executor_token()),
        &data_dir,
    )?;
    if let Some(executor) = folder_executor {
        tokio::spawn(executor.run(folder_executor::Scope::AllChats));
    }
    // The address is the client's entry point: the parent process that launched
    // the daemon reads it from stdout to connect, and a container entrypoint
    // waits on the same line.
    println!("openwave: listening on http://{}", server.local_addr());
    // The token is a secret, so an integrator should capture this process's
    // stdout directly (a piped child) rather than run the daemon under a
    // logging supervisor. On self-host it is not printed at all: the
    // per-launch bearer names nobody on a shared deployment and authenticates
    // nobody there (see the server's `auth` module docs), so printing it into
    // a container's logs would only invite someone to try it.
    if profile != Profile::SelfHost {
        println!("openwave: token {}", server.token());
    }
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
