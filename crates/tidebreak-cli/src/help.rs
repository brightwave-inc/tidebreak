//! Help text and usage errors for the `tidebreak` binary.
//!
//! Top-level help is assembled from the same family strings usage errors print,
//! so a verb cannot appear in one place and vanish from the other.

use std::cell::Cell;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::sync::OnceLock;

thread_local! {
    static USAGE_FAMILY: Cell<Family> = const { Cell::new(Family::Top) };
}

/// Which family's usage a parse error should print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Top,
    Daemon,
    Print,
    Output,
    Setup,
    Plugins,
    Diagnostics,
    Data,
    Folder,
    Code,
    AgentTools,
}

impl Family {
    fn usage_text(self) -> &'static str {
        match self {
            Self::Top => top_usage_text(),
            Self::Daemon => DAEMON_USAGE,
            Self::Print => PRINT_USAGE,
            Self::Output => OUTPUT_USAGE,
            Self::Setup => SETUP_USAGE,
            Self::Plugins => PLUGINS_USAGE,
            Self::Diagnostics => DIAGNOSTICS_USAGE,
            Self::Data => DATA_USAGE,
            Self::Folder => FOLDER_USAGE,
            Self::Code => CODE_USAGE,
            Self::AgentTools => agent_tools_text(),
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Self::Top => "Tidebreak command-line interface",
            Self::Daemon => "Daemon",
            Self::Print => "Print mode",
            Self::Output => "Outputs and attachments",
            Self::Setup => "Setup",
            Self::Plugins => "Plugins",
            Self::Diagnostics => "Diagnostics",
            Self::Data => "Data and privacy",
            Self::Folder => "Folders",
            Self::Code => "Code",
            Self::AgentTools => "Agent tools (run inside a Tidebreak session)",
        }
    }
}

/// Record which family the next usage error belongs to.
pub fn set_usage_family(family: Family) {
    USAGE_FAMILY.with(|slot| slot.set(family));
}

/// Exit status for a usage error: a bad flag or argument.
pub(crate) const EXIT_USAGE: i32 = 2;

/// Print a usage error for the current family and exit with [`EXIT_USAGE`].
pub fn usage_error(message: &str) -> ! {
    let family = USAGE_FAMILY.with(Cell::get);
    eprintln!("tidebreak: {message}\n\n{}", family.usage_text());
    std::process::exit(EXIT_USAGE);
}

/// True when `arg` is a help request (`--help`, `-h`, or `help`).
pub fn is_help_token(arg: &OsStr) -> bool {
    arg == OsStr::new("--help") || arg == OsStr::new("-h") || arg == OsStr::new("help")
}

/// True when `arg` is `--help` or `-h` (not the `help` command word).
pub fn is_help_flag(arg: &OsStr) -> bool {
    arg == OsStr::new("--help") || arg == OsStr::new("-h")
}

/// Resolve a help topic from argv after `--server` flags have been stripped.
///
/// `tidebreak help`, `tidebreak --help`, and `tidebreak -h` print top-level
/// help. `tidebreak help code` and `tidebreak code --help` print that family.
pub fn help_topic(args: &[impl AsRef<OsStr>]) -> Option<Family> {
    let args: Vec<&OsStr> = args.iter().map(AsRef::as_ref).collect();
    if args.is_empty() {
        return None;
    }
    if is_help_token(args[0]) {
        return Some(family_from_command(args.get(1).copied()));
    }
    if args.iter().any(|arg| is_help_flag(arg)) {
        return Some(family_from_command(Some(args[0])));
    }
    None
}

fn family_from_command(command: Option<&OsStr>) -> Family {
    match command.and_then(OsStr::to_str) {
        None => Family::Top,
        Some("serve" | "mcp" | "rehome-secrets" | "--version") => Family::Daemon,
        Some("-p" | "--print") => Family::Print,
        Some("output" | "attach") => Family::Output,
        Some("provider" | "model" | "settings" | "mcp-server" | "chat" | "agent-run") => {
            Family::Setup
        }
        Some("plugins") => Family::Plugins,
        Some("diagnostics") => Family::Diagnostics,
        Some("data") => Family::Data,
        Some("folder") => Family::Folder,
        Some("code") => Family::Code,
        Some("browser" | "browser-mcp" | "computer" | "computer-mcp" | "agent-mcp") => {
            Family::AgentTools
        }
        Some(_) => Family::Top,
    }
}

/// Write help for `family` to stdout.
pub fn print_help(family: Family) {
    match family {
        Family::Top => print!("{}", top_help_text()),
        other => print!("{}\n\n{}", other.heading(), other.usage_text()),
    }
}

const DAEMON_USAGE: &str = "\
usage: tidebreak serve
       tidebreak --version
       tidebreak mcp <workspace>
       tidebreak rehome-secrets

serve runs the Tidebreak HTTP server over TIDEBREAK_DATA_DIR, or over the
Tidebreak app's data when that is unset, so it cannot run while the app does.
mcp serves read-only filesystem tools over MCP stdio, confined to the given
workspace. rehome-secrets rewrites desktop-profile credentials so they belong
to this binary's code signature.";

const PRINT_USAGE: &str = "\
usage: tidebreak -p <prompt> [--chat <id>] [--output-format text|json]
                  [--permission-mode ask|auto|allow|plan]
                  [--model <key>]

-p runs one turn without a terminal. stdout carries the assistant's text
(or, with --output-format json, the turn's event stream as NDJSON).
--permission-mode sets the chat's permission mode for the run. --model pins
the chat's model selection before the turn.

-p also takes --server <url> [--server-token-env <var>], --attach, or --embed.";

const OUTPUT_USAGE: &str = "\
usage: tidebreak output list <chat> [--output-format text|json]
       tidebreak output show <chat> <output> [--revision <id>] [--output-format text|json]
       tidebreak output revisions <chat> <output> [--output-format text|json]
       tidebreak output export <chat> <output> <path> [--revision <id>] [--output-format text|json]
       tidebreak attach <chat> <file>

output reads a conversation's outputs. attach puts a local file into a
conversation. Both take --server <url> [--server-token-env <var>], --attach,
or --embed.";

const SETUP_USAGE: &str = "\
usage: tidebreak provider list
       tidebreak provider set-key <kind> [--from-env <var>]
       tidebreak provider remove-key <kind>
       tidebreak model list
       tidebreak model roles
       tidebreak model select <key|auto> [--role <role>]
       tidebreak settings show
       tidebreak settings web-search select <provider|off>
       tidebreak settings web-search set-key <provider> [--from-env <var>]
       tidebreak settings web-search remove-key <provider>
       tidebreak settings exec select <provider|off>
       tidebreak settings exec set-key <provider> [--from-env <var>]
       tidebreak settings exec remove-key <provider>
       tidebreak mcp-server list
       tidebreak mcp-server add <name> (--command <cmd> [--arg <a>]… | --url <url>)
                  [--env-from <var>]… [--cwd <dir>] [--bearer-token-env <var>]
                  [--timeout-ms <ms>] [--disabled]
       tidebreak mcp-server remove <name>
       tidebreak chat list [--archived]
       tidebreak chat create
       tidebreak chat delete <chat>
       tidebreak chat pin|unpin <chat>
       tidebreak chat archive|unarchive <chat>
       tidebreak chat steer <chat> <turn> <text...>
       tidebreak chat retry <chat> [--turn <turn>] [--wait]
       tidebreak chat regenerate <chat> [--turn <turn>] [--model <key>] [--wait]
       tidebreak chat edit <chat> <text...> [--turn <turn>] [--wait]
       tidebreak chat branch <chat> [--turn <turn>]
       tidebreak agent-run list <chat>
       tidebreak agent-run show <chat> <run>
       tidebreak agent-run cancel <chat> <run>

These commands configure the profile the same way the desktop settings pages
do. They take --output-format text|json, and --server <url>
[--server-token-env <var>], --attach, or --embed. A key is read from stdin, or
from the environment variable named by --from-env — never from an argument.";

const DIAGNOSTICS_USAGE: &str = "\
usage: tidebreak diagnostics snapshot
       tidebreak diagnostics metrics
       tidebreak diagnostics export <path>

diagnostics reads process measurements and local log tails from the server.
export writes a ZIP for performance investigations; it does not read
conversations, databases, blobs, attachments, or credential stores.
These commands take --server <url> [--server-token-env <var>], --attach, or
--embed.";

const PLUGINS_USAGE: &str = "\
usage: tidebreak plugins install --git <url> --ref <tag-or-sha> [--json]

plugins install fetches one public HTTPS Git repository at a pinned tag or
full commit SHA and imports it as an instruction-only plugin. A moving
branch is refused. The plugin's files run with the agent's permissions.
--json prints one object stamped with schema_version. These commands take
--server <url> [--server-token-env <var>], --attach, or --embed.";

const DATA_USAGE: &str = "\
usage: tidebreak data show [--output-format text|json]
       tidebreak data backup <path> [--force] [--output-format text|json]
       tidebreak data export <path> [--format markdown|json] [--chat <id>]…
                  [--force] [--output-format text|json]

show prints where the profile lives and how much disk each part uses. backup
writes the data folder as one .tar.gz: the database, attachments, outputs,
skills, plugins, and the rest, without keys, logs, engine tools, earlier
backups, or working files. It copies the database with SQLite's own VACUUM
INTO while Tidebreak keeps running. A server on PostgreSQL refuses it; back
that database up with its own tools. export writes your chats as a .zip of
Markdown files, or as one JSON file with --format json: the messages and the
names of attached files, without coding sessions, tool activity, or
attachment contents. --chat limits it to the chats you name. Neither replaces
a file already at <path> unless you pass --force. With no flag they work on the
app's data through the running app. These commands take --server <url>
[--server-token-env <var>], --attach, or --embed.";

const FOLDER_USAGE: &str = "\
usage: tidebreak folder connect <path> --chat <id> [--output-format text|json]
       tidebreak folder list [--chat <id>] [--output-format text|json]
       tidebreak folder disconnect <path-or-root-id> --chat <id> [--output-format text|json]

folder records standing consent for a host folder. These commands do not
take --server, --attach, or --embed: they provision local host consent in the
profile's own broker and product store, and they can run while serve or the
desktop already owns the data directory.";

/// Short usage for the `code` family. Parse errors print this instead of the
/// whole CLI surface.
const CODE_USAGE: &str = "\
usage: tidebreak code doctor [--refresh]
       tidebreak code repo add <path> [--name <name>] [--base-ref <ref>] [--branch-prefix <p>]
       tidebreak code repo list
       tidebreak code repo rm <id>
       tidebreak code ws new --repo <id|path> [--title <title>] [--base-ref <ref>]
       tidebreak code ws list [--repo <id|path>]
       tidebreak code ws show <id>
       tidebreak code ws archive <id> [--force]
       tidebreak code session start --ws <id> --harness <kind> [--mode plan|ask|auto|allow] [--model <id>] [--reasoning <level>] [--fast]
       tidebreak code session show <id>
       tidebreak code session mode <id> plan|ask|auto|allow
       tidebreak code session reap <id>
       tidebreak code share grant <session-id> <subject> [--level view|contribute]
       tidebreak code share list <session-id>
       tidebreak code share revoke <session-id> <subject>
       tidebreak code share visibility <session-id> private|deployment
       tidebreak code run (--session <id> | --ws <id>) [<message>]
                  [--on-approval wait|fail] [--timeout <secs>]
       tidebreak code approvals [--session <id>]
       tidebreak code approve <approval-id>
       tidebreak code deny <approval-id> [-m <feedback>]
       tidebreak code interrupt --session <id>
       tidebreak code turns --session <id>
       tidebreak code diff --ws <id> [--turn N] [--file PATH]
       tidebreak code files --ws <id> [--turn N]
       tidebreak code git commit --ws <id> [-m MSG]
       tidebreak code git push --ws <id>
       tidebreak code git pr --ws <id> [--title <title>] [--body <body>]
       tidebreak code git status --ws <id>
       tidebreak code action <name> --ws <id>
       tidebreak code watch [--once] [--timeout <secs>]

Every verb takes --json (or --output-format json). run and watch stream NDJSON
under --json. --timeout is seconds. watch --once prints the connect snapshot
and exits. session start without --mode uses the first mode the engine supports:
allow, auto, ask, then plan. Pass --mode ask to require approval prompts.
The code family also takes --server <url> [--server-token-env <var>], --attach,
or --embed.";

/// Usage text shown for `tidebreak browser` (and `browser-mcp`).
const BROWSER_USAGE: &str = "\
usage: tidebreak browser list --json
       tidebreak browser navigate --browser-id <id> --url <url> --json
       tidebreak browser snapshot --browser-id <id> [--max-nodes <n>] --json
       tidebreak browser wait --browser-id <id> --snapshot-id <id> --document-epoch <n> \
              (--url-changed | --load-state <idle|loading|ready> | \
               --text-present <text> | --text-absent <text>) \
              [--timeout-ms <ms>] --json
       tidebreak browser screenshot --browser-id <id> --snapshot-id <id> \
              --document-epoch <n> [--max-width <px>] [--max-height <px>] \
              [--output <path>] --json
       tidebreak browser act --browser-id <id> --snapshot-id <id> \
              --document-epoch <n> --ref <ref> \
              (--click | --focus | --hover | --fill <text> | --select <value> | \
               --check | --uncheck | --press <key> | --scroll-into-view) \
              [--execution-mode <background|foreground>] --json
       tidebreak browser open --url <url> --json
       tidebreak browser close --browser-id <id> --json
       tidebreak browser activate --browser-id <id> --json
       tidebreak browser diagnostics --browser-id <id> \
              [--after-sequence <n>] [--max-entries <n>] --json
       tidebreak browser-mcp

With --output, screenshot writes the decoded image to the given path with
private permissions and prints JSON without base-64 pixels.

Browser commands run inside a Tidebreak session, using the session-private
capfile named by TIDEBREAK_BROWSER_CAPFILE. They do not take --server/--attach.";

/// Usage text shown for `tidebreak computer` (and `computer-mcp`).
const COMPUTER_USAGE: &str = "\
usage: tidebreak computer <tool> --json '<arguments-json>' [--output <path>]
       tidebreak computer-mcp

<tool> is any canonical native or Chrome computer-use tool (run
`tidebreak computer list-tools` for the current set, e.g.
computer_list_windows, computer_capture_screen, computer_click,
computer_type_text, computer_key_press, computer_scroll,
computer_hover, computer_drag, computer_wait).

--json     the tool's argument object, as one JSON string (use '{}' for none)
--output   write the first result image (PNG) to this private path instead of
           discarding it; read the file afterwards to see the pixels

Computer commands run inside a Tidebreak session, using the session-private
capfile named by TIDEBREAK_NATIVE_CAPFILE. They do not take --server/--attach.";

const AGENT_MCP_USAGE: &str = "\
usage: tidebreak agent-mcp

agent-mcp serves chat-mode tools over MCP stdio so an external agent can
drive a running Tidebreak over the attach contract. With no flags it connects
to the Tidebreak app. Unlike mcp and browser-mcp it accepts --server and
--attach: it is a client, the same way -p is. It runs a server of its own
only with --embed, even when TIDEBREAK_DATA_DIR is set.";

const SHARED_NOTES: &str = "\
The setup commands, the output family, the folder commands, and the code
family take --output-format text|json. Code commands also accept --json.
A key is read from stdin, or from the environment variable named by
--from-env — never from an argument, which every process on the machine
can read.

Which data the CLI uses: with TIDEBREAK_DATA_DIR unset, every command works on
the Tidebreak app's own data. -p, output, attach, diagnostics, data,
agent-mcp, plugins, the setup commands, and the code family connect to the app
while it runs, and stop with what to do next when it does not. --embed runs a
server in this process over the app's data instead. Set TIDEBREAK_DATA_DIR to
work on a separate profile in that folder: commands then run their own server
over it, except agent-mcp, which needs --attach, --server, or --embed. A
separate profile keeps its credentials apart from the app's. Nothing uses the
current directory.

--server <url> [--server-token-env <var>] talks to a server that is already
running instead. --attach reads {TIDEBREAK_DATA_DIR}/listen.json (written by
serve and the desktop), or the app's when TIDEBREAK_DATA_DIR is unset. With
--server the token comes from TIDEBREAK_SERVER_TOKEN, or from the named
variable; it is never an argument either. Remote server URLs must use https;
http is accepted only for a loopback host. The folder commands do not take
--server, --attach, or --embed: they provision local host consent in the
profile's own broker and product store, and they can run while serve or the
desktop already owns the data directory.

Browser, computer, browser-mcp, and computer-mcp run inside a Tidebreak
session. They read a session-private capfile and do not take --server/--attach.";

/// One-line descriptions, grouped the same way top-level help is.
const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Daemon",
        &[
            ("serve", "Run the Tidebreak HTTP server"),
            (
                "mcp <workspace>",
                "Serve read-only filesystem tools over MCP stdio",
            ),
            (
                "rehome-secrets",
                "Rewrite stored credentials for this binary's signature",
            ),
            ("--version", "Print the build version"),
        ],
    ),
    (
        "Print",
        &[("-p, --print <prompt>", "Run one unattended turn")],
    ),
    (
        "Outputs",
        &[
            (
                "output list|show|revisions|export",
                "Read or write a conversation output",
            ),
            (
                "attach <chat> <file>",
                "Put a local file into a conversation",
            ),
        ],
    ),
    (
        "Setup",
        &[
            ("provider …", "List providers and set or remove API keys"),
            ("model …", "List models and choose one for a role"),
            (
                "settings …",
                "Show settings and configure web search or exec",
            ),
            ("mcp-server …", "List, add, or remove MCP servers"),
            (
                "chat …",
                "List, create, delete, steer, retry, regenerate, edit, or branch chats",
            ),
            ("agent-run …", "List, show, or cancel agent runs"),
            (
                "plugins install",
                "Install an instruction-only plugin from a pinned Git source",
            ),
        ],
    ),
    (
        "Diagnostics",
        &[(
            "diagnostics snapshot|metrics|export",
            "Read process measurements or export a diagnostics ZIP",
        )],
    ),
    (
        "Data and privacy",
        &[(
            "data show|backup|export",
            "Show where data lives, back it up, or export conversations",
        )],
    ),
    (
        "Folders",
        &[(
            "folder connect|list|disconnect",
            "Record or withdraw standing consent for a host folder",
        )],
    ),
    (
        "Code",
        &[(
            "code …",
            "Drive repos, workspaces, sessions, turns, diffs, and git",
        )],
    ),
    (
        "Agent tools (run inside a Tidebreak session)",
        &[
            (
                "browser …",
                "Drive a session browser (requires TIDEBREAK_BROWSER_CAPFILE)",
            ),
            ("browser-mcp", "Serve browser tools over MCP stdio"),
            (
                "computer …",
                "Drive computer-use tools (requires TIDEBREAK_NATIVE_CAPFILE)",
            ),
            ("computer-mcp", "Serve computer-use tools over MCP stdio"),
            (
                "agent-mcp",
                "Serve chat-mode tools over MCP stdio as a client",
            ),
        ],
    ),
];

const FAMILY_SYNTAX: &[&str] = &[
    DAEMON_USAGE,
    PRINT_USAGE,
    OUTPUT_USAGE,
    SETUP_USAGE,
    PLUGINS_USAGE,
    DIAGNOSTICS_USAGE,
    DATA_USAGE,
    FOLDER_USAGE,
    CODE_USAGE,
    BROWSER_USAGE,
    COMPUTER_USAGE,
    AGENT_MCP_USAGE,
];

fn stacked_syntax() -> String {
    let mut text = String::from("usage:");
    let mut first = true;
    for family in FAMILY_SYNTAX {
        for line in family.lines() {
            let rest = if let Some(rest) = line.strip_prefix("usage: ") {
                rest
            } else if let Some(rest) = line.strip_prefix("       ") {
                rest
            } else if line.is_empty() {
                break;
            } else {
                continue;
            };
            if first {
                let _ = write!(text, " {rest}");
                first = false;
            } else {
                let _ = write!(text, "\n       {rest}");
            }
        }
    }
    text
}

fn top_help() -> String {
    let mut text = String::from(
        "Tidebreak command-line interface\n\n\
         Run `tidebreak help <command>` for a family's syntax.\n\n",
    );
    for (heading, commands) in GROUPS {
        let _ = writeln!(text, "{heading}");
        for (name, summary) in *commands {
            let _ = writeln!(text, "  {name:<42} {summary}");
        }
        text.push('\n');
    }
    text.push_str(&stacked_syntax());
    text.push('\n');
    text.push('\n');
    text.push_str(SHARED_NOTES);
    text
}

fn top_usage() -> String {
    let mut text = stacked_syntax();
    text.push('\n');
    text.push('\n');
    text.push_str(SHARED_NOTES);
    text
}

fn agent_tools_usage() -> String {
    let mut text = String::from("Agent tools (run inside a Tidebreak session)\n\n");
    text.push_str(BROWSER_USAGE);
    text.push_str("\n\n");
    text.push_str(COMPUTER_USAGE);
    text.push_str("\n\n");
    text.push_str(AGENT_MCP_USAGE);
    text
}

static TOP_HELP: OnceLock<String> = OnceLock::new();
static TOP_USAGE: OnceLock<String> = OnceLock::new();
static AGENT_TOOLS: OnceLock<String> = OnceLock::new();

fn top_help_text() -> &'static str {
    TOP_HELP.get_or_init(top_help)
}

pub(crate) fn top_usage_text() -> &'static str {
    TOP_USAGE.get_or_init(top_usage)
}

fn agent_tools_text() -> &'static str {
    AGENT_TOOLS.get_or_init(agent_tools_usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_help_includes_every_family_syntax_line() {
        let help = top_help_text();
        assert!(help.contains("tidebreak code session mode"));
        assert!(help.contains("tidebreak browser act"));
        assert!(help.contains("tidebreak browser open"));
        assert!(help.contains("tidebreak computer "));
        assert!(help.contains("tidebreak computer-mcp"));
        assert!(help.contains("Agent tools (run inside a Tidebreak session)"));
        assert!(help.contains("Run the Tidebreak HTTP server"));
    }

    #[test]
    fn help_topic_accepts_help_flags_and_the_help_command() {
        assert_eq!(help_topic(&["--help"]), Some(Family::Top));
        assert_eq!(help_topic(&["-h"]), Some(Family::Top));
        assert_eq!(help_topic(&["help"]), Some(Family::Top));
        assert_eq!(help_topic(&["help", "code"]), Some(Family::Code));
        assert_eq!(help_topic(&["code", "--help"]), Some(Family::Code));
        assert_eq!(help_topic(&["browser", "-h"]), Some(Family::AgentTools));
        assert_eq!(help_topic(&["output", "list"]), None);
    }
}
