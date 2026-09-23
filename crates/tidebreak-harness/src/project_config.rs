//! The engine config a repository checkout carries, and what loading it
//! would do.
//!
//! Each engine reads project-level files from the directory it runs in:
//! settings with hooks, MCP server lists, plugins, environment, helper
//! commands, and instructions. Until the user trusts a repository, every
//! adapter launches its engine with that loading turned off
//! ([`crate::ProjectConfig::Skip`]). This module finds the files the pinned
//! engines would load, so the user can see what trusting the repository turns
//! on before they decide.
//!
//! A location is listed only for the engines whose untrusted launch actually
//! skips it. Grok gates its project config behind its own folder trust, which
//! Tidebreak never grants, so trusting a repository changes nothing Grok
//! loads and no location names it.
//!
//! The scan reads only fixed paths, never walks the tree, refuses anything
//! but a regular file for contents, and caps what it reads, so a hostile
//! checkout cannot make it slow or hang.

use std::path::Path;

use tidebreak_core::HarnessKind;

/// The most bytes read from one config file. Anything larger is still listed,
/// with nothing counted.
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
/// The most entries counted in one config directory.
const MAX_DIRECTORY_ENTRIES: usize = 1_000;

/// What loading a config file would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProjectConfigEffect {
    /// Commands the engine runs on its own events, such as a session start.
    Hooks,
    /// MCP servers the engine starts or connects to.
    McpServers,
    /// Plugins, or the marketplaces they install from.
    Plugins,
    /// Packages the engine installs, with their install scripts.
    Packages,
    /// Environment variables the engine and its tools run with.
    EnvironmentVariables,
    /// Rules that allow or deny tools without asking.
    PermissionRules,
    /// Commands the engine runs for credentials, a status line, or other
    /// helpers.
    HelperCommands,
    /// Tools written in code that the engine loads.
    CustomTools,
    /// Workflow scripts the engine runs.
    Workflows,
    /// Prompts the engine runs on a schedule.
    ScheduledTasks,
    /// Agent definitions.
    Agents,
    /// Slash commands.
    Commands,
    /// Skills.
    Skills,
    /// Instructions the engine reads before it starts, such as `CLAUDE.md`.
    Instructions,
    /// Other settings, such as the model or the sandbox.
    Settings,
}

/// One engine config file or directory in a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfigFile {
    /// Path from the checkout root with `/` separators. A directory ends in
    /// `/`.
    pub path: String,
    /// The engines that load it.
    pub engines: Vec<HarnessKind>,
    /// What loading it would do, counted. Empty when the contents could not
    /// be read or summarized; the file is still config the engine loads.
    pub effects: Vec<(ProjectConfigEffect, u32)>,
}

/// How to summarize one known location.
#[derive(Debug, Clone, Copy)]
enum Reader {
    /// Claude Code's `settings.json` shape.
    ClaudeSettings,
    /// An `mcpServers` object, as in `.mcp.json`.
    McpServers,
    /// A hooks file: an object of events, or one with a `hooks` object.
    Hooks,
    /// Codex's `config.toml`.
    CodexConfig,
    /// opencode's `opencode.json` or `opencode.jsonc`.
    OpencodeConfig,
    /// A `package.json` whose dependencies get installed.
    Packages,
    /// A list or map of scheduled tasks.
    ScheduledTasks,
    /// A file that counts as one of `effect`, whatever it holds.
    Single(ProjectConfigEffect),
    /// A directory whose entries are each one of `effect`.
    Directory(ProjectConfigEffect),
}

/// Which checkout a location is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Checkout {
    /// The directory the engine runs in.
    Worktree,
    /// The repository's main checkout. An engine running in a linked worktree
    /// still reads these from there.
    Main,
}

/// One location an engine loads project config from.
struct Location {
    path: &'static str,
    engines: &'static [HarnessKind],
    reader: Reader,
    checkout: Checkout,
}

const CLAUDE: &[HarnessKind] = &[HarnessKind::ClaudeCode];
const CODEX: &[HarnessKind] = &[HarnessKind::Codex];
const OPENCODE: &[HarnessKind] = &[HarnessKind::Opencode];
const CLAUDE_AND_OPENCODE: &[HarnessKind] = &[HarnessKind::ClaudeCode, HarnessKind::Opencode];
const CODEX_AND_OPENCODE: &[HarnessKind] = &[HarnessKind::Codex, HarnessKind::Opencode];

const fn at(path: &'static str, engines: &'static [HarnessKind], reader: Reader) -> Location {
    Location {
        path,
        engines,
        reader,
        checkout: Checkout::Worktree,
    }
}

const fn main_checkout(
    path: &'static str,
    engines: &'static [HarnessKind],
    reader: Reader,
) -> Location {
    Location {
        path,
        engines,
        reader,
        checkout: Checkout::Main,
    }
}

/// Every location an engine loads that its untrusted launch turns off.
/// Order is the order the scan reports.
///
/// - Claude Code: `--setting-sources user` skips both settings files,
///   `.mcp.json`, the instruction files, and the agent, command, skill,
///   output-style, and workflow directories; `CLAUDE_CODE_DISABLE_CRON`
///   skips the scheduled tasks.
/// - Codex: marking the worktree untrusted skips `.codex/` config, hooks, and
///   rules, and the `AGENTS.md` files.
/// - opencode: `OPENCODE_DISABLE_PROJECT_CONFIG` skips `opencode.json`, the
///   whole `.opencode/` directory, and the top-level instruction files;
///   `OPENCODE_DISABLE_EXTERNAL_SKILLS` skips the Claude and agents skill
///   directories.
const LOCATIONS: &[Location] = &[
    at(".claude/settings.json", CLAUDE, Reader::ClaudeSettings),
    main_checkout(
        ".claude/settings.local.json",
        CLAUDE,
        Reader::ClaudeSettings,
    ),
    at(".mcp.json", CLAUDE, Reader::McpServers),
    at(
        ".claude/scheduled_tasks.json",
        CLAUDE,
        Reader::ScheduledTasks,
    ),
    at(
        ".claude/loop.md",
        CLAUDE,
        Reader::Single(ProjectConfigEffect::ScheduledTasks),
    ),
    at(
        ".claude/workflows/",
        CLAUDE,
        Reader::Directory(ProjectConfigEffect::Workflows),
    ),
    at(
        ".claude/agents/",
        CLAUDE,
        Reader::Directory(ProjectConfigEffect::Agents),
    ),
    at(
        ".claude/commands/",
        CLAUDE,
        Reader::Directory(ProjectConfigEffect::Commands),
    ),
    at(
        ".claude/skills/",
        CLAUDE_AND_OPENCODE,
        Reader::Directory(ProjectConfigEffect::Skills),
    ),
    at(
        ".claude/rules/",
        CLAUDE,
        Reader::Directory(ProjectConfigEffect::Instructions),
    ),
    at(
        ".claude/output-styles/",
        CLAUDE,
        Reader::Directory(ProjectConfigEffect::Instructions),
    ),
    at(
        "CLAUDE.md",
        CLAUDE_AND_OPENCODE,
        Reader::Single(ProjectConfigEffect::Instructions),
    ),
    at(
        ".claude/CLAUDE.md",
        CLAUDE,
        Reader::Single(ProjectConfigEffect::Instructions),
    ),
    at(
        "CLAUDE.local.md",
        CLAUDE,
        Reader::Single(ProjectConfigEffect::Instructions),
    ),
    at(".codex/config.toml", CODEX, Reader::CodexConfig),
    main_checkout(".codex/hooks.json", CODEX, Reader::Hooks),
    at(
        ".codex/rules/",
        CODEX,
        Reader::Directory(ProjectConfigEffect::PermissionRules),
    ),
    at(
        "AGENTS.md",
        CODEX_AND_OPENCODE,
        Reader::Single(ProjectConfigEffect::Instructions),
    ),
    at(
        "AGENTS.override.md",
        CODEX,
        Reader::Single(ProjectConfigEffect::Instructions),
    ),
    at("opencode.json", OPENCODE, Reader::OpencodeConfig),
    at("opencode.jsonc", OPENCODE, Reader::OpencodeConfig),
    at(".opencode/opencode.json", OPENCODE, Reader::OpencodeConfig),
    at(".opencode/opencode.jsonc", OPENCODE, Reader::OpencodeConfig),
    at(".opencode/package.json", OPENCODE, Reader::Packages),
    at(
        ".opencode/plugin/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Plugins),
    ),
    at(
        ".opencode/plugins/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Plugins),
    ),
    at(
        ".opencode/tool/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::CustomTools),
    ),
    at(
        ".opencode/tools/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::CustomTools),
    ),
    at(
        ".opencode/agent/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Agents),
    ),
    at(
        ".opencode/agents/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Agents),
    ),
    at(
        ".opencode/mode/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Agents),
    ),
    at(
        ".opencode/modes/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Agents),
    ),
    at(
        ".opencode/command/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Commands),
    ),
    at(
        ".opencode/commands/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Commands),
    ),
    at(
        ".opencode/skill/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Skills),
    ),
    at(
        ".opencode/skills/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Skills),
    ),
    at(
        ".agents/skills/",
        OPENCODE,
        Reader::Directory(ProjectConfigEffect::Skills),
    ),
    at(
        "CONTEXT.md",
        OPENCODE,
        Reader::Single(ProjectConfigEffect::Instructions),
    ),
];

/// The engine config a checkout carries, in a stable order.
///
/// `root` is the directory the engine runs in. `main_checkout` is the
/// repository's main checkout when `root` is a linked worktree of it: a few
/// files are read from there even by an engine running in the worktree.
///
/// Reads only the fixed locations above. A location that is missing, or that
/// the scan cannot open, is left out; one it can open but not summarize is
/// listed with no effects, because the engine would still load it.
#[must_use]
pub fn scan_project_config(root: &Path, main_checkout: Option<&Path>) -> Vec<ProjectConfigFile> {
    LOCATIONS
        .iter()
        .filter_map(|location| {
            let base = match location.checkout {
                Checkout::Worktree => root,
                Checkout::Main => main_checkout.unwrap_or(root),
            };
            let effects = read_location(base, location)?;
            Some(ProjectConfigFile {
                path: location.path.to_owned(),
                engines: location.engines.to_vec(),
                effects,
            })
        })
        .collect()
}

/// Whether any file applies to `engine`, which is what decides whether a
/// session on that engine has anything to ask about.
#[must_use]
pub fn loads_for(files: &[ProjectConfigFile], engine: HarnessKind) -> bool {
    files.iter().any(|file| file.engines.contains(&engine))
}

fn read_location(root: &Path, location: &Location) -> Option<Vec<(ProjectConfigEffect, u32)>> {
    let path = root.join(location.path.trim_end_matches('/'));
    if let Reader::Directory(effect) = location.reader {
        // An empty directory loads nothing, so it is nothing to ask about.
        let count = count_entries(&path).filter(|count| *count > 0)?;
        return Some(vec![(effect, count)]);
    }
    let Some(text) = read_config(&path)? else {
        return Some(Vec::new());
    };
    Some(match location.reader {
        Reader::ClaudeSettings => parse_json(&text)
            .map(|value| claude_settings_effects(&value))
            .unwrap_or_default(),
        Reader::McpServers => parse_json(&text)
            .map(|value| {
                nonzero(vec![(
                    ProjectConfigEffect::McpServers,
                    object_len(value.get("mcpServers")),
                )])
            })
            .unwrap_or_default(),
        Reader::Hooks => parse_json(&text)
            .map(|value| {
                let hooks = value.get("hooks").unwrap_or(&value);
                nonzero(vec![(ProjectConfigEffect::Hooks, hook_count(Some(hooks)))])
            })
            .unwrap_or_default(),
        Reader::CodexConfig => text
            .parse::<toml::Table>()
            .map(|table| codex_config_effects(&table))
            .unwrap_or_default(),
        Reader::OpencodeConfig => parse_json(&text)
            .map(|value| opencode_config_effects(&value))
            .unwrap_or_default(),
        Reader::Packages => parse_json(&text)
            .map(|value| {
                let count = ["dependencies", "devDependencies", "optionalDependencies"]
                    .iter()
                    .map(|key| object_len(value.get(*key)))
                    .sum();
                nonzero(vec![(ProjectConfigEffect::Packages, count)])
            })
            .unwrap_or_default(),
        Reader::ScheduledTasks => parse_json(&text)
            .map(|value| {
                let tasks = value.get("tasks").unwrap_or(&value);
                let count = match tasks {
                    serde_json::Value::Array(tasks) => saturate(tasks.len()),
                    serde_json::Value::Object(tasks) => saturate(tasks.len()),
                    _ => 0,
                };
                nonzero(vec![(ProjectConfigEffect::ScheduledTasks, count)])
            })
            .unwrap_or_default(),
        Reader::Single(effect) => vec![(effect, 1)],
        Reader::Directory(_) => Vec::new(),
    })
}

/// The contents of a config file: `None` when there is no regular file to
/// load, `Some(None)` when there is one but it is too large or not text.
fn read_config(path: &Path) -> Option<Option<String>> {
    use std::io::Read as _;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // A FIFO in the checkout must not park the scan.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Some(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Some(None);
    }
    Some(String::from_utf8(bytes).ok())
}

/// How many entries a config directory holds, not counting dotfiles.
/// `None` when there is no directory there.
fn count_entries(path: &Path) -> Option<u32> {
    if !std::fs::metadata(path).ok()?.is_dir() {
        return None;
    }
    let entries = std::fs::read_dir(path).ok()?;
    let count = entries
        .take(MAX_DIRECTORY_ENTRIES)
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .count();
    Some(saturate(count))
}

/// Parse JSON, tolerating the comments and trailing commas that JSONC and
/// hand-edited settings files carry.
fn parse_json(text: &str) -> Option<serde_json::Value> {
    serde_json::from_str(text)
        .ok()
        .or_else(|| serde_json::from_str(&strip_jsonc(text)).ok())
}

/// Remove `//` and `/* */` comments and trailing commas outside strings.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
            }
            ',' => {
                // Whitespace runs after different commas never overlap, so
                // the look-ahead stays linear over the whole text.
                let next = chars.clone().find(|next| !next.is_whitespace());
                if !matches!(next, Some('}' | ']')) {
                    out.push(ch);
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

fn saturate(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn object_len(value: Option<&serde_json::Value>) -> u32 {
    value
        .and_then(serde_json::Value::as_object)
        .map_or(0, |object| saturate(object.len()))
}

fn array_len(value: Option<&serde_json::Value>) -> u32 {
    value
        .and_then(serde_json::Value::as_array)
        .map_or(0, |array| saturate(array.len()))
}

fn nonzero(effects: Vec<(ProjectConfigEffect, u32)>) -> Vec<(ProjectConfigEffect, u32)> {
    effects
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect()
}

/// Every command hook in a `hooks` object: event → matcher groups → hooks.
/// A group with no `hooks` list counts as one hook.
fn hook_count(hooks: Option<&serde_json::Value>) -> u32 {
    let Some(events) = hooks.and_then(serde_json::Value::as_object) else {
        return 0;
    };
    let count: usize = events
        .values()
        .filter_map(serde_json::Value::as_array)
        .flatten()
        .map(|group| {
            group
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .map_or(1, Vec::len)
        })
        .sum();
    saturate(count)
}

/// Claude Code settings keys whose value is a command the engine runs.
const CLAUDE_HELPER_KEYS: &[&str] = &[
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
    "gcpAuthRefresh",
    "otelHeadersHelper",
    "proxyAuthHelper",
    "statusLine",
    "fileSuggestion",
];

/// Claude Code settings keys this summary counts on their own lines.
const CLAUDE_COUNTED_KEYS: &[&str] = &[
    "hooks",
    "env",
    "permissions",
    "enabledPlugins",
    "extraKnownMarketplaces",
    "enableAllProjectMcpServers",
    "enabledMcpjsonServers",
    "disabledMcpjsonServers",
    "$schema",
];

fn claude_settings_effects(settings: &serde_json::Value) -> Vec<(ProjectConfigEffect, u32)> {
    let Some(object) = settings.as_object() else {
        return Vec::new();
    };
    let permissions = object.get("permissions");
    let permission_rules = ["allow", "deny", "ask"]
        .iter()
        .map(|key| array_len(permissions.and_then(|value| value.get(*key))))
        .sum::<u32>();
    let plugins =
        object_len(object.get("enabledPlugins")) + object_len(object.get("extraKnownMarketplaces"));
    // Approving every `.mcp.json` server is one decision, however many the
    // file lists; `.mcp.json` itself carries the count.
    let mcp_servers = if object
        .get("enableAllProjectMcpServers")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        1
    } else {
        array_len(object.get("enabledMcpjsonServers"))
    };
    let helpers = CLAUDE_HELPER_KEYS
        .iter()
        .filter(|key| object.contains_key(**key))
        .count();
    let other = object
        .keys()
        .filter(|key| {
            !CLAUDE_COUNTED_KEYS.contains(&key.as_str())
                && !CLAUDE_HELPER_KEYS.contains(&key.as_str())
        })
        .count();
    nonzero(vec![
        (ProjectConfigEffect::Hooks, hook_count(object.get("hooks"))),
        (ProjectConfigEffect::McpServers, mcp_servers),
        (ProjectConfigEffect::Plugins, plugins),
        (
            ProjectConfigEffect::EnvironmentVariables,
            object_len(object.get("env")),
        ),
        (ProjectConfigEffect::PermissionRules, permission_rules),
        (ProjectConfigEffect::HelperCommands, saturate(helpers)),
        (ProjectConfigEffect::Settings, saturate(other)),
    ])
}

fn toml_len(value: Option<&toml::Value>) -> u32 {
    match value {
        Some(toml::Value::Table(table)) => saturate(table.len()),
        Some(toml::Value::Array(array)) => saturate(array.len()),
        _ => 0,
    }
}

/// Codex `config.toml` keys this summary counts on their own lines.
const CODEX_COUNTED_KEYS: &[&str] = &[
    "mcp_servers",
    "hooks",
    "permissions",
    "shell_environment_policy",
];

fn codex_config_effects(config: &toml::Table) -> Vec<(ProjectConfigEffect, u32)> {
    let environment = config
        .get("shell_environment_policy")
        .and_then(|policy| policy.get("set"));
    let other = config
        .keys()
        .filter(|key| !CODEX_COUNTED_KEYS.contains(&key.as_str()))
        .count();
    nonzero(vec![
        (ProjectConfigEffect::Hooks, toml_len(config.get("hooks"))),
        (
            ProjectConfigEffect::McpServers,
            toml_len(config.get("mcp_servers")),
        ),
        (
            ProjectConfigEffect::EnvironmentVariables,
            toml_len(environment),
        ),
        (
            ProjectConfigEffect::PermissionRules,
            toml_len(config.get("permissions")),
        ),
        (ProjectConfigEffect::Settings, saturate(other)),
    ])
}

/// opencode config keys this summary counts on their own lines.
const OPENCODE_COUNTED_KEYS: &[&str] = &[
    "mcp",
    "plugin",
    "permission",
    "agent",
    "mode",
    "command",
    "formatter",
    "lsp",
    "$schema",
];

fn opencode_config_effects(config: &serde_json::Value) -> Vec<(ProjectConfigEffect, u32)> {
    let Some(object) = config.as_object() else {
        return Vec::new();
    };
    let permission_rules = match object.get("permission") {
        Some(serde_json::Value::Object(rules)) => saturate(rules.len()),
        Some(serde_json::Value::String(_)) => 1,
        _ => 0,
    };
    // Formatters and language servers are commands opencode starts itself.
    let helpers = object_len(object.get("formatter")) + object_len(object.get("lsp"));
    let other = object
        .keys()
        .filter(|key| !OPENCODE_COUNTED_KEYS.contains(&key.as_str()))
        .count();
    nonzero(vec![
        (
            ProjectConfigEffect::McpServers,
            object_len(object.get("mcp")),
        ),
        (
            ProjectConfigEffect::Plugins,
            array_len(object.get("plugin")),
        ),
        (ProjectConfigEffect::PermissionRules, permission_rules),
        (ProjectConfigEffect::HelperCommands, helpers),
        (
            ProjectConfigEffect::Agents,
            object_len(object.get("agent")) + object_len(object.get("mode")),
        ),
        (
            ProjectConfigEffect::Commands,
            object_len(object.get("command")),
        ),
        (ProjectConfigEffect::Settings, saturate(other)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, contents: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn effects(files: &[ProjectConfigFile], path: &str) -> Vec<(ProjectConfigEffect, u32)> {
        files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path} was not found in {files:?}"))
            .effects
            .clone()
    }

    #[test]
    fn a_checkout_without_engine_config_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "# hello\n");
        write(dir.path(), "src/lib.rs", "");
        assert!(scan_project_config(dir.path(), None).is_empty());
    }

    #[test]
    fn claude_settings_count_hooks_servers_and_helpers() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".claude/settings.json",
            r#"{
              "hooks": {
                "SessionStart": [{"hooks": [{"type": "command", "command": "./setup.sh"}]}],
                "UserPromptSubmit": [{"matcher": "", "hooks": [
                  {"type": "command", "command": "echo one"},
                  {"type": "command", "command": "echo two"}
                ]}]
              },
              "env": {"API_URL": "https://example.test"},
              "permissions": {"allow": ["Bash(npm test)"], "deny": []},
              "apiKeyHelper": "./key.sh",
              "model": "opus"
            }"#,
        );
        write(
            dir.path(),
            ".mcp.json",
            r#"{"mcpServers": {"docs": {"command": "npx", "args": ["docs-mcp"]}}}"#,
        );
        let files = scan_project_config(dir.path(), None);
        assert_eq!(
            effects(&files, ".claude/settings.json"),
            [
                (ProjectConfigEffect::Hooks, 3),
                (ProjectConfigEffect::EnvironmentVariables, 1),
                (ProjectConfigEffect::PermissionRules, 1),
                (ProjectConfigEffect::HelperCommands, 1),
                (ProjectConfigEffect::Settings, 1),
            ]
        );
        assert_eq!(
            effects(&files, ".mcp.json"),
            [(ProjectConfigEffect::McpServers, 1)]
        );
        assert_eq!(files[0].engines, [HarnessKind::ClaudeCode]);
    }

    /// Every engine that skips a repository's instructions until it is
    /// trusted says so, because the session would otherwise lose them
    /// without a word.
    #[test]
    fn instruction_files_name_every_engine_that_skips_them() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "CLAUDE.md", "Be brief.\n");
        write(dir.path(), "AGENTS.md", "Run the tests.\n");
        let files = scan_project_config(dir.path(), None);
        let engines = |path: &str| {
            files
                .iter()
                .find(|file| file.path == path)
                .map(|file| file.engines.clone())
                .unwrap()
        };
        assert_eq!(
            engines("CLAUDE.md"),
            [HarnessKind::ClaudeCode, HarnessKind::Opencode]
        );
        assert_eq!(
            engines("AGENTS.md"),
            [HarnessKind::Codex, HarnessKind::Opencode]
        );
        assert_eq!(
            effects(&files, "CLAUDE.md"),
            [(ProjectConfigEffect::Instructions, 1)]
        );
        assert!(loads_for(&files, HarnessKind::Codex));
        assert!(
            !loads_for(&files, HarnessKind::Grok),
            "Grok keeps its own folder trust, which Tidebreak never grants"
        );
    }

    /// An engine in a linked worktree reads a few files from the main
    /// checkout, so the scan does too.
    #[test]
    fn main_checkout_files_are_read_from_the_main_checkout() {
        let main = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        write(
            main.path(),
            ".claude/settings.local.json",
            r#"{"permissions": {"allow": ["Bash(make)", "Bash(git status)"]}}"#,
        );
        write(
            worktree.path(),
            ".claude/settings.local.json",
            r#"{"permissions": {"allow": ["Bash(ignored)"]}}"#,
        );
        let files = scan_project_config(worktree.path(), Some(main.path()));
        assert_eq!(
            effects(&files, ".claude/settings.local.json"),
            [(ProjectConfigEffect::PermissionRules, 2)]
        );
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_still_summarize() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "opencode.jsonc",
            r#"{
              // Load the team's plugin.
              "plugin": ["opencode-team-plugin",],
              /* and one server */
              "mcp": {"db": {"type": "local", "command": ["db-mcp"]},},
            }"#,
        );
        assert_eq!(
            effects(&scan_project_config(dir.path(), None), "opencode.jsonc"),
            [
                (ProjectConfigEffect::McpServers, 1),
                (ProjectConfigEffect::Plugins, 1),
            ]
        );
    }

    #[test]
    fn codex_config_counts_servers_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".codex/config.toml",
            r#"
model = "gpt-5"

[mcp_servers.docs]
command = "docs-mcp"

[shell_environment_policy.set]
API_URL = "https://example.test"
REGION = "us"
"#,
        );
        assert_eq!(
            effects(&scan_project_config(dir.path(), None), ".codex/config.toml"),
            [
                (ProjectConfigEffect::McpServers, 1),
                (ProjectConfigEffect::EnvironmentVariables, 2),
                (ProjectConfigEffect::Settings, 1),
            ]
        );
    }

    #[test]
    fn unreadable_config_is_listed_without_counts() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".claude/settings.json", "{ not json");
        write(
            dir.path(),
            ".codex/config.toml",
            &"x".repeat(usize::try_from(MAX_CONFIG_BYTES).unwrap() + 1),
        );
        let files = scan_project_config(dir.path(), None);
        assert_eq!(effects(&files, ".claude/settings.json"), []);
        assert_eq!(effects(&files, ".codex/config.toml"), []);
    }

    #[test]
    fn config_directories_count_their_entries() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".opencode/plugin/audit.ts", "export default {}");
        write(
            dir.path(),
            ".opencode/plugin/notify.js",
            "export default {}",
        );
        write(dir.path(), ".opencode/plugin/.DS_Store", "");
        std::fs::create_dir_all(dir.path().join(".claude/agents")).unwrap();
        let files = scan_project_config(dir.path(), None);
        assert_eq!(
            effects(&files, ".opencode/plugin/"),
            [(ProjectConfigEffect::Plugins, 2)]
        );
        assert!(
            !files.iter().any(|file| file.path == ".claude/agents/"),
            "an empty directory loads nothing: {files:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_a_config_file_does_not_hang_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        let fifo = dir.path().join(".claude/settings.json");
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `path` is a valid NUL-terminated string for the call.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(scan_project_config(dir.path(), None).is_empty());
    }
}
