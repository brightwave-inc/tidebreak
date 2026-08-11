//! Deterministic safety analysis for shell commands.
//!
//! Given a raw command and a set of standing allow/deny rules, this decides
//! whether the command may run **without asking** ([`ShellVerdict::Allow`]),
//! must be **put to a human** ([`ShellVerdict::Ask`]), or is structurally
//! unsafe and must never take the auto-run path at all
//! ([`ShellVerdict::Deny`]).
//!
//! It exists so that "remember this command" can mean something narrower than
//! the executable. Without a real parser the only honest rungs are *this exact
//! invocation* and *every call to this tool*, because anything in between —
//! "any `cargo test`" — cannot be matched without knowing where one command
//! ends and the next begins. That gap is also why a command is never handed to
//! a model for judgement: with no deterministic floor beneath it, the model
//! would be the only gate on arbitrary shell.
//!
//! The crate is pure: no process is spawned, no filesystem is touched, no
//! `PATH` is consulted, and no shell needs to exist. A string goes in and a
//! verdict comes out, so the same answer is reachable from the approval gate,
//! from storage, and from a test.
//!
//! Design invariants:
//!
//! * **Real grammar, never regex/string-split.** Commands are parsed into a typed AST with
//!   `brush-parser`. The traversal is closed-world: any AST node kind we are not prepared to reason
//!   about degrades to `Ask`. Because the AST is a typed Rust enum, the closed world is
//!   *compiler-enforced* — a new node variant fails to compile until it is explicitly handled.
//! * **Fail closed.** Any parse error, any unsupported construct, a dynamically-resolved command
//!   word, a glob in the *program* word (`/bin/s*`), ANSI-C `$'...'` quoting, brace expansion
//!   (`{sh,-c,id}`), zsh `=`-expansion, and any substitution the text contains but the AST did not
//!   surface (e.g. nested in a parameter expansion) degrade to `Ask` — never `Allow`.
//! * **Conjunctive allow.** A compound command auto-runs only if *every* leaf sub-command is
//!   independently covered by an allow rule. One uncovered sub-command forces `Ask`.
//! * **Deny precedence.** Interpreter invocations (`sh`/`bash`/`eval` ...), writes to sensitive
//!   paths, and explicit deny rules win over any allow rule, including the `All` ("act without
//!   asking in this folder") rule.
//! * **Specificity is the safety dial.** Restricted programs (wrappers, escalators, editors/pagers,
//!   remote/exfil tools) may only be covered by an `Exact` rule, never by `Prefix`/`All`. Script
//!   executors (`python`, `node`, ...) and package installers (`pip`, `npm`, ...) run code someone
//!   else supplied, so they need a rule that names the program: `Exact` or `Prefix` covers them,
//!   a blanket `All` does not.

use std::sync::OnceLock;

use brush_parser::ast;
use brush_parser::word::{self, WordPiece, WordPieceWithSource};
use brush_parser::{Parser, ParserOptions};
use fancy_regex::Regex;

// --- public API ------------------------------------------------------------

/// The analyzer's classification of a command against a rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellVerdict {
    /// Every sub-command is covered and nothing dangerous is present — auto-run.
    Allow,
    /// Uncertain or uncovered — show a human approval card.
    Ask,
    /// Structurally unsafe (interpreter, sensitive write, explicit deny) — never auto-run.
    Deny,
}

/// The specificity ladder. Breadth is the safety dial (narrow → broad).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRuleKind {
    /// The command's `argv` must equal `tokens` exactly.
    Exact,
    /// The command's `argv` must *start with* `tokens`.
    Prefix,
    /// Matches any command (`tokens` must be empty) — "act without asking in this folder".
    All,
}

/// A single standing allow/deny rule. `tokens` is the literal `argv` to match against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRule {
    /// The rung of the specificity ladder.
    pub kind: CommandRuleKind,
    /// The literal `argv` tokens (empty for `All`).
    pub tokens: Vec<String>,
}

impl CommandRule {
    /// Build a rule, rejecting the shapes that have no meaning
    /// (`All` with tokens; `Exact`/`Prefix` without).
    pub fn new(kind: CommandRuleKind, tokens: Vec<String>) -> Result<Self, &'static str> {
        match kind {
            CommandRuleKind::All if !tokens.is_empty() => Err("an 'all' rule must have no tokens"),
            CommandRuleKind::Exact | CommandRuleKind::Prefix if tokens.is_empty() => {
                Err("an exact/prefix rule requires tokens")
            }
            _ => Ok(Self { kind, tokens }),
        }
    }

    fn matches(&self, argv: &[String]) -> bool {
        match self.kind {
            CommandRuleKind::All => true,
            CommandRuleKind::Exact => argv == self.tokens.as_slice(),
            CommandRuleKind::Prefix => {
                argv.len() >= self.tokens.len() && argv[..self.tokens.len()] == self.tokens[..]
            }
        }
    }
}

/// The standing rules for one (project, root): user-granted allows + denies.
///
/// `deny` holds *user-authored* deny rules. The structural deny floor (interpreters, sensitive-path
/// writes) is built into the analyzer and is not expressible — or removable — here.
#[derive(Debug, Clone, Default)]
pub struct ShellRuleSet {
    /// User-granted allow rules.
    pub allow: Vec<CommandRule>,
    /// User-authored deny rules (win over allows).
    pub deny: Vec<CommandRule>,
}

/// Result of [`analyze_shell_command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellAnalysis {
    /// The classification.
    pub verdict: ShellVerdict,
    /// Human-readable reason, for the audit trail.
    pub reason: String,
    /// The sub-command (rendered) that drove a non-allow verdict, for audit/UI.
    pub offending_command: Option<String>,
}

impl ShellAnalysis {
    fn new(verdict: ShellVerdict, reason: impl Into<String>) -> Self {
        Self {
            verdict,
            reason: reason.into(),
            offending_command: None,
        }
    }

    fn with_offender(verdict: ShellVerdict, reason: impl Into<String>, offender: &str) -> Self {
        Self {
            verdict,
            reason: reason.into(),
            offending_command: Some(offender.to_owned()),
        }
    }
}

/// Classify `command` against `ruleset`.
///
/// Returns [`ShellVerdict::Allow`] only when every sub-command is covered by an allow rule and
/// nothing structurally dangerous, obfuscated, or hidden is present. Everything uncertain degrades
/// to [`ShellVerdict::Ask`]; structurally unsafe constructs return [`ShellVerdict::Deny`].
pub fn analyze_shell_command(command: &str, ruleset: &ShellRuleSet) -> ShellAnalysis {
    if command.trim().is_empty() {
        return ShellAnalysis::new(ShellVerdict::Ask, "empty command");
    }

    // Lexical fail-closed: constructs the AST cannot faithfully represent or that rewrite the token
    // stream (ANSI-C quoting, zsh `=(...)` process substitution, brace expansion).
    if has_obfuscated_construct(command) {
        return ShellAnalysis::new(
            ShellVerdict::Ask,
            "obfuscated construct ($'...', =(...), or brace expansion)",
        );
    }

    let program = match parse_program(command) {
        Ok(program) => program,
        Err(reason) => {
            return ShellAnalysis::new(ShellVerdict::Ask, format!("unparsable command: {reason}"))
        }
    };

    let mut acc = Collected::default();
    if let Err(reason) = collect_program(&program, &mut acc) {
        return ShellAnalysis::new(ShellVerdict::Ask, reason);
    }

    // A substitution the text contains but the AST did not surface is hidden (e.g. nested in a
    // parameter expansion default value) — we cannot vet it. Fail closed.
    if count_substitution_markers(command) > acc.surfaced_substitutions {
        return ShellAnalysis::new(ShellVerdict::Ask, "possible hidden command substitution");
    }

    if acc.simples.is_empty() {
        return ShellAnalysis::new(ShellVerdict::Ask, "no executable command found");
    }

    evaluate(&acc, ruleset, Expansion::Pending)
}

/// Classify one already-parsed `argv` against `ruleset`.
///
/// For callers whose tool takes an executable and an argument vector rather
/// than a command line — there is no shell, so there is nothing to parse, but
/// every reason the analyzer would refuse a leaf still applies. An interpreter
/// is still an interpreter when it arrives as `["bash", "-c", …]` rather than
/// as text, and a wrapper is still one that a prefix grant must not reach
/// through.
///
/// Returns `Ask` for an empty `argv`: nothing to run is not something to
/// allow.
#[must_use]
pub fn analyze_argv(argv: &[String], ruleset: &ShellRuleSet) -> ShellAnalysis {
    let Some(program) = argv.first() else {
        return ShellAnalysis::new(ShellVerdict::Ask, "empty command");
    };
    let acc = Collected {
        simples: vec![SimpleCmd {
            program: program.clone(),
            argv: argv.to_vec(),
            // An argv carries no redirections; there is no shell to write them.
            write_targets: Vec::new(),
            read_targets: Vec::new(),
        }],
        ..Collected::default()
    };
    evaluate(&acc, ruleset, Expansion::Resolved)
}

/// Whether the collected tokens are still subject to shell expansion.
///
/// The analyzer literalizes a word without running it: a parameter expansion
/// or command substitution contributes its *source text* to the token, and
/// glob metacharacters survive verbatim. So on the parsed path a token is a
/// spelling of what will run, not the thing itself, and a path check over it
/// can be dodged by writing the path in a form the shell resolves later. On
/// the `argv` path there is no shell between the check and the `execve`, so a
/// token is exactly the operand the program receives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expansion {
    /// Parsed from a command line: expansions and globs are still unresolved.
    Pending,
    /// An already-resolved `argv`: the tokens are what the program will see.
    Resolved,
}

/// A token that names a path but still carries something the shell will
/// resolve after this analysis: a parameter expansion, a command
/// substitution, or a pathname-expansion metacharacter that could be hiding a
/// literal.
///
/// Such a token cannot be checked against the sensitive-path markers at all —
/// `$HOME/.ssh/authorized_keys` and `/et[c]/shadow` both miss every marker
/// while naming a file the floor exists to protect.
///
/// What matters is what the program does with the token, so every rule below
/// is stated against its [`OperandRole`]. A pattern and a path can be spelled
/// identically — `.*` is the commonest regex there is and `.e*` names a
/// credential file — and only the program tells them apart.
///
/// Three rules, narrowest first:
///
/// - A **command substitution** counts in any operand of any program. Its text
///   is computed by a command that has already run, so nothing about the path
///   is visible here — and unlike a variable it needs no cooperating
///   environment: `awk '{print}' $(cat p)` is a complete exfiltration written
///   in one line. Enumerating the programs it matters for is hopeless, since
///   any program that opens a file will do. The one exemption is a flag-shaped
///   token on a program that takes no path operands, which is `make -j$(nproc)`
///   and its relatives; `sort -o$(cat p)` is a flag too, but `sort` opens paths,
///   so it stays refused.
/// - A **parameter expansion** counts in something already shaped like a path,
///   or in an operand the program will open — so `cat $F` is caught and
///   `echo $USER` is not.
/// - A **glob** counts in an operand the program will open, when the token is
///   rooted outside the working tree (`/…`, `~…`), names a hidden directory or
///   file anywhere along its path, or puts a metacharacter in a segment that
///   is not the last. The hidden-segment part is where the disguises live: the
///   marker list is almost entirely dotfiles, so `.en?` names one specific
///   credential file while matching no marker, and `.git/hook*/pre-commit`
///   reaches a hook — arbitrary code on the next commit — with every character
///   of `.git` spelled out. Ordinary patterns do not glob through hidden
///   directories, so `*.py`, `src/*`, `report[1].pdf` are left alone.
///
///   Outside a path operand only the rooted test survives, because everything
///   else it could say about a token is something a regex says too: `grep
///   '.*foo'`, `sed -E 's/.*//'` and `awk '/.*x/{print}'` are not paths and
///   must not be treated as though they were.
///
/// The glob rule is about what a *spelling* can hide, not about where a glob
/// can reach: a relative glob expands under the working directory, but a `cd`
/// earlier in the line can move that directory somewhere the grant never
/// meant to cover. That is a separate gap and not one this predicate closes.
fn unvettable_path_argument(token: &str, role: OperandRole, program_takes_paths: bool) -> bool {
    if token.contains("$(") || token.contains('`') {
        let exempt_flag = token.starts_with('-') && !program_takes_paths;
        if !exempt_flag {
            return true;
        }
    }
    // Program text is never opened as a path: `$1` is a field reference and
    // `s/.*//` is a substitution. Only the substitution rule above applies.
    if role == OperandRole::Script {
        return false;
    }
    let opens_path = role == OperandRole::Path;
    let path_shaped = token.contains('/') || token.starts_with('~');
    if token.contains('$') && (path_shaped || opens_path) {
        return true;
    }
    if !token.contains(GLOB_METACHARACTERS) {
        return false;
    }
    let rooted = token.starts_with('/') || token.starts_with('~');
    if !opens_path {
        return rooted;
    }
    let segments: Vec<&str> = token.split('/').collect();
    let hides_a_segment = segments
        .iter()
        .any(|segment| segment.starts_with('.') && *segment != "." && *segment != "..");
    let globs_a_parent = segments[..segments.len().saturating_sub(1)]
        .iter()
        .any(|segment| segment.contains(GLOB_METACHARACTERS));
    rooted || hides_a_segment || globs_a_parent
}

const GLOB_METACHARACTERS: [char; 3] = ['*', '?', '['];

/// What a program will do with one of its arguments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OperandRole {
    /// A path the program opens.
    Path,
    /// Program text — a `sed` script or an `awk` program.
    Script,
    /// A flag, or an operand of a program whose operands are not paths: a
    /// pattern, a target name, a number.
    Other,
}

/// A redirect target the analyzer cannot resolve to a path.
///
/// Broader than [`unvettable_path_argument`] because it needs no shape test: a
/// redirect operand *is* a path, so a bare `$F` names a file just as much as
/// `$HOME/x` does — and `F=~/.ssh/authorized_keys` followed by `>> $F` is the
/// whole reason this exists.
fn unvettable_redirect_target(token: &str) -> bool {
    token.contains(['$', '`', '*', '?', '['])
}

/// Programs whose non-flag operands are filesystem paths.
///
/// For these, an operand the shell has yet to resolve is as uncheckable as an
/// unresolved redirect target: `cat $F` opens whatever `$F` names, and the
/// literal `$F` matches no sensitive marker. Programs whose operands are
/// patterns, expressions, or numbers are deliberately absent — `grep`'s first
/// operand is a regex and `find`'s are predicates, so refusing an unresolved
/// operand there would cost far more than it buys. The list only governs bare
/// parameter expansions; a command substitution is refused whoever runs it.
const PATH_OPERAND_PROGRAMS: &[&str] = &[
    "cat",
    "gcat",
    "tac",
    "head",
    "ghead",
    "tail",
    "gtail",
    "nl",
    "od",
    "xxd",
    "hexdump",
    "strings",
    "base64",
    "gbase64",
    "md5sum",
    "gmd5sum",
    "shasum",
    "sha1sum",
    "sha256sum",
    "cksum",
    "sort",
    "gsort",
    "cut",
    "gcut",
    "sed",
    "gsed",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "patch",
    "tar",
    "gtar",
    "bsdtar",
    "cp",
    "gcp",
    "mv",
    "gmv",
    "rm",
    "grm",
    "rmdir",
    "ln",
    "gln",
    "install",
    "ginstall",
    "tee",
    "gtee",
    "touch",
    "gtouch",
    "truncate",
    "gtruncate",
    "shred",
    "gshred",
    "dd",
    "gdd",
    "chmod",
    "gchmod",
    "chown",
    "gchown",
    "chgrp",
    "gchgrp",
    "stat",
    "gstat",
    "file",
    "readlink",
    "greadlink",
    "realpath",
    "grealpath",
    "less",
    "more",
    "open",
    "xdg-open",
    "gzip",
    "gunzip",
    "bzip2",
    "xz",
    "zstd",
    "zip",
    "unzip",
];

/// Programs from [`PATH_OPERAND_PROGRAMS`] whose *first* operand is a script,
/// not a path.
///
/// `awk '{print $1}' data.txt` is the case that matters: the `$1` is a field
/// reference in the program text, and treating it as an unresolved path would
/// refuse one of the most ordinary commands there is. Everything after the
/// script is still a path.
const SCRIPT_FIRST_OPERAND_PROGRAMS: &[&str] = &["awk", "gawk", "mawk", "nawk", "sed", "gsed"];

/// Whether a flag already supplies the script, so no operand slot holds one.
///
/// `sed -e p f` and `sed -ep f` and `sed --expression=p f` all run the same
/// program over the same file, but only the first spends an operand on the
/// script. Miss the other two and the file lands in the slot that gets
/// skipped, which is the whole operand check gone. Reading a cluster as
/// script-supplying when it is not only costs an extra path check, so the test
/// is deliberately generous: any short cluster containing `e` or `f`, and the
/// long forms in both spellings.
fn supplies_script(arg: &str) -> bool {
    if let Some(long) = arg.strip_prefix("--") {
        let name = long.split('=').next().unwrap_or(long);
        return matches!(name, "expression" | "file");
    }
    match arg.strip_prefix('-') {
        Some(cluster) => cluster.contains(['e', 'f']),
        None => false,
    }
}

/// What each of `args` is to this program.
fn operand_roles(program: &str, args: &[String]) -> Vec<OperandRole> {
    let base = basename(program);
    let takes_paths = PATH_OPERAND_PROGRAMS.contains(&base);
    let script_first = SCRIPT_FIRST_OPERAND_PROGRAMS.contains(&base)
        && !args.iter().any(|arg| supplies_script(arg));
    let mut seen_operand = false;
    args.iter()
        .map(|arg| {
            if arg.starts_with('-') {
                return OperandRole::Other;
            }
            let first_operand = !seen_operand;
            seen_operand = true;
            if script_first && first_operand {
                OperandRole::Script
            } else if takes_paths {
                OperandRole::Path
            } else {
                OperandRole::Other
            }
        })
        .collect()
}

/// The three tiers, applied to whatever leaves were collected.
fn evaluate(acc: &Collected, ruleset: &ShellRuleSet, expansion: Expansion) -> ShellAnalysis {
    let pending = expansion == Expansion::Pending;
    let unvettable_argument = |token: &str, role: OperandRole, takes_paths: bool| {
        pending && unvettable_path_argument(token, role, takes_paths)
    };
    let unvettable_target = |token: &str| pending && unvettable_redirect_target(token);
    // (1) Deny floor — wins over every allow rule, including `All`.
    for sc in &acc.simples {
        if INTERPRETERS.contains(&basename(&sc.program)) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Deny,
                format!("interpreter invocation: {}", sc.program),
                &sc.display(),
            );
        }
        for target in &sc.write_targets {
            if hits_sensitive(target) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Deny,
                    format!("write to sensitive path: {target}"),
                    &sc.display(),
                );
            }
        }
        for rule in &ruleset.deny {
            if rule.matches(&sc.argv) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Deny,
                    format!("matches deny rule: {}", sc.display()),
                    &sc.display(),
                );
            }
        }
    }
    for target in &acc.group_write_targets {
        if hits_sensitive(target) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Deny,
                format!("write to sensitive path: {target}"),
                target,
            );
        }
    }

    // (2) Escalation — forces `Ask` over any allow match, including `All`.
    //
    // An assignment runs nothing, so it is never a leaf and its value is never an argument. But
    // naming a sensitive path in one is the first half of `F=…; cat $F`, and the value is the only
    // place that path is ever visible in plain text.
    //
    // These are the same two checks an argument gets, so `PREFIX=../out make` asks for the same
    // reason `make ../out` would. `CFLAGS=-I../include make` does not: `climbs_out` normalizes a
    // path, and `-I../include` is a flag with a path glued to it, not a path. That asymmetry is
    // deliberate — the value that gets expanded back out as a word later is the one worth checking.
    for value in &acc.assignment_values {
        if hits_sensitive(value) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Ask,
                format!("assignment names a sensitive path: {value}"),
                value,
            );
        }
        if climbs_out(value) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Ask,
                format!("assignment escapes folder: {value}"),
                value,
            );
        }
    }
    for sc in &acc.simples {
        let args = &sc.argv[1..];
        if is_execution_enabling(&sc.program, args) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Ask,
                format!("execution-enabling construct: {}", sc.display()),
                &sc.display(),
            );
        }
        if is_destructive(&sc.program, args) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Ask,
                format!("destructive operation: {}", sc.display()),
                &sc.display(),
            );
        }
        let roles = operand_roles(&sc.program, args);
        let takes_paths = PATH_OPERAND_PROGRAMS.contains(&basename(&sc.program));
        for (token, role) in args.iter().zip(roles) {
            if hits_sensitive(token) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Ask,
                    format!("sensitive path in arguments: {}", sc.display()),
                    &sc.display(),
                );
            }
            if climbs_out(token) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Ask,
                    format!("argument escapes folder: {}", sc.display()),
                    &sc.display(),
                );
            }
            if unvettable_argument(token, role, takes_paths) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Ask,
                    format!("unresolved path in arguments: {}", sc.display()),
                    &sc.display(),
                );
            }
        }
        for target in &sc.read_targets {
            if hits_sensitive(target) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Ask,
                    format!("read from sensitive path: {target}"),
                    &sc.display(),
                );
            }
            if unvettable_target(target) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Ask,
                    format!("read from a path that cannot be vetted: {target}"),
                    &sc.display(),
                );
            }
        }
        for target in sc.write_targets.iter().chain(sc.read_targets.iter()) {
            if climbs_out(target) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Ask,
                    format!("redirect target escapes folder: {target}"),
                    &sc.display(),
                );
            }
            // "I cannot resolve this" is a weaker signal than "this is a known-sensitive path",
            // so it earns the tier a human can answer rather than the unappealable one: a
            // timestamped log file is not `> ~/.ssh/authorized_keys`.
            if unvettable_target(target) {
                return ShellAnalysis::with_offender(
                    ShellVerdict::Ask,
                    format!("redirect target cannot be vetted: {target}"),
                    &sc.display(),
                );
            }
        }
    }
    for target in acc
        .group_read_targets
        .iter()
        .chain(acc.group_write_targets.iter())
    {
        if hits_sensitive(target) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Ask,
                format!("redirect to sensitive path: {target}"),
                target,
            );
        }
        if unvettable_target(target) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Ask,
                format!("redirect to a path that cannot be vetted: {target}"),
                target,
            );
        }
        if climbs_out(target) {
            return ShellAnalysis::with_offender(
                ShellVerdict::Ask,
                format!("redirect target escapes folder: {target}"),
                target,
            );
        }
    }

    // (3) Conjunctive allow — every sub-command must be independently covered.
    for sc in &acc.simples {
        let covered = ruleset
            .allow
            .iter()
            .any(|rule| rule_may_cover(rule.kind, &sc.program) && rule.matches(&sc.argv));
        if !covered {
            return ShellAnalysis::with_offender(
                ShellVerdict::Ask,
                format!("no allow rule covers: {}", sc.display()),
                &sc.display(),
            );
        }
    }

    ShellAnalysis::new(
        ShellVerdict::Allow,
        "all sub-commands covered by allow rules",
    )
}

// --- parsing ---------------------------------------------------------------

fn parser_options() -> ParserOptions {
    ParserOptions::default()
}

fn parse_program(command: &str) -> Result<ast::Program, String> {
    let options = parser_options();
    let mut parser = Parser::new(std::io::Cursor::new(command.as_bytes()), &options);
    parser.parse_program().map_err(|err| format!("{err:?}"))
}

// --- AST traversal ---------------------------------------------------------

#[derive(Debug, Clone)]
struct SimpleCmd {
    program: String,
    argv: Vec<String>,
    // File targets of output (`>`/`>>`) and input (`<`) redirects.
    write_targets: Vec<String>,
    read_targets: Vec<String>,
}

impl SimpleCmd {
    fn display(&self) -> String {
        self.argv.join(" ")
    }
}

#[derive(Debug, Default)]
struct Collected {
    simples: Vec<SimpleCmd>,
    // Substitutions the AST actually exposed (vs. the lexical count).
    surfaced_substitutions: usize,
    // Redirects attached to a compound (`{ ...; } > file`) rather than a single command.
    group_write_targets: Vec<String>,
    group_read_targets: Vec<String>,
    // Every assignment value seen, whether or not a program followed it. A pure assignment runs
    // nothing, so it is not a leaf — but the path it names is what a later word expands to.
    assignment_values: Vec<String>,
    // Recursion guard for nested command substitutions (fail closed if exceeded).
    depth: usize,
}

const MAX_DEPTH: usize = 25;

// `Err(reason)` is the internal control-flow signal "degrade to Ask".
type CollectResult = Result<(), String>;

fn collect_program(program: &ast::Program, acc: &mut Collected) -> CollectResult {
    for command in &program.complete_commands {
        collect_compound_list(command, acc)?;
    }
    Ok(())
}

fn collect_compound_list(list: &ast::CompoundList, acc: &mut Collected) -> CollectResult {
    for item in &list.0 {
        collect_and_or_list(&item.0, acc)?;
    }
    Ok(())
}

fn collect_and_or_list(list: &ast::AndOrList, acc: &mut Collected) -> CollectResult {
    collect_pipeline(&list.first, acc)?;
    for and_or in &list.additional {
        match and_or {
            ast::AndOr::And(pipeline) | ast::AndOr::Or(pipeline) => {
                collect_pipeline(pipeline, acc)?
            }
        }
    }
    Ok(())
}

fn collect_pipeline(pipeline: &ast::Pipeline, acc: &mut Collected) -> CollectResult {
    // The pipeline's `bang` (`! cmd`) is ignored — we classify the real command(s) regardless, so a
    // negated interpreter invocation (`! bash -c id`) still hits the deny floor.
    for command in &pipeline.seq {
        collect_command(command, acc)?;
    }
    Ok(())
}

fn collect_command(command: &ast::Command, acc: &mut Collected) -> CollectResult {
    match command {
        ast::Command::Simple(simple) => collect_simple_command(simple, acc),
        ast::Command::Compound(compound, redirects) => {
            match compound {
                // Subshell `(...)` and brace group `{ ...; }` group commands we recurse into.
                ast::CompoundCommand::Subshell(s) => collect_compound_list(&s.list, acc)?,
                ast::CompoundCommand::BraceGroup(b) => collect_compound_list(&b.list, acc)?,
                // Control structures and arithmetic are closed-world rejects (degrade to Ask). Listed
                // exhaustively so a future brush variant fails to compile until reviewed.
                ast::CompoundCommand::Arithmetic(_)
                | ast::CompoundCommand::ArithmeticForClause(_)
                | ast::CompoundCommand::ForClause(_)
                | ast::CompoundCommand::CaseClause(_)
                | ast::CompoundCommand::IfClause(_)
                | ast::CompoundCommand::WhileClause(_)
                | ast::CompoundCommand::UntilClause(_)
                | ast::CompoundCommand::Coprocess(_) => {
                    return Err("unsupported construct: control structure".to_owned());
                }
            }
            // Redirects attached to the whole group have no owning simple command.
            if let Some(redirects) = redirects {
                for redirect in &redirects.0 {
                    let (mut writes, mut reads) = (Vec::new(), Vec::new());
                    collect_redirect(redirect, acc, &mut writes, &mut reads)?;
                    acc.group_write_targets.extend(writes);
                    acc.group_read_targets.extend(reads);
                }
            }
            Ok(())
        }
        ast::Command::Function(_) => Err("unsupported construct: function definition".to_owned()),
        ast::Command::ExtendedTest(_, _) => Err("unsupported construct: extended test".to_owned()),
    }
}

fn collect_simple_command(simple: &ast::SimpleCommand, acc: &mut Collected) -> CollectResult {
    let mut argv: Vec<String> = Vec::new();
    let mut writes: Vec<String> = Vec::new();
    let mut reads: Vec<String> = Vec::new();

    if let Some(prefix) = &simple.prefix {
        for item in &prefix.0 {
            collect_prefix_or_suffix_item(item, acc, &mut argv, &mut writes, &mut reads)?;
        }
    }

    // The program word. Dynamic / zsh `=`-expansion / glob-in-program-word all fail closed.
    if let Some(word) = &simple.word_or_name {
        let program = process_word(word, acc, true)?;
        if program.starts_with('=') {
            return Err(format!("zsh =-expansion command: {program}"));
        }
        if unresolvable_program_re().is_match(&program).unwrap_or(true) {
            return Err(format!("unresolvable program word: {program}"));
        }
        argv.push(program);
    }

    if let Some(suffix) = &simple.suffix {
        for item in &suffix.0 {
            collect_prefix_or_suffix_item(item, acc, &mut argv, &mut writes, &mut reads)?;
        }
    }

    if argv.is_empty() {
        // Pure assignment(s) and/or redirects: executes no program, but the redirect still touches
        // the filesystem.
        acc.group_write_targets.extend(writes);
        acc.group_read_targets.extend(reads);
        return Ok(());
    }

    // The program word must be argv[0]. A bare word in a prefix (not valid bash) would violate this;
    // brush only emits assignments/redirects in prefixes, so argv[0] is the program.
    let program = argv[0].clone();
    acc.simples.push(SimpleCmd {
        program,
        argv,
        write_targets: writes,
        read_targets: reads,
    });
    Ok(())
}

fn collect_prefix_or_suffix_item(
    item: &ast::CommandPrefixOrSuffixItem,
    acc: &mut Collected,
    argv: &mut Vec<String>,
    writes: &mut Vec<String>,
    reads: &mut Vec<String>,
) -> CollectResult {
    match item {
        ast::CommandPrefixOrSuffixItem::Word(word) => {
            let literal = process_word(word, acc, false)?;
            argv.push(literal);
            Ok(())
        }
        ast::CommandPrefixOrSuffixItem::IoRedirect(redirect) => {
            collect_redirect(redirect, acc, writes, reads)
        }
        ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) => {
            let name = match &assignment.name {
                ast::AssignmentName::VariableName(name) => name.as_str(),
                ast::AssignmentName::ArrayElementName(name, _index) => name.as_str(),
            };
            // The raw value text, partitioned at the first `=`.
            let value = match &assignment.value {
                ast::AssignmentValue::Scalar(word) => word.value.clone(),
                ast::AssignmentValue::Array(items) => items
                    .iter()
                    .map(|(_k, v)| v.value.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            if assignment_is_dangerous(name, &value) {
                return Err(format!("dangerous environment assignment: {name}={value}"));
            }
            acc.assignment_values.push(value.clone());
            // `FOO=$(evil) cmd` — a substitution in the value is a sub-command.
            match &assignment.value {
                ast::AssignmentValue::Scalar(word) => {
                    collect_word_substitutions(word, acc)?;
                }
                ast::AssignmentValue::Array(items) => {
                    for (_key, word) in items {
                        collect_word_substitutions(word, acc)?;
                    }
                }
            }
            Ok(())
        }
        ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_kind, subshell) => {
            acc.surfaced_substitutions += 1;
            recurse_into_compound_list(&subshell.list, acc)
        }
    }
}

fn collect_redirect(
    redirect: &ast::IoRedirect,
    acc: &mut Collected,
    writes: &mut Vec<String>,
    reads: &mut Vec<String>,
) -> CollectResult {
    match redirect {
        ast::IoRedirect::File(_fd, kind, target) => {
            let path = match target {
                ast::IoFileRedirectTarget::Filename(word)
                | ast::IoFileRedirectTarget::Duplicate(word) => {
                    let literal = process_word(word, acc, false)?;
                    Some(literal)
                }
                ast::IoFileRedirectTarget::ProcessSubstitution(_kind, subshell) => {
                    acc.surfaced_substitutions += 1;
                    recurse_into_compound_list(&subshell.list, acc)?;
                    None
                }
                ast::IoFileRedirectTarget::Fd(_) => None,
            };
            if let Some(path) = path {
                match kind {
                    // Any `>`-flavored redirect is a write (`<>` is read+write — treated as
                    // a write); `<`-flavored is a read.
                    ast::IoFileRedirectKind::Write
                    | ast::IoFileRedirectKind::Append
                    | ast::IoFileRedirectKind::Clobber
                    | ast::IoFileRedirectKind::ReadAndWrite
                    | ast::IoFileRedirectKind::DuplicateOutput => writes.push(path),
                    ast::IoFileRedirectKind::Read | ast::IoFileRedirectKind::DuplicateInput => {
                        reads.push(path)
                    }
                }
            }
            Ok(())
        }
        // `&>file` / `&>>file` — write to a path.
        ast::IoRedirect::OutputAndError(word, _append) => {
            let literal = process_word(word, acc, false)?;
            writes.push(literal);
            Ok(())
        }
        // Here-doc body and here-string are DATA, never a filesystem path or a command.
        ast::IoRedirect::HereDocument(_, _) | ast::IoRedirect::HereString(_, _) => Ok(()),
    }
}

fn recurse_into_compound_list(list: &ast::CompoundList, acc: &mut Collected) -> CollectResult {
    if acc.depth >= MAX_DEPTH {
        return Err("command nesting too deep".to_owned());
    }
    acc.depth += 1;
    let result = collect_compound_list(list, acc);
    acc.depth -= 1;
    result
}

fn recurse_into_command_substitution(inner: &str, acc: &mut Collected) -> CollectResult {
    if acc.depth >= MAX_DEPTH {
        return Err("command nesting too deep".to_owned());
    }
    let program =
        parse_program(inner).map_err(|err| format!("unparsable command substitution: {err}"))?;
    acc.depth += 1;
    let result = collect_program(&program, acc);
    acc.depth -= 1;
    result
}

/// Literalize a word into its `argv` token, recursing into any command substitutions it surfaces.
///
/// `is_program` is set for the command's program word: a dynamic program word (parameter expansion,
/// command substitution, arithmetic) cannot be statically resolved and fails closed.
fn process_word(word: &ast::Word, acc: &mut Collected, is_program: bool) -> Result<String, String> {
    let pieces = word::parse(&word.value, &parser_options())
        .map_err(|err| format!("unparsable word: {err:?}"))?;
    let mut literal = String::new();
    walk_pieces(&pieces, &word.value, acc, is_program, &mut literal)?;
    Ok(literal)
}

fn walk_pieces(
    pieces: &[WordPieceWithSource],
    source: &str,
    acc: &mut Collected,
    is_program: bool,
    literal: &mut String,
) -> CollectResult {
    for piece in pieces {
        let raw = source.get(piece.start_index..piece.end_index).unwrap_or("");
        match &piece.piece {
            WordPiece::Text(text) | WordPiece::SingleQuotedText(text) => literal.push_str(text),
            WordPiece::EscapeSequence(seq) => literal.push_str(dequote_escape(seq)),
            // ANSI-C `$'...'` can encode obfuscated flags; the lexical guard already caught `$'`, but
            // fail closed here too in case it is reached.
            WordPiece::AnsiCQuotedText(_) => return Err("ANSI-C quoting".to_owned()),
            WordPiece::DoubleQuotedSequence(inner)
            | WordPiece::GettextDoubleQuotedSequence(inner) => {
                walk_pieces(inner, source, acc, is_program, literal)?;
            }
            WordPiece::TildeExpansion(tilde) => literal.push_str(&tilde_to_str(tilde)),
            WordPiece::CommandSubstitution(inner)
            | WordPiece::BackquotedCommandSubstitution(inner) => {
                if is_program {
                    return Err(format!("dynamically-resolved command: {raw}"));
                }
                acc.surfaced_substitutions += 1;
                recurse_into_command_substitution(inner, acc)?;
                literal.push_str(raw);
            }
            WordPiece::ParameterExpansion(_) | WordPiece::ArithmeticExpression(_) => {
                if is_program {
                    return Err(format!("dynamically-resolved command: {raw}"));
                }
                literal.push_str(raw);
            }
        }
    }
    Ok(())
}

/// A `$(...)`/backtick inside a word is itself a sub-command that must be independently covered.
fn collect_word_substitutions(word: &ast::Word, acc: &mut Collected) -> CollectResult {
    let pieces = word::parse(&word.value, &parser_options())
        .map_err(|err| format!("unparsable word: {err:?}"))?;
    collect_pieces_substitutions(&pieces, acc)
}

fn collect_pieces_substitutions(
    pieces: &[WordPieceWithSource],
    acc: &mut Collected,
) -> CollectResult {
    for piece in pieces {
        match &piece.piece {
            WordPiece::CommandSubstitution(inner)
            | WordPiece::BackquotedCommandSubstitution(inner) => {
                acc.surfaced_substitutions += 1;
                recurse_into_command_substitution(inner, acc)?;
            }
            WordPiece::DoubleQuotedSequence(inner)
            | WordPiece::GettextDoubleQuotedSequence(inner) => {
                collect_pieces_substitutions(inner, acc)?;
            }
            WordPiece::AnsiCQuotedText(_) => return Err("ANSI-C quoting".to_owned()),
            WordPiece::Text(_)
            | WordPiece::SingleQuotedText(_)
            | WordPiece::EscapeSequence(_)
            | WordPiece::TildeExpansion(_)
            | WordPiece::ParameterExpansion(_)
            | WordPiece::ArithmeticExpression(_) => {}
        }
    }
    Ok(())
}

fn dequote_escape(seq: &str) -> &str {
    // brush represents an escaped character as e.g. `\;`; the literal value is the trailing char.
    if let Some(rest) = seq.strip_prefix('\\') {
        if !rest.is_empty() {
            return rest;
        }
    }
    seq
}

fn tilde_to_str(tilde: &word::TildeExpr) -> String {
    match tilde {
        word::TildeExpr::Home => "~".to_owned(),
        word::TildeExpr::UserHome(user) => format!("~{user}"),
        word::TildeExpr::WorkingDir => "~+".to_owned(),
        word::TildeExpr::OldWorkingDir => "~-".to_owned(),
        word::TildeExpr::NthDirFromTopOfDirStack { .. }
        | word::TildeExpr::NthDirFromBottomOfDirStack { .. } => "~".to_owned(),
    }
}

// --- lexical fail-closed backstops -----------------------------------------

fn brace_expansion_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{[^{}]*(?:,|\.\.)[^{}]*\}").expect("valid regex"))
}

fn unresolvable_program_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[*?]|\[[^\]]*\]").expect("valid regex"))
}

fn has_obfuscated_construct(raw: &str) -> bool {
    raw.contains("$'") || raw.contains("=(") || brace_expansion_re().is_match(raw).unwrap_or(true)
}

fn count_substitution_markers(raw: &str) -> usize {
    let dollar_paren = raw
        .matches("$(")
        .count()
        .saturating_sub(raw.matches("$((").count());
    let backticks = raw.matches('`').count() / 2;
    let proc_sub = raw.matches("<(").count() + raw.matches(">(").count();
    dollar_paren + backticks + proc_sub
}

// --- path helpers ----------------------------------------------------------

fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// Lexically normalize a POSIX path (resolve `.`/`..`, collapse `//`) without touching the
/// filesystem. Mirrors `posixpath.normpath`.
fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let is_absolute = path.starts_with('/');
    // POSIX: a leading "//" is special, but normpath collapses 3+ to one and keeps exactly two.
    let leading_double = path.starts_with("//") && !path.starts_with("///");
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                if is_absolute {
                    parts.pop();
                } else if matches!(parts.last(), Some(&"..")) || parts.is_empty() {
                    parts.push("..");
                } else {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if is_absolute {
        let prefix = if leading_double { "//" } else { "/" };
        format!("{prefix}{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    }
}

fn climbs_out(path: &str) -> bool {
    let norm = normpath(path);
    norm == ".." || norm.starts_with("../")
}

// --- runtime normalization -------------------------------------------------

const RUNTIME_STEMS: [&str; 8] = [
    "python", "php", "ruby", "perl", "node", "lua", "deno", "bun",
];

fn normalize_runtime(basename: &str) -> String {
    match basename {
        "nodejs" => return "node".to_owned(),
        "pypy" | "pypy3" => return "python".to_owned(),
        _ => {}
    }
    if let Some(rest) = basename.strip_prefix("pypy") {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return "python".to_owned();
        }
    }
    for stem in RUNTIME_STEMS {
        if basename == stem {
            return stem.to_owned();
        }
        if let Some(rest) = basename.strip_prefix(stem) {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
                return stem.to_owned();
            }
        }
    }
    basename.to_owned()
}

// --- flag parsing ----------------------------------------------------------

/// The value passed to the first matching flag, in `-f X` / `-fX` / `--file X` / `--file=X` form.
fn flag_value(args: &[String], flags: &[&str]) -> Option<String> {
    let longs: Vec<&str> = flags
        .iter()
        .copied()
        .filter(|f| f.starts_with("--"))
        .collect();
    let shorts: Vec<&str> = flags
        .iter()
        .copied()
        .filter(|f| !f.starts_with("--"))
        .collect();
    for (i, arg) in args.iter().enumerate() {
        for lf in &longs {
            if arg == lf {
                return Some(args.get(i + 1).cloned().unwrap_or_default());
            }
            if let Some(rest) = arg.strip_prefix(&format!("{lf}=")) {
                return Some(rest.to_owned());
            }
        }
        if !arg.starts_with("--") {
            for sf in &shorts {
                if arg == sf {
                    return Some(args.get(i + 1).cloned().unwrap_or_default());
                }
                if arg.starts_with(sf) && arg.len() > sf.len() {
                    return Some(arg[sf.len()..].to_owned());
                }
            }
        }
    }
    None
}

// --- dangerous environment-variable assignments ----------------------------

const ALWAYS_DANGEROUS_ENV: &[&str] = &[
    "PATH",
    "BASH_ENV",
    "ENV",
    "SHELLOPTS",
    "BASHOPTS",
    "IFS",
    "PROMPT_COMMAND",
    "PS1",
    "PS2",
    "PS3",
    "PS4",
    "PYTHONSTARTUP",
    "PYTHONINSPECT",
    "LUA_INIT",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_PROXY_COMMAND",
    "GIT_EXTERNAL_DIFF",
    "PERL5OPT",
    "RUBYOPT",
    "DOTNET_STARTUP_HOOKS",
    "CORECLR_PROFILER",
    "CORECLR_ENABLE_PROFILING",
    "CORECLR_PROFILER_PATH",
    "ASPNETCORE_HOSTINGSTARTUPASSEMBLIES",
];
const ALWAYS_DANGEROUS_ENV_PREFIXES: &[&str] = &["LD_", "DYLD_"];
const ALWAYS_DANGEROUS_ENV_MARKERS: &[&str] =
    &["PRELOAD", "INSERT_LIBRARIES", "_AUDIT", "PROXY_COMMAND"];

const PROGRAM_VALUED_ENV: &[&str] = &[
    "CC",
    "CXX",
    "CPP",
    "LD",
    "AS",
    "AR",
    "NM",
    "RANLIB",
    "OBJCOPY",
    "OBJDUMP",
    "STRIP",
    "RUSTC",
    "RUSTDOC",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "MANPAGER",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SUDO_ASKPASS",
    "LESSOPEN",
    "LESSCLOSE",
    "GZIP",
    "BROWSER",
    "TERMINFO",
    "TERMCAP",
    "PYTHONHOME",
    "PYTHONEXECUTABLE",
    "PERL5LIB",
    "PERLLIB",
    "PERL5DB",
    "RUBYLIB",
    "CLASSPATH",
    "NODE_PATH",
    "NODE_REPL_EXTERNAL_MODULE",
    "NPM_CONFIG_SCRIPT_SHELL",
    "npm_config_script_shell",
    "R_PROFILE",
    "R_PROFILE_USER",
    "R_ENVIRON",
    "R_ENVIRON_USER",
    "JULIA_STARTUP_FILE",
    "LUA_PATH",
    "LUA_CPATH",
    "QT_PLUGIN_PATH",
    "GEM_HOME",
    "GEM_PATH",
    "BUNDLE_GEMFILE",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
];

const OPTION_BAG_ENV: &[&str] = &[
    "NODE_OPTIONS",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "GOFLAGS",
    "RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "npm_config_node_options",
];

const ENV_INJECT_FLAGS: &[&str] = &[
    "-javaagent",
    "-agentlib",
    "-agentpath",
    "--require",
    "--import",
    "--loader",
    "--experimental-loader",
    "linker=",
    "-toolexec",
    "-exec=",
    "-fplugin",
    "-Xbootclasspath",
    "--script",
];

fn env_value_names_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with('~')
        || value.contains('/')
}

fn env_value_injects(value: &str) -> bool {
    ENV_INJECT_FLAGS.iter().any(|flag| value.contains(*flag))
}

fn assignment_is_dangerous(name: &str, value: &str) -> bool {
    if ALWAYS_DANGEROUS_ENV.contains(&name)
        || ALWAYS_DANGEROUS_ENV_PREFIXES
            .iter()
            .any(|p| name.starts_with(*p))
        || ALWAYS_DANGEROUS_ENV_MARKERS
            .iter()
            .any(|m| name.contains(*m))
    {
        return true;
    }
    if PROGRAM_VALUED_ENV.contains(&name)
        || name.ends_with("_WRAPPER")
        || (name.starts_with("CARGO_TARGET_") && name.ends_with("_RUNNER"))
    {
        return env_value_names_path(value) || env_value_injects(value);
    }
    if OPTION_BAG_ENV.contains(&name)
        || name.ends_with("FLAGS")
        || name.ends_with("_OPTS")
        || name.ends_with("_OPTIONS")
    {
        return env_value_injects(value);
    }
    false
}

// --- sed script danger -----------------------------------------------------

fn sed_addr() -> &'static str {
    r"(?:[0-9$+~,!\s]|/(?:\\/|[^/\n])*/|\\(.)(?:\\.|[^\n])*?\1)*"
}

fn sed_exec_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"(?:^|[;{{}}\n]){}e(?:\s|$|;|\}})", sed_addr())).expect("valid regex")
    })
}

fn sed_write_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"(?:^|[;{{}}\n]){}[wW]\s*\S", sed_addr())).expect("valid regex")
    })
}

fn sed_subst_exec_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"s(\S).*?\1.*?\1[a-zA-Z0-9]*e").expect("valid regex"))
}

fn sed_subst_write_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"s(\S).*?\1.*?\1[a-zA-Z0-9]*[wW]\s*\S").expect("valid regex"))
}

fn sed_script_is_dangerous(script: &str) -> bool {
    // On a regex-engine error (e.g. backtracking limit), treat as dangerous — fail closed.
    sed_exec_re().is_match(script).unwrap_or(true)
        || sed_write_re().is_match(script).unwrap_or(true)
        || sed_subst_exec_re().is_match(script).unwrap_or(true)
        || sed_subst_write_re().is_match(script).unwrap_or(true)
}

fn sed_command_is_dangerous(args: &[String]) -> bool {
    for arg in args {
        if let Some(rest) = arg.strip_prefix("--") {
            let long = format!("--{rest}");
            if long == "--file" || long.starts_with("--file=") {
                return true;
            }
            if let Some(script) = long.strip_prefix("--expression=") {
                if sed_script_is_dangerous(script) {
                    return true;
                }
            }
            continue;
        }
        if let Some(after_dash) = arg.strip_prefix('-') {
            // `-e` consumes the remainder of the token as the script, so only the bundle BEFORE the
            // `e` is flags. `-f` there loads an opaque file.
            let e = after_dash.find('e');
            let flags = match e {
                Some(idx) => &after_dash[..idx],
                None => after_dash,
            };
            if flags.contains('f') {
                return true;
            }
            if let Some(idx) = e {
                let script = &after_dash[idx + 1..];
                if !script.is_empty() && sed_script_is_dangerous(script) {
                    return true;
                }
            }
            continue;
        }
        if sed_script_is_dangerous(arg) {
            return true;
        }
    }
    false
}

// --- script executors ------------------------------------------------------

const SCRIPT_EXECUTOR_PROGRAMS: &[&str] = &[
    "node",
    "nodejs",
    "deno",
    "bun",
    "tsx",
    "ts-node",
    "zx",
    "python",
    "python2",
    "python3",
    "ruby",
    "php",
    "perl",
    "lua",
    "luajit",
    "Rscript",
    "R",
    "tclsh",
    "wish",
    "guile",
    "raku",
    "perl6",
    "groovy",
    "ghc",
    "runghc",
    "runhaskell",
    "elixir",
    "swift",
    "scala",
    "julia",
    "go",
    "dotnet",
    "dotnet-script",
    "mono",
    "tcc",
    "clojure",
    "bb",
];

// --- dangerous git config keys ---------------------------------------------

const DANGEROUS_GIT_CONFIG_KEYS: &[&str] = &[
    "core.pager",
    "core.sshcommand",
    "core.editor",
    "core.fsmonitor",
    "core.hookspath",
    "core.askpass",
    "core.gitproxy",
    "credential.helper",
    "gpg.program",
    "gpg.openpgp.program",
    "sequence.editor",
    "interactive.difffilter",
    "sendemail.smtpserver",
    "sendemail.sendmailcmd",
    "alias.",
    "include.path",
    "includeif",
    "protocol.",
    "safe.directory",
    "diff.external",
    "filter.",
    "fsmonitor.",
    "uploadpack.",
    "receivepack.",
    "ssh.variant",
    "http.proxy",
    "url.",
];

fn git_config_key_is_dangerous(arg_lower: &str) -> bool {
    DANGEROUS_GIT_CONFIG_KEYS
        .iter()
        .any(|k| arg_lower.starts_with(*k))
}

// --- execution-enabling escalation -----------------------------------------

fn is_execution_enabling(program: &str, args: &[String]) -> bool {
    let p = normalize_runtime(basename(program));
    let any_arg = |needles: &[&str]| args.iter().any(|a| needles.contains(&a.as_str()));
    let any_prefix = |prefixes: &[&str]| {
        args.iter()
            .any(|a| prefixes.iter().any(|pre| a.starts_with(pre)))
    };
    let short_bundle_has = |letters: &[char]| {
        args.iter().any(|a| {
            a.starts_with('-')
                && !a.starts_with("--")
                && letters.iter().any(|c| a[1..].contains(*c))
        })
    };
    let operands: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();
    let first_operand = operands.first().copied().unwrap_or("");

    // A script executor pointed at a file outside the folder runs attacker code.
    if SCRIPT_EXECUTOR_PROGRAMS.contains(&p.as_str())
        && operands
            .iter()
            .any(|op| op.starts_with('/') || op.starts_with('~') || climbs_out(op))
    {
        return true;
    }

    match p.as_str() {
        "find" => any_arg(&[
            "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprintf", "-fprint", "-fprint0",
            "-fls",
        ]),
        "git" => {
            if any_arg(&["-c", "--exec-path", "--upload-pack", "--receive-pack"])
                || any_prefix(&["-c", "--exec-path=", "--upload-pack=", "--receive-pack="])
            {
                return true;
            }
            let sub = first_operand;
            if matches!(sub, "filter-branch" | "filter-repo" | "daemon" | "instaweb") {
                return true;
            }
            if sub == "bisect" && args.iter().any(|a| a == "run") {
                return true;
            }
            if sub == "submodule" && args.iter().any(|a| a == "foreach") {
                return true;
            }
            if sub == "rebase" && (any_arg(&["-x", "--exec"]) || any_prefix(&["--exec="])) {
                return true;
            }
            if matches!(sub, "difftool" | "mergetool")
                && (any_arg(&["--extcmd", "-x"]) || any_prefix(&["--extcmd="]))
            {
                return true;
            }
            if sub == "send-email"
                && (any_arg(&["--sendmail-cmd", "--smtp-server"])
                    || any_prefix(&["--sendmail-cmd=", "--smtp-server="]))
            {
                return true;
            }
            if sub == "config" {
                return args
                    .iter()
                    .any(|a| git_config_key_is_dangerous(&a.to_lowercase()));
            }
            false
        }
        "tar" | "gtar" | "bsdtar" => any_prefix(&[
            "--to-command",
            "--checkpoint-action",
            "--use-compress-program",
            "-I",
            "--rmt-command",
            "--rsh-command",
            "--info-script",
            "-F",
            "--newer-mtime",
        ]),
        "awk" | "gawk" | "mawk" | "nawk" => {
            if any_prefix(&["-f", "--file", "--source", "-i", "--include", "-e"]) {
                return true;
            }
            operands
                .iter()
                .any(|a| a.contains('|') || a.contains("system(") || a.contains("getline"))
        }
        "sed" | "gsed" => sed_command_is_dangerous(args),
        "node" | "deno" | "bun" => {
            if first_operand == "eval" {
                return true;
            }
            any_arg(&[
                "-e",
                "--eval",
                "-p",
                "--print",
                "-r",
                "--require",
                "--import",
            ]) || any_prefix(&[
                "-e",
                "--eval=",
                "--require=",
                "--loader",
                "--experimental-loader",
                "--import=",
            ])
        }
        "php" => any_arg(&["-r", "-R", "-F", "-B", "-E"]) || any_prefix(&["-r", "-R"]),
        "ruby" => args.iter().any(|a| {
            a.starts_with('-')
                && !a.starts_with("--")
                && (a[1..].contains('e') || a[1..].contains('r'))
        }),
        "perl" => args.iter().any(|a| {
            a.starts_with('-')
                && !a.starts_with("--")
                && a[1..].chars().any(|c| matches!(c, 'e' | 'E' | 'M'))
        }),
        "python" => short_bundle_has(&['c']),
        "lua" | "luajit" | "Rscript" => short_bundle_has(&['e']) || any_prefix(&["--eval="]),
        "pandoc" => {
            any_arg(&["-F", "--filter", "-L", "--lua-filter"])
                || any_prefix(&["--filter=", "--lua-filter="])
        }
        "cmake" => any_arg(&["-P", "-E"]) || any_prefix(&["-P", "-E"]),
        "make" | "gmake" | "gnumake" | "bmake" => {
            let makefile = flag_value(args, &["-f", "--file", "--makefile"]);
            matches!(makefile, Some(m) if m == "-" || m.starts_with('/') || m.starts_with('~') || climbs_out(&m))
        }
        "ln" => {
            any_arg(&["-s", "-sf", "-fs", "--symbolic"])
                || args
                    .iter()
                    .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('s'))
        }
        "zip" => any_arg(&["-TT"]) || any_prefix(&["--unzip-command", "--test-command", "-TT"]),
        "fd" | "fdfind" => any_arg(&["-x", "--exec", "-X", "--exec-batch"]),
        "rg" | "ag" => {
            if any_arg(&["--pre"]) || any_prefix(&["--pre=", "--pre-glob"]) {
                return true;
            }
            matches!(flag_value(args, &["--pager"]), Some(pager) if pager.contains('/') || pager.starts_with('~'))
        }
        "go" => {
            if first_operand == "generate" {
                return true;
            }
            any_arg(&["-exec", "-toolexec"]) || any_prefix(&["-exec=", "-toolexec="])
        }
        "pip" | "pip3" => any_prefix(&["--global-option", "--install-option", "--build-option"]),
        "npm" | "pnpm" => matches!(first_operand, "exec" | "explore" | "dlx"),
        "yarn" => first_operand == "dlx",
        "mvn" | "mvnw" => any_prefix(&["-Dexec.executable"]),
        "java" => {
            let jar = flag_value(args, &["-jar"]);
            matches!(jar, Some(j) if j.starts_with('/') || j.starts_with('~') || climbs_out(&j))
        }
        "R" | "guile" | "raku" | "perl6" | "groovy" | "ghc" | "runghc" | "runhaskell"
        | "elixir" | "iex" | "swift" | "scala" | "julia" | "ts-node" | "tsx" | "zx" | "clojure"
        | "bb" | "dotnet-script" => {
            any_arg(&["-e", "-c", "--eval", "--command"]) || any_prefix(&["-e", "-c", "--eval="])
        }
        "tcc" => any_arg(&["-run"]) || any_prefix(&["-run"]),
        "stap" => any_arg(&["-e"]) || any_prefix(&["-e"]),
        "poetry" | "pdm" | "hatch" | "pipenv" | "rye" => {
            matches!(first_operand, "run" | "exec" | "shell")
        }
        "open" => {
            any_arg(&["-a", "--application"])
                || any_prefix(&["-a", "--application="])
                || operands.iter().any(|op| {
                    op.ends_with(".app") || op.ends_with(".command") || op.ends_with(".workflow")
                })
        }
        _ => false,
    }
}

// --- destructive escalation ------------------------------------------------

fn is_destructive(program: &str, args: &[String]) -> bool {
    let p = basename(program);
    let short_flag_has = |letter: char| {
        args.iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains(letter))
    };
    let operands: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();
    let has = |needle: &str| args.iter().any(|a| a == needle);
    let any_prefix = |pre: &str| args.iter().any(|a| a.starts_with(pre));

    match p {
        "rm" => {
            let recursive = short_flag_has('r')
                || short_flag_has('R')
                || args.iter().any(|a| a.to_lowercase().contains("recursive"));
            let broad = operands.iter().any(|op| {
                op.contains('*')
                    || op.contains('?')
                    || op.contains('[')
                    || matches!(*op, "." | ".." | "/" | "~")
                    || op.starts_with('/')
                    || op.starts_with('~')
            });
            recursive || broad
        }
        "dd" | "gdd" | "ddrescue" | "dcfldd" | "shred" | "gshred" | "truncate" | "gtruncate"
        | "mkfs" | "srm" | "scrub" | "wipe" | "fallocate" | "gfallocate" | "diskutil"
        | "chflags" | "wipefs" | "blkdiscard" | "sgdisk" | "gdisk" | "fdisk" | "sfdisk"
        | "cfdisk" | "parted" => true,
        "chmod" | "gchmod" | "chown" | "gchown" | "chgrp" | "gchgrp" => {
            if has("--recursive") || short_flag_has('R') {
                return true;
            }
            (p == "chmod" || p == "gchmod")
                && operands
                    .iter()
                    .any(|op| op.contains("+s") || setuid_octal_re().is_match(op).unwrap_or(true))
        }
        "git" => {
            let sub = operands.first().copied().unwrap_or("");
            let restore_staged_only =
                (has("--staged") || has("-S")) && !has("--worktree") && !has("-W");
            (sub == "clean" && (short_flag_has('f') || any_prefix("--force")))
                || (sub == "reset" && has("--hard"))
                || (sub == "checkout"
                    && (has("-f")
                        || has("--force")
                        || has("--")
                        || has("--ours")
                        || has("--theirs")
                        || operands.get(1..).is_some_and(|rest| rest.contains(&"."))))
                || (sub == "switch" && (has("-f") || has("--force") || has("--discard-changes")))
                || (sub == "restore" && !restore_staged_only)
                || (sub == "read-tree" && has("--reset"))
                || (sub == "stash" && (has("drop") || has("clear")))
                || (sub == "branch" && (has("-D") || has("-d")))
                || (sub == "tag" && (has("-d") || has("--delete")))
                || (sub == "push"
                    && (has("--mirror")
                        || has("--delete")
                        || has("-d")
                        || has("-f")
                        || any_prefix("--force")
                        || operands.iter().any(|op| op.starts_with(':'))))
                || (sub == "update-ref" && has("-d"))
                || (sub == "update-index" && (has("--force-remove") || has("--remove")))
                || (sub == "symbolic-ref" && has("-d"))
                || (sub == "replace" && (has("-d") || has("--delete")))
                || (sub == "notes" && (has("remove") || has("prune")))
                || (sub == "reflog" && (has("expire") || has("delete")))
                || (sub == "gc" && any_prefix("--prune"))
                || (sub == "rm")
                || (sub == "worktree"
                    && (has("prune")
                        || (has("remove") && (short_flag_has('f') || any_prefix("--force")))))
        }
        _ => p.starts_with("mkfs."),
    }
}

fn setuid_octal_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[4267][0-7]{3}$").expect("valid regex"))
}

// --- sensitive paths (deny floor) ------------------------------------------

const SENSITIVE_TARGET_MARKERS: &[&str] = &[
    ".ssh/",
    "authorized_keys",
    "known_hosts",
    "id_rsa",
    "id_ed25519",
    ".aws/",
    ".gnupg/",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".docker/config",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".env",
    "/etc/",
    ".git/hooks/",
    "/dev/tcp/",
    "/dev/udp/",
    "/dev/sd",
    "/dev/disk",
    "/dev/rdisk",
    "crontab",
    "/cron.d/",
    "/sudoers",
    "launchagents/",
    "launchdaemons/",
    ".kube/config",
    ".gitconfig",
    ".local/bin",
    ".git/config",
    ".config/git/config",
    ".config/fish/config.fish",
    ".bash_aliases",
    ".bash_logout",
    ".inputrc",
    ".cshrc",
    ".tcshrc",
    ".kshrc",
    ".zlogin",
    ".zlogout",
    ".bash_completion",
    ".config/fish/",
    ".config/nushell/",
    ".config/elvish/",
    ".config/powershell/",
    ".vimrc",
    ".config/nvim/",
    ".emacs.d/",
    ".gdbinit",
    ".lldbinit",
    ".psqlrc",
    ".wgetrc",
    ".curlrc",
    ".my.cnf",
    ".s3cfg",
    ".hgrc",
    ".subversion/",
    ".rprofile",
    ".renviron",
    ".cargo/credentials",
    ".cargo/config",
    ".config/gh/",
    ".config/gcloud/",
    ".config/rclone/",
    ".config/git/credentials",
    ".terraform.d/",
    ".config/op/",
    ".azure/",
    ".config/containers/",
    ".pgpass",
    ".authinfo",
    ".mozilla/",
    "keychains/",
    "login data",
    ".config/systemd/",
    ".config/autostart/",
    "/var/at/",
    "/var/spool/cron",
    "/environ",
    "proc/self/mem",
    "proc/self/maps",
    "proc/kcore",
    ".pem",
];

const ENV_TEMPLATE_SUFFIXES: &[&str] = &[
    ".example",
    ".sample",
    ".template",
    ".dist",
    ".defaults",
    ".tpl",
];

fn hits_sensitive(token: &str) -> bool {
    let raw = token.to_lowercase();
    let norm = normpath(token).to_lowercase();
    for &marker in SENSITIVE_TARGET_MARKERS {
        if raw.contains(marker) || norm.contains(marker) {
            if marker == ".env" && ENV_TEMPLATE_SUFFIXES.iter().any(|suf| raw.ends_with(*suf)) {
                continue;
            }
            return true;
        }
    }
    false
}

// --- program classification ------------------------------------------------

const INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "rbash",
    "zsh",
    "dash",
    "ksh",
    "mksh",
    "lksh",
    "pdksh",
    "ksh88",
    "ksh93",
    "ksh2020",
    "rksh",
    "oksh",
    "loksh",
    "ash",
    "posh",
    "hush",
    "sash",
    "jsh",
    "csh",
    "tcsh",
    "fish",
    "fizsh",
    "pwsh",
    "powershell",
    "nu",
    "xonsh",
    "elvish",
    "yash",
    "oil",
    "osh",
    "ysh",
    "mrsh",
    "brush",
    "ion",
    "murex",
    "ngs",
    "closh",
    "scsh",
    "rc",
    "es",
    "eval",
    "source",
    ".",
];

const EXACT_ONLY_PROGRAMS: &[&str] = &[
    "env",
    "xargs",
    "timeout",
    "nice",
    "nohup",
    "setsid",
    "stdbuf",
    "time",
    "ionice",
    "chrt",
    "command",
    "builtin",
    "exec",
    "watch",
    "parallel",
    "flock",
    "unshare",
    "script",
    "runuser",
    "capsh",
    "proot",
    "fakeroot",
    "firejail",
    "bwrap",
    "setarch",
    "arch",
    "taskset",
    "numactl",
    "prlimit",
    "setpriv",
    "gosu",
    "eatmydata",
    "run-parts",
    "expect",
    "ssh-agent",
    "gdbserver",
    "wine",
    "wine64",
    "box64",
    "box86",
    "qemu",
    "caffeinate",
    "toybox",
    "systemd-run",
    "systemd-inhibit",
    "dbus-run-session",
    "dbus-launch",
    "at",
    "batch",
    "watchexec",
    "entr",
    "nodemon",
    "foreman",
    "mise",
    "direnv",
    "pyenv",
    "rbenv",
    "nodenv",
    "volta",
    "fnm",
    "asdf",
    "npx",
    "uvx",
    "pipx",
    "bunx",
    "limactl",
    "incus",
    "lxc",
    "apptainer",
    "singularity",
    "oc",
    "crictl",
    "nerdctl",
    "ctr",
    "nomad",
    "helm",
    "ansible",
    "ansible-playbook",
    "salt",
    "salt-call",
    "terraform",
    "pulumi",
    "tmux",
    "screen",
    "byobu",
    "zellij",
    "dtach",
    "abduco",
    "sudo",
    "doas",
    "su",
    "pkexec",
    "vim",
    "vi",
    "nvim",
    "emacs",
    "ex",
    "view",
    "less",
    "more",
    "man",
    "nano",
    "pico",
    "ed",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "telnet",
    "nc",
    "ncat",
    "netcat",
    "socat",
    "curl",
    "curlie",
    "xh",
    "wget",
    "wget2",
    "ftp",
    "tftp",
    "aria2c",
    "lynx",
    "links",
    "w3m",
    "http",
    "https",
    "httpie",
    "httpx",
    "websocat",
    "grpcurl",
    "croc",
    "wormhole",
    "kcat",
    "kafkacat",
    "mosh",
    "ssh-copy-id",
    "sshpass",
    "docker",
    "kubectl",
    "podman",
    "nsenter",
    "chroot",
    "dig",
    "nslookup",
    "host",
    "drill",
    "openssl",
    "gpg",
    "gpg2",
    "aws",
    "gcloud",
    "az",
    "rclone",
    "mc",
    "s3cmd",
    "gsutil",
    "azcopy",
    "restic",
    "gh",
    "op",
    "vault",
    "aws-vault",
    "sops",
    "vercel",
    "doppler",
    "infisical",
    "gdb",
    "lldb",
    "strace",
    "ltrace",
    "dtrace",
    "dtruss",
    "perf",
    "valgrind",
    "bpftrace",
    "rr",
    "osascript",
    "jrunscript",
    "busybox",
    "ssh-keygen",
    "sqlite3",
    "psql",
    "pgcli",
    "mysql",
    "mysqladmin",
    "mariadb",
    "mycli",
    "litecli",
    "mongo",
    "mongosh",
    "mongoexport",
    "mongodump",
    "redis-cli",
    "duckdb",
    "clickhouse-client",
    "cockroach",
    "cqlsh",
    "influx",
    "usql",
    "dropdb",
    "createdb",
    "vacuumdb",
    "pg_dump",
    "pg_restore",
    "noglob",
    "nocorrect",
    "nofork",
];

fn is_exact_only(program: &str) -> bool {
    let b = basename(program);
    if EXACT_ONLY_PROGRAMS.contains(&b) {
        return true;
    }
    // The dynamic loader runs an arbitrary binary; qemu user-mode runs a following program. Both
    // have version/arch-suffixed basenames, so match by prefix.
    b.starts_with("ld-linux")
        || b.starts_with("ld-musl")
        || b.starts_with("qemu-")
        || b == "ld.so"
        || b == "ld-elf.so.1"
}

/// Package managers that run code the package author wrote — `setup.py`,
/// `postinstall`, a formula — as an ordinary part of installing.
///
/// Build tools that run code the *project* declares (`cargo`, `make`,
/// `cmake`, `mvn`) are deliberately not here; that is a wider question than
/// the one this list answers.
const PACKAGE_INSTALLER_PROGRAMS: &[&str] = &[
    "pip",
    "pip3",
    "pipenv",
    "poetry",
    "pdm",
    "hatch",
    "rye",
    "uv",
    "conda",
    "mamba",
    "micromamba",
    "npm",
    "pnpm",
    "yarn",
    "gem",
    "bundle",
    "bundler",
    "composer",
    "cabal",
    "stack",
    "brew",
    "port",
    "apt",
    "apt-get",
    "aptitude",
    "dnf",
    "yum",
    "zypper",
    "pacman",
    "apk",
];

/// Whether running `program` means running code chosen by its arguments or
/// fetched from a registry, rather than by the program itself.
///
/// `python script.py` and `pip install x` are only as safe as whatever wrote
/// the script or published the package. Naming such a program in a rule is a
/// real decision about it; a blanket "allow every command" is not.
fn runs_supplied_code(program: &str) -> bool {
    let b = basename(program);
    let normalized = normalize_runtime(b);
    SCRIPT_EXECUTOR_PROGRAMS.contains(&normalized.as_str())
        || SCRIPT_EXECUTOR_PROGRAMS.contains(&b)
        || PACKAGE_INSTALLER_PROGRAMS.contains(&normalized.as_str())
        || PACKAGE_INSTALLER_PROGRAMS.contains(&b)
}

/// Whether a rule of this breadth may cover `program` at all.
///
/// Breadth is not one axis. A wrapper (`is_exact_only`) has to be named token
/// for token, because its arguments are a different command and a prefix
/// grant for `timeout` would carry every program it can launch. A script
/// executor or package installer may be named by the program — writing
/// `python` in an allowlist says something about `python` — but is never
/// reached by a blanket `all`, which names nothing and so cannot have meant
/// "run whatever the agent just wrote".
fn rule_may_cover(kind: CommandRuleKind, program: &str) -> bool {
    match kind {
        CommandRuleKind::Exact => true,
        CommandRuleKind::Prefix => !is_exact_only(program),
        CommandRuleKind::All => !is_exact_only(program) && !runs_supplied_code(program),
    }
}

/// Every leaf command's `argv`, or `None` when the command cannot be read
/// exactly.
///
/// `None` is not "no commands" — it is "we will not say", and it is returned
/// for exactly the inputs [`analyze_shell_command`] refuses to reason about:
/// a parse error, an unsupported construct, a dynamically-resolved program
/// word, or a substitution the text contains but the grammar did not surface.
/// A caller building a grant from this must treat `None` as "offer nothing
/// narrower than the whole tool".
#[must_use]
pub fn simple_command_argvs(command: &str) -> Option<Vec<Vec<String>>> {
    if command.trim().is_empty() || has_obfuscated_construct(command) {
        return None;
    }
    let program = parse_program(command).ok()?;
    let mut acc = Collected::default();
    collect_program(&program, &mut acc).ok()?;
    if count_substitution_markers(command) > acc.surfaced_substitutions {
        return None;
    }
    if acc.simples.is_empty() {
        return None;
    }
    Some(acc.simples.into_iter().map(|simple| simple.argv).collect())
}

/// The program plus its leading non-flag operands: the subcommand chain.
///
/// `npm run test --silent` yields `npm run test`, and `git commit -m x`
/// yields `git commit`. Stopping at the first flag is what keeps the rung
/// meaningful — a grant for `git commit` should not also cover
/// `git commit --amend --no-verify` only because the flags happened to sort
/// that way.
fn subcommand_prefix(argv: &[String]) -> Vec<String> {
    let mut prefix = Vec::with_capacity(argv.len());
    prefix.push(argv[0].clone());
    for token in &argv[1..] {
        if token.starts_with('-') {
            break;
        }
        prefix.push(token.clone());
    }
    prefix
}

/// The grant rungs worth offering for one `argv`, narrowest first.
///
/// The argv analogue of [`suggested_rungs`], and verified the same way: a
/// rung is offered only when granting exactly that rule would in fact stop
/// the asking, so a wrapper yields no prefix rung and an interpreter yields
/// none at all.
#[must_use]
pub fn suggested_rungs_for_argv(argv: &[String]) -> Vec<CommandRule> {
    if argv.is_empty() {
        return Vec::new();
    }
    let mut candidates: Vec<CommandRule> = Vec::new();
    candidates.extend(CommandRule::new(CommandRuleKind::Exact, argv.to_vec()));
    candidates.extend(CommandRule::new(
        CommandRuleKind::Prefix,
        subcommand_prefix(argv),
    ));
    candidates.extend(CommandRule::new(
        CommandRuleKind::Prefix,
        vec![argv[0].clone()],
    ));
    candidates.extend(CommandRule::new(CommandRuleKind::All, Vec::new()));

    let mut rungs: Vec<CommandRule> = Vec::new();
    for rule in candidates {
        if rungs.contains(&rule) {
            continue;
        }
        let ruleset = ShellRuleSet {
            allow: vec![rule.clone()],
            deny: Vec::new(),
        };
        if analyze_argv(argv, &ruleset).verdict == ShellVerdict::Allow {
            rungs.push(rule);
        }
    }
    rungs
}

/// The grant rungs worth offering for `command`, narrowest first.
///
/// Every candidate is **verified** rather than assumed: a rung is offered
/// only if granting exactly that rule would actually let this command run
/// without asking. That is what keeps the ladder honest about restricted
/// programs — `timeout 5 sh -c id` yields no prefix rung at all, because a
/// prefix grant would not have covered it, and offering one would promise
/// something the analyzer then refuses.
///
/// A compound or unreadable command gets at most the widest rung, since
/// there is no single `argv` to name.
#[must_use]
pub fn suggested_rungs(command: &str) -> Vec<CommandRule> {
    let mut candidates: Vec<CommandRule> = Vec::new();
    if let Some(argvs) = simple_command_argvs(command) {
        if let [argv] = argvs.as_slice() {
            candidates.extend(CommandRule::new(CommandRuleKind::Exact, argv.clone()));
            candidates.extend(CommandRule::new(
                CommandRuleKind::Prefix,
                subcommand_prefix(argv),
            ));
            candidates.extend(CommandRule::new(
                CommandRuleKind::Prefix,
                vec![argv[0].clone()],
            ));
        }
    }
    candidates.extend(CommandRule::new(CommandRuleKind::All, Vec::new()));

    let mut rungs: Vec<CommandRule> = Vec::new();
    for rule in candidates {
        if rungs.contains(&rule) {
            continue;
        }
        let ruleset = ShellRuleSet {
            allow: vec![rule.clone()],
            deny: Vec::new(),
        };
        if analyze_shell_command(command, &ruleset).verdict == ShellVerdict::Allow {
            rungs.push(rule);
        }
    }
    rungs
}

#[cfg(test)]
mod tests;
