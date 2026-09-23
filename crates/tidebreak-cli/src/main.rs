//! Tidebreak CLI — the headless daemon and MCP server.
//!
//! `tidebreak serve` boots the in-process HTTP/WebSocket surface and prints the
//! address it bound, so a local client (the desktop shell, or a script) can
//! connect. It binds a loopback, ephemeral port by default, and prints the
//! per-launch bearer token it minted alongside the address — except on the
//! self-host profile, where that token authenticates nobody and the address
//! may be set with `TIDEBREAK_LISTEN_ADDR`. Configuration comes from
//! the environment via [`profile::config`] (`TIDEBREAK_PROFILE`,
//! `TIDEBREAK_DATA_DIR`, `TIDEBREAK_CONTAINER_EXECUTION_ENABLED`,
//! `TIDEBREAK_CONTAINER_IMAGE`, and `TIDEBREAK_LISTEN_ADDR`); the model API key
//! comes from `ANTHROPIC_API_KEY`, and `TIDEBREAK_MCP_CONFIG` may name an
//! external stdio-server configuration file. With `TIDEBREAK_DATA_DIR` unset,
//! `serve` serves the Tidebreak app's own data, so it cannot run while the app
//! does.
//!
//! `tidebreak mcp <workspace>` serves the built-in read-only filesystem tools over
//! MCP stdio, confined to the explicit workspace directory.
//!
//! `tidebreak rehome-secrets` rewrites the desktop profile's stored credentials
//! so their keychain items belong to the running binary's code signature, which
//! stops macOS asking for credentials an earlier build created. Self-host
//! profiles reject this command because they never use the OS keychain.
//!
//! `tidebreak output list|show|revisions|export <chat> …` reads a conversation's
//! outputs and writes one to a path, and `tidebreak attach <chat> <file>` puts a
//! local file into a conversation. Both drive the same server routes the desktop
//! does.
//!
//! `tidebreak -p "<prompt>"` runs one turn without a terminal: same engine, no
//! terminal. stdout carries the assistant's text (or, with
//! `--output-format json`, the turn's event stream as NDJSON), and the exit
//! status says whether the turn completed. `--permission-mode` sets the chat's
//! permission mode for the run, `--model` pins the chat's model selection
//! before the turn, and under `--output-format json` a driving process answers
//! approvals, plans, and questions over stdin — see [`print::protocol`].
//!
//! `tidebreak provider|model|settings|mcp-server …` configure the profile the
//! same way the desktop's settings pages do — over the server's own routes.
//! See [`setup`]; secrets are read from stdin or a named environment variable,
//! never from a command-line argument.
//!
//! `tidebreak plugins install --git <url> --ref <tag-or-sha>` imports one
//! pinned instruction-only plugin over `POST /plugins/install`. The URL must
//! be public HTTPS, and the revision must be a tag or a full commit SHA.
//!
//! `tidebreak code …` drives the code-mode surface — repos, workspaces,
//! sessions, turns, approvals, diffs, and the git/PR flow — over the same
//! `/code/*` routes the desktop uses. `--json` (or `--output-format json`)
//! writes one object, or NDJSON for `code run` and `code watch`. See [`code`].
//!
//! `tidebreak diagnostics snapshot|metrics|export` reads bounded process
//! measurements and local log tails from the same server. The export is a ZIP
//! for performance investigations; the exporter does not read conversations,
//! databases, blobs, attachments, or credential stores.
//!
//! `tidebreak folder connect|list|disconnect` is the headless equivalent of the
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
//! `tidebreak browser list|navigate|snapshot|wait|screenshot|act --json` drive a
//! running Tidebreak browser server through the session-private capability
//! file named by `TIDEBREAK_BROWSER_CAPFILE`. These commands write JSON to
//! stdout and errors to stderr, and they refuse `--server`/`--attach` since the
//! browser server is reached exclusively through the capfile.
//!
//! `tidebreak browser-mcp` serves the same browser operations over MCP stdio.
//! MCP includes act only when the native engine supports trusted input.
//! The native runtime authorizes every browser action.
//! See [`browser`].
//!
//! `tidebreak agent-mcp` serves chat-mode tools over MCP stdio so an external
//! agent can drive a running Tidebreak over the attach contract. Unlike `mcp`
//! and `browser-mcp` it accepts `--server` / `--attach`: it is a client, the
//! same way `-p` is. It runs a server of its own only with `--embed`. See
//! [`agent_mcp`].
//!
//! Which profile a command works on is [`profile`]'s decision:
//! `TIDEBREAK_DATA_DIR` when it is set, and the Tidebreak app's own data
//! otherwise. Nothing defaults to the current directory. With neither
//! `TIDEBREAK_DATA_DIR` nor a flag, a client command connects to the app while
//! it runs, and stops with what to do next when it does not. `--embed` runs
//! the server in this process instead, and a set `TIDEBREAK_DATA_DIR` does the
//! same for every client command but `agent-mcp`. `--server <url>` (or
//! `TIDEBREAK_SERVER_URL`) makes a command a pure client of a server that is
//! already running, with the bearer token coming from `TIDEBREAK_SERVER_TOKEN`
//! — see [`connect`]. `--attach` reads the `listen.json` the running server
//! wrote into the profile's data directory, so the token never rides argv.
//! Two processes embedding servers over one data directory is refused.
//!
//! A bare `tidebreak` prints help. It used to run `serve`; `serve` is spelled
//! out now, by the container entrypoint too.

// `tidebreak-server/postgres` deepens the `output_command` async state machine
// past rustc's default 128-query layout limit, which is how the self-host image
// build broke while every default-feature build stayed green. The limit is a
// compile-time bound, so raising it costs nothing at runtime.
#![recursion_limit = "256"]

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use tidebreak_core::{
    AgentError, Config, ListDir, Profile, ReadFile, Result, SessionId, ToolCtx, ToolRegistry,
    TurnId,
};

mod agent_mcp;
mod api;
mod browser;
mod code;
#[cfg(test)]
mod compatibility;
mod computer_use;
mod connect;
mod diagnostics;
mod event_stream;
mod folder;
mod folder_executor;
mod help;
mod image_output;
mod json_output;
mod outputs;
mod plugins;
mod print;
mod profile;
mod setup;

use help::{set_usage_family, usage_error, Family};
use print::OutputFormat;
use setup::{ChatPlacement, Command as SetupCommand, SecretSource};

/// What `--version` reports.
///
/// The workspace manifest stays at `0.0.0`: release numbering lives on the Git
/// tag and the desktop bundle, not in Cargo. A release build passes the tag
/// through `TIDEBREAK_VERSION` so the published binary states the release it
/// came from. A build without it says so plainly rather than claiming a
/// version it does not have.
const VERSION: &str = match option_env!("TIDEBREAK_VERSION") {
    Some(version) => version,
    None => "0.0.0-unreleased",
};

#[tokio::main]
async fn main() {
    // First, so a panic anywhere in the process reaches the log with its
    // location, thread, and backtrace. The commands that open a profile
    // point it at that profile's boot failure log once they know it.
    tidebreak_server::logging::install_panic_hook(None);
    let outcome = run().await;
    // Log files are written by background threads; write out what they still
    // hold before `exit` ends the process under them.
    tidebreak_server::logging::shutdown();
    match outcome {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            // Print the error's `Display` (e.g. "configuration error: …"), not
            // the `Debug` form the stdlib `Termination` impl would show.
            eprintln!("tidebreak: {error}");
            std::process::exit(1);
        }
    }
}

/// Dispatch one command, returning the process exit status. Only print mode
/// reports anything but `0`, since only it distinguishes a failure of the
/// command from a failure of the work the command drove.
async fn run() -> Result<i32> {
    let (args, server_flags) = take_server_flags(std::env::args_os().skip(1).collect());
    if let Some(family) = help::help_topic(&args) {
        help::print_help(family);
        return Ok(0);
    }
    let mut args = args.into_iter();
    match args.next().as_deref() {
        // A bare `tidebreak` asks what it can do. It used to run `serve` over
        // whatever directory it ran in, which is how a stray profile appeared
        // in a project folder.
        None => {
            if server_flags.given() {
                usage_error("name a command to run with --server, --attach, or --embed");
            }
            help::print_help(Family::Top);
            Ok(0)
        }
        Some(command) if command == OsStr::new("serve") => {
            set_usage_family(Family::Daemon);
            server_flags.refuse("serve");
            if args.next().is_some() {
                usage_error("serve does not accept arguments");
            }
            serve().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("--version") => {
            // A published container has no other way to say what it is: the
            // image tag can be moved, and `serve` needs a database before it
            // reports anything. This is also the smoke test the server image
            // publish runs against a freshly built binary.
            set_usage_family(Family::Daemon);
            server_flags.refuse("--version");
            if args.next().is_some() {
                usage_error("--version does not accept arguments");
            }
            println!("tidebreak {VERSION}");
            Ok(0)
        }
        Some(command) if command == OsStr::new("mcp") => {
            set_usage_family(Family::Daemon);
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
            set_usage_family(Family::Daemon);
            server_flags.refuse("rehome-secrets");
            if args.next().is_some() {
                usage_error("rehome-secrets does not accept arguments");
            }
            rehome_secrets().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("output") => {
            set_usage_family(Family::Output);
            output_command(&mut args, server_flags.resolve()?)
                .await
                .map(|()| 0)
        }
        Some(command) if command == OsStr::new("attach") => {
            set_usage_family(Family::Output);
            let Some(chat) = args.next() else {
                usage_error("attach requires a chat id");
            };
            let Ok(chat) = SessionId::from_str(&chat.to_string_lossy()) else {
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
            set_usage_family(Family::Print);
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
                    match SessionId::from_str(&id.to_string_lossy()) {
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
            set_usage_family(Family::Setup);
            let family = command.to_string_lossy().into_owned();
            let (command, format) = parse_setup(&family, text_args(args));
            setup::run(command, format, server_flags.resolve()?)
                .await
                .map(|()| 0)
        }
        Some(command) if command == OsStr::new("plugins") => {
            set_usage_family(Family::Plugins);
            match plugins::parse(args) {
                Ok(command) => plugins::run(command, server_flags.resolve()?)
                    .await
                    .map(|()| 0),
                Err(message) => usage_error(&message),
            }
        }
        // Folder consent is host-machine state: the broker's own state file and
        // this profile's data directory. There is nothing to point at another
        // server, and pointing at one would say the grant lands somewhere it
        // does not — so `--server` is refused rather than ignored.
        Some(command) if command == OsStr::new("folder") => {
            set_usage_family(Family::Folder);
            server_flags.refuse("folder");
            match folder::parse(args) {
                Ok(command) => folder::run(command).await.map(|()| 0),
                Err(message) => usage_error(&message),
            }
        }
        Some(command) if command == OsStr::new("diagnostics") => {
            set_usage_family(Family::Diagnostics);
            let subcommand = args.next().unwrap_or_default();
            let command = if subcommand == OsStr::new("snapshot") {
                if args.next().is_some() {
                    usage_error("diagnostics snapshot does not accept arguments");
                }
                diagnostics::Command::Snapshot
            } else if subcommand == OsStr::new("metrics") {
                if args.next().is_some() {
                    usage_error("diagnostics metrics does not accept arguments");
                }
                diagnostics::Command::Metrics
            } else if subcommand == OsStr::new("export") {
                let Some(destination) = args.next() else {
                    usage_error("diagnostics export requires a destination path");
                };
                if args.next().is_some() {
                    usage_error("diagnostics export accepts one destination path");
                }
                diagnostics::Command::Export {
                    destination: destination.into(),
                }
            } else {
                usage_error("diagnostics accepts snapshot, metrics, or export");
            };
            diagnostics::run(command, server_flags.resolve()?)
                .await
                .map(|()| 0)
        }
        Some(command) if command == OsStr::new("browser") => {
            set_usage_family(Family::AgentTools);
            server_flags.refuse("browser");
            let mut raw = text_args(args);
            let json_count = raw.iter().filter(|a| a.as_str() == "--json").count();
            if json_count == 0 {
                usage_error("browser commands require --json");
            }
            if json_count > 1 {
                usage_error("duplicate --json");
            }
            if let Some(pos) = raw.iter().position(|a| a == "--json") {
                raw.remove(pos);
            }
            match crate::browser::parse_browser(raw) {
                Ok(command) => crate::browser::run_browser(command).await.map(|()| 0),
                Err(message) => usage_error(&message),
            }
        }
        Some(command) if command == OsStr::new("browser-mcp") => {
            set_usage_family(Family::AgentTools);
            server_flags.refuse("browser-mcp");
            if args.next().is_some() {
                usage_error("browser-mcp accepts no arguments");
            }
            crate::browser::run_browser_mcp().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("computer") => {
            set_usage_family(Family::AgentTools);
            server_flags.refuse("computer");
            let raw = text_args(args);
            match crate::computer_use::parse_computer(raw) {
                Ok(command) => crate::computer_use::run_computer(command).await.map(|()| 0),
                Err(message) => usage_error(&message),
            }
        }
        Some(command) if command == OsStr::new("computer-mcp") => {
            set_usage_family(Family::AgentTools);
            server_flags.refuse("computer-mcp");
            if args.next().is_some() {
                usage_error("computer-mcp accepts no arguments");
            }
            crate::computer_use::run_computer_mcp().await.map(|()| 0)
        }
        Some(command) if command == OsStr::new("agent-mcp") => {
            set_usage_family(Family::AgentTools);
            if args.next().is_some() {
                usage_error("agent-mcp accepts no arguments beyond --server, --attach, or --embed");
            }
            crate::agent_mcp::run(server_flags.resolve_without_implicit_embed("agent-mcp")?)
                .await
                .map(|()| 0)
        }
        Some(command) if command == OsStr::new("code") => {
            set_usage_family(Family::Code);
            match crate::code::parse(text_args(args)) {
                Ok(command) => crate::code::run(command, server_flags.resolve()?).await,
                Err(message) => usage_error(&message),
            }
        }
        Some(other) => {
            usage_error(&format!("unknown command {other:?}"));
        }
    }
}

/// The `--server` / `--server-token-env` / `--attach` / `--embed` choice,
/// lifted out of the arguments.
struct ServerFlags {
    url: Option<String>,
    token_env: Option<String>,
    attach: bool,
    embed: bool,
}

impl ServerFlags {
    /// Turn the flags plus the environment into the choice to embed or attach.
    fn resolve(self) -> Result<connect::Server> {
        connect::Server::resolve(connect::Flags {
            url: self.url,
            token_env: self.token_env,
            attach: self.attach,
            embed: self.embed,
        })
    }

    /// [`Self::resolve`] for a command that drives a server that is already
    /// running. It starts one of its own only when `--embed` says so. A set
    /// `TIDEBREAK_DATA_DIR` alone is not enough: an MCP client launches this
    /// command to drive a Tidebreak someone is using, not a new server over a
    /// folder nobody is watching.
    fn resolve_without_implicit_embed(self, command: &str) -> Result<connect::Server> {
        let embed = self.embed;
        let server = self.resolve()?;
        if matches!(server, connect::Server::Embed) && !embed {
            usage_error(&format!(
                "{command} connects to a Tidebreak server that is already running, and \
                 TIDEBREAK_DATA_DIR alone does not start one. Pass --attach to connect to \
                 the server that owns that folder, --server <url> to connect to another, \
                 or --embed to run one in this process."
            ));
        }
        Ok(server)
    }

    /// Whether any connection flag was given.
    fn given(&self) -> bool {
        self.url.is_some() || self.token_env.is_some() || self.attach || self.embed
    }

    /// Refuse the flags on a command that has no client to point elsewhere.
    ///
    /// Only the explicit flag is an error. `TIDEBREAK_SERVER_URL` is ambient —
    /// a shell that exports it so its `-p` runs attach must still be able to
    /// start a daemon.
    fn refuse(&self, command: &str) {
        if self.given() {
            usage_error(&format!(
                "{command} takes no --server, --attach, or --embed"
            ));
        }
    }
}

/// Pull `--server <url>`, `--server-token-env <var>`, `--attach`, and
/// `--embed` out of the arguments wherever they appear, leaving the rest for
/// the per-command parsers.
///
/// A pre-pass rather than an option on each parser: the flags apply to every
/// client command, and no command takes a value beginning with `--` (each
/// parser rejects one), so nothing else can legitimately be spelled this way.
fn take_server_flags(args: Vec<OsString>) -> (Vec<OsString>, ServerFlags) {
    let mut flags = ServerFlags {
        url: None,
        token_env: None,
        attach: false,
        embed: false,
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
        if arg == OsStr::new("--embed") {
            if flags.embed {
                usage_error("--embed given more than once");
            }
            flags.embed = true;
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
        ("chat", "list") => {
            let mut archived = false;
            let mut format = OutputFormat::Text;
            while let Some(flag) = cursor.next() {
                match flag.as_str() {
                    "--archived" => archived = true,
                    "--output-format" => format = parse_format(cursor.value("--output-format")),
                    other => usage_error(&format!("unknown chat list argument {other:?}")),
                }
            }
            return (SetupCommand::ChatList { archived }, format);
        }
        ("chat", "create") => SetupCommand::ChatCreate,
        ("chat", "delete") => {
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            SetupCommand::ChatDelete { chat }
        }
        ("chat", "pin") => SetupCommand::ChatPlace {
            chat: parse_chat_id(&cursor.positional("a chat id")),
            placement: ChatPlacement::Pin,
        },
        ("chat", "unpin") => SetupCommand::ChatPlace {
            chat: parse_chat_id(&cursor.positional("a chat id")),
            placement: ChatPlacement::Unpin,
        },
        ("chat", "archive") => SetupCommand::ChatPlace {
            chat: parse_chat_id(&cursor.positional("a chat id")),
            placement: ChatPlacement::Archive,
        },
        ("chat", "unarchive") => SetupCommand::ChatPlace {
            chat: parse_chat_id(&cursor.positional("a chat id")),
            placement: ChatPlacement::Unarchive,
        },
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
        ("chat", "regenerate") => {
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            let mut turn = None;
            let mut model = None;
            let mut wait = false;
            let mut format = OutputFormat::Text;
            while let Some(flag) = cursor.next() {
                match flag.as_str() {
                    "--turn" => turn = Some(parse_turn_id(&cursor.value("--turn"))),
                    "--model" => model = Some(cursor.value("--model")),
                    "--wait" => wait = true,
                    "--output-format" => format = parse_format(cursor.value("--output-format")),
                    other => usage_error(&format!("unknown chat regenerate argument {other:?}")),
                }
            }
            return (
                SetupCommand::ChatRegenerate {
                    chat,
                    turn,
                    model,
                    wait,
                },
                format,
            );
        }
        ("chat", "edit") => {
            // The remaining positionals are the new message, as with steer.
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            let mut turn = None;
            let mut wait = false;
            let mut content_parts = Vec::new();
            let mut format = OutputFormat::Text;
            while let Some(arg) = cursor.next() {
                match arg.as_str() {
                    "--turn" => turn = Some(parse_turn_id(&cursor.value("--turn"))),
                    "--wait" => wait = true,
                    "--output-format" => format = parse_format(cursor.value("--output-format")),
                    other if other.starts_with("--") => {
                        usage_error(&format!("unknown chat edit argument {other:?}"));
                    }
                    other => content_parts.push(other.to_owned()),
                }
            }
            if content_parts.is_empty() {
                usage_error("expected the new message after the chat id");
            }
            return (
                SetupCommand::ChatEdit {
                    chat,
                    turn,
                    content: content_parts.join(" "),
                    wait,
                },
                format,
            );
        }
        ("chat", "branch") => {
            let chat = parse_chat_id(&cursor.positional("a chat id"));
            let mut turn = None;
            let mut format = OutputFormat::Text;
            while let Some(flag) = cursor.next() {
                match flag.as_str() {
                    "--turn" => turn = Some(parse_turn_id(&cursor.value("--turn"))),
                    "--output-format" => format = parse_format(cursor.value("--output-format")),
                    other => usage_error(&format!("unknown chat branch argument {other:?}")),
                }
            }
            return (SetupCommand::ChatBranch { chat, turn }, format);
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

fn parse_chat_id(value: &str) -> SessionId {
    SessionId::from_str(value).unwrap_or_else(|_| usage_error("expected a chat UUID"))
}

fn parse_turn_id(value: &str) -> TurnId {
    TurnId::from_str(value).unwrap_or_else(|_| usage_error("expected a turn UUID"))
}

fn parse_agent_run_id(value: &str) -> tidebreak_core::AgentRunId {
    tidebreak_core::AgentRunId::from_str(value)
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

/// Parse and run one `tidebreak output …` subcommand.
///
/// Positional chat and output ids, matching `tidebreak attach <chat> <file>`;
/// `--revision <id>` names an exact version instead of the current one, and
/// `--output-format text|json` is the same opt-in the setup family uses.
async fn output_command(
    args: &mut impl Iterator<Item = OsString>,
    server: connect::Server,
) -> Result<()> {
    let subcommand = args.next().unwrap_or_default();
    let chat = match args.next() {
        Some(chat) => match SessionId::from_str(&chat.to_string_lossy()) {
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
        Some(output) => match tidebreak_core::OutputId::from_str(&output.to_string_lossy()) {
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
) -> (Option<tidebreak_core::OutputRevisionId>, OutputFormat) {
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
            match tidebreak_core::OutputRevisionId::from_str(&id.to_string_lossy()) {
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

/// Configuration for the profile this command works on: `TIDEBREAK_DATA_DIR`,
/// or the Tidebreak app's own data directory, with the credential item that
/// goes with it. See [`profile`].
pub(crate) fn profile_config() -> Result<Config> {
    profile::config()
}

/// Bind the server and run its accept loop, announcing where to reach it.
async fn serve() -> Result<()> {
    let config = profile_config()?;
    let profile = config.profile;
    let data_dir = config.data_dir.clone();
    // Tracing events land in `logs/tidebreak.log` under the profile data dir
    // (plus stderr in debug builds); see `tidebreak_server::logging`.
    tidebreak_server::logging::init_logging(&config.data_dir);
    // A panic, on any thread, also lands in the profile's boot failure log.
    tidebreak_server::logging::install_panic_hook(Some(&config.data_dir));
    if profile == Profile::SelfHost {
        // The container may run as a uid with no home; see the function.
        tidebreak_server::ensure_home_dir();
    }
    let server = tidebreak_server::bind_configured(config).await?;
    // The daemon is the trusted client for its own connected-folder tool calls:
    // it holds this machine's broker state, so a turn that reads a folder an
    // operator connected is executed here rather than parked for a shell that
    // does not exist. It is the same executor `tidebreak -p` runs, over every
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
    println!("tidebreak: listening on http://{}", server.local_addr());
    // The token is a secret, so an integrator should capture this process's
    // stdout directly (a piped child) rather than run the daemon under a
    // logging supervisor. On self-host it is not printed at all: the
    // per-launch bearer names nobody on a shared deployment and authenticates
    // nobody there (see the server's `auth` module docs), so printing it into
    // a container's logs would only invite someone to try it.
    if profile != Profile::SelfHost {
        println!("tidebreak: token {}", server.token());
    }
    // A stop signal ends the accept loop by dropping the server, which removes
    // `listen.json` and releases the data directory. Left to the signal's
    // default, the process would die with the file still naming this port.
    tokio::select! {
        result = server.serve() => result,
        () = stop_signal() => Ok(()),
    }
}

/// Resolve on the first SIGTERM or Ctrl-C (SIGINT).
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    // If no handler could be installed, the signals keep their default action
    // and end the process; until then, keep serving.
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Rewrite the desktop profile's stored credentials so their item belongs to
/// this binary's code signature.
///
/// macOS keeps prompting for credentials an earlier, differently signed build
/// created — see [`tidebreak_server::secret_rehome`] for why an approval given at
/// that prompt does not survive the next rebuild. Run this through Cargo
/// (`cargo run -p tidebreak-cli -- rehome-secrets`) so the dev signing runner
/// applies; access is asked for once more, and then stops being asked.
///
/// Credentials live in one item, so this normally rewrites exactly that one.
/// A profile last written by a build that predates the bundle also has its
/// leftover per-key items swept in on the way past.
///
/// A profile other than the app's first gets back the credentials it stored
/// while it still shared the app's item: they are copied into its own item
/// once, and the shared item is left as it is. See [`profile`].
async fn rehome_secrets() -> Result<()> {
    use tidebreak_core::BUNDLE_KEY;
    use tidebreak_server::secret_rehome::RehomeOutcome;

    let config = profile_config()?;
    adopt_previous_credentials(&config).await?;
    let mut touched = 0usize;
    let mut lost = 0usize;
    for (key, outcome) in tidebreak_server::rehome_configured_secrets(&config).await? {
        // The bundle is the item itself; every other key is a credential that
        // used to have one of its own. Saying "re-homed" about both would tell
        // a reader nothing about which happened.
        let bundle = key == BUNDLE_KEY;
        match outcome {
            RehomeOutcome::Absent => {}
            RehomeOutcome::Rehomed if bundle => {
                touched += 1;
                println!("tidebreak: re-homed the credential bundle");
            }
            RehomeOutcome::Rehomed => {
                touched += 1;
                println!("tidebreak: moved {key} into the credential bundle");
            }
            RehomeOutcome::Skipped(reason) => {
                touched += 1;
                eprintln!("tidebreak: left {key} as it was — {reason}");
            }
            RehomeOutcome::Lost(reason) => {
                touched += 1;
                lost += 1;
                eprintln!("tidebreak: lost {key} — {reason}; store this credential again");
            }
        }
    }
    if touched == 0 {
        println!("tidebreak: no stored credentials to re-home");
    }
    if lost > 0 {
        return Err(AgentError::msg(format!(
            "{lost} credential(s) were removed but could not be stored again"
        )));
    }
    Ok(())
}

/// Copy the credentials a profile other than the app's stored while it shared
/// the app's keychain item into its own item, the first time only.
#[cfg(feature = "keychain")]
async fn adopt_previous_credentials(config: &Config) -> Result<()> {
    use tidebreak_core::KeychainSecretProvider;

    let (Some(previous), Some(own)) = (
        profile::previous_keychain_service(config),
        config.keychain_service.as_deref(),
    ) else {
        return Ok(());
    };
    let adoption = profile::adopt_previous_bundle(
        &KeychainSecretProvider::with_service(previous),
        &KeychainSecretProvider::with_service(own),
    )
    .await?;
    if adoption == profile::Adoption::Copied {
        println!(
            "tidebreak: copied the credentials this profile stored in the shared keychain \
             entry {previous} into its own entry {own}; the shared entry is unchanged"
        );
    }
    Ok(())
}

/// A build without the keychain keeps no desktop credentials to copy.
#[cfg(not(feature = "keychain"))]
async fn adopt_previous_credentials(_config: &Config) -> Result<()> {
    Ok(())
}

/// Serve the built-in read-only filesystem tools over MCP stdio.
async fn serve_mcp(workspace: PathBuf) -> Result<()> {
    let ctx = ToolCtx::try_new_legacy_workspace(SessionId::new(), None, workspace.clone())
        .map_err(|error| {
            AgentError::config(format!(
                "could not open MCP workspace {}: {error}",
                workspace.display()
            ))
        })?;

    let tools = Arc::new(
        ToolRegistry::new()
            .with(Box::new(ReadFile))
            .with(Box::new(ListDir)),
    );
    let server = tidebreak_mcp::McpServer::new(tools, ctx);
    tidebreak_mcp::serve_stdio(server)
        .await
        .map_err(|error| AgentError::msg(format!("MCP stdio error: {error}")))
}
