//! Closed renderer projections of what a tool will do, and what it did.
//!
//! The renderer boundary carries no tool arguments and no tool output. These
//! types are the deliberate exceptions: a tool may opt in to showing a human
//! the action under review and the result it produced, field by field.
//!
//! They are not passthroughs. Each variant enumerates exactly what a person
//! needs in order to consent to an action or to understand its outcome, values
//! are clamped, and a tool without a variant projects nothing.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::tool::ToolOutput;

/// Longest command, argument, or directory string an action preview carries.
pub const MAX_ACTION_FIELD_CHARS: usize = 512;

/// Most arguments an action preview carries before it elides the tail.
pub const MAX_ACTION_ARGS: usize = 32;

/// Longest captured stream a result preview carries. The execution provider
/// already bounds what it captures; this bounds what crosses the boundary.
pub const MAX_RESULT_STREAM_CHARS: usize = 40_000;

/// Longest `ui://` view reference a result preview carries. Mirrors the
/// discovery-time bound in `openwave-mcp`; a reference is carried whole or not
/// at all, because a truncated URI is a different URI.
pub const MAX_RESULT_VIEW_URI_CHARS: usize = 2 * 1024;

/// Longest label, detail, or meta string one listed result row carries.
pub const MAX_RESULT_ENTRY_CHARS: usize = 200;

/// Most rows a result preview lists before it counts the rest instead.
pub const MAX_RESULT_ENTRIES: usize = 50;

/// The action a call will take, in a form a human can inspect.
///
/// Approval cards need this because consent to an action you cannot see is not
/// consent. Result cards reuse it so the same action is described the same way
/// before and after it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolActionPreview {
    /// A command execution, as the argument vector it will actually run. There
    /// is no shell string to show because no shell parses it.
    Exec {
        /// Executable name or path the model chose.
        command: String,
        /// Arguments passed directly to the executable.
        args: Vec<String>,
        /// Working directory relative to the chat's private scratch, never a
        /// host path.
        cwd: String,
        /// Scratch-relative files and directories staged into the sandbox
        /// before the command runs, empty when the model staged none.
        ///
        /// Part of the action, not incidental setup: this list is what the
        /// command can read, so a person approving `python3 analyze.py` is
        /// entitled to see which of their files it is being handed. Omitting
        /// it also made two calls that stage different documents project
        /// identically, which is what `describes_exactly` promises they do
        /// not.
        ///
        /// `default` because grants and approval rows retained before the
        /// field existed carry no staging list; they read back as staging
        /// nothing, which is narrower than what they were given for and so
        /// sends a call that stages anything back to the person.
        #[serde(default)]
        files: Vec<String>,
    },
    /// A search of this conversation's own sources. The query is the whole
    /// action, and it is what the excerpts returned will be chosen to match.
    Search { query: String },
    /// A public web search. What leaves the device is the entire reason this
    /// call asks first, so the card has to be able to show all of it — the
    /// filters are told to the provider too, not just the query.
    WebSearch {
        query: String,
        /// Sites the search is confined to, empty when the model named none.
        domains: Vec<String>,
        /// Earliest publication date the search will accept, as the model
        /// wrote it. Kept verbatim rather than reformatted: the card's job is
        /// to show what the provider is actually told.
        start_published_at: Option<String>,
        /// Latest publication date the search will accept.
        end_published_at: Option<String>,
    },
    /// A single public web page fetch. The URL is the entire action — it is
    /// both what leaves the device and where the request goes — so the card
    /// shows it whole.
    WebExtract { url: String },
    /// A file write inside the chat's private workspace. The path is the
    /// resource under review — which file the call will create or replace —
    /// and deliberately the only field: the content is not projected, so the
    /// card names the target without carrying a document across the boundary.
    WriteFile {
        /// Workspace-relative destination path, never a host path.
        path: String,
    },
}

impl ToolActionPreview {
    /// Project the action for a call, or `None` when the tool has none.
    ///
    /// `arguments` are the canonical model-authored arguments. Only the fields
    /// named by a variant are read; malformed arguments yield `None` rather
    /// than degrading the card into a raw JSON dump.
    #[must_use]
    pub fn build(tool_name: &str, arguments: &Value) -> Option<Self> {
        match tool_name {
            // Kept beside `ToolApprovalKind::for_tool_name`, which already owns
            // the closed mapping from tool name to consent semantics.
            "exec" => {
                let command = clamp(arguments.get("command")?.as_str()?, MAX_ACTION_FIELD_CHARS)?;
                let args = clamped_list(arguments.get("args"));
                let cwd = clamped_field(arguments.get("cwd")).unwrap_or_else(|| ".".into());
                let files = clamped_list(arguments.get("files"));
                Some(Self::Exec {
                    command,
                    args,
                    cwd,
                    files,
                })
            }
            // Approving a search without seeing its query is not consent to
            // anything in particular, and for `web_search` that query is the
            // thing that leaves the machine. Trimmed, because the tool trims
            // before searching and a card should show what actually goes out.
            "search" => Some(Self::Search {
                query: search_query(arguments)?,
            }),
            "web_search" => Some(Self::WebSearch {
                query: search_query(arguments)?,
                domains: clamped_list(arguments.get("domains")),
                start_published_at: clamped_field(arguments.get("start_published_at")),
                end_published_at: clamped_field(arguments.get("end_published_at")),
            }),
            // Approving a page fetch without seeing the URL is not consent to
            // fetching anything in particular. Trimmed like the queries above,
            // because the tool trims before admitting the URL.
            "web_extract" => Some(Self::WebExtract {
                url: clamp(
                    arguments.get("url")?.as_str()?.trim(),
                    MAX_ACTION_FIELD_CHARS,
                )?,
            }),
            // A workspace write gated in Ask mode names the file it will touch.
            // The content stays behind the boundary: the resource is the path,
            // and the card's job is to say where the write lands, not to
            // preview a document.
            "write_file" => Some(Self::WriteFile {
                path: clamp(arguments.get("path")?.as_str()?, MAX_ACTION_FIELD_CHARS)?,
            }),
            _ => None,
        }
    }

    /// Whether [`Self::build`] would reproduce `arguments` without losing
    /// anything.
    ///
    /// The preview is clamped so a card stays bounded: long fields are cut,
    /// control characters removed, and surplus or unreadable arguments dropped.
    /// That is right for showing a human what a call does and wrong for
    /// deciding whether a *later* call is the same one, because the clamp is
    /// many-to-one — a 600-character command and that command with anything
    /// appended project to the same 512 characters.
    ///
    /// A scope narrower than the whole tool may only be created from, or
    /// matched against, a call this returns `true` for. Anything else has to
    /// keep asking.
    #[must_use]
    pub fn describes_exactly(tool_name: &str, arguments: &Value) -> bool {
        match tool_name {
            "exec" => {
                arguments
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(survives_clamp)
                    // A dropped or elided argument silently changes the call's
                    // arity, and an absent `cwd` is faithfully the default the
                    // preview shows.
                    && list_survives_clamp(arguments.get("args"))
                    && field_survives_clamp(arguments.get("cwd"))
                    // The staging list is part of the action for the same
                    // reason the argument vector is: it decides what the
                    // command can read. A dropped or elided entry would let a
                    // grant given for one set of documents cover another.
                    && list_survives_clamp(arguments.get("files"))
            }
            // A search's action is its query, so a grant may name that query.
            // `k` is not part of it: it bounds how many passages come back and
            // changes nothing about what leaves the machine.
            "search" => query_survives_clamp(arguments),
            // `max_results` bounds the response the same way, but the domain
            // and date filters are told to the provider alongside the query.
            // They are on the card for that reason, and a grant may only be
            // built from a call the card showed in full.
            "web_search" => {
                query_survives_clamp(arguments)
                    && list_survives_clamp(arguments.get("domains"))
                    && field_survives_clamp(arguments.get("start_published_at"))
                    && field_survives_clamp(arguments.get("end_published_at"))
            }
            // A page fetch's action is its URL, all of it: a grant keyed on a
            // clamped URL would cover other URLs sharing its first 512
            // characters.
            "web_extract" => arguments
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|url| survives_clamp(url.trim())),
            // Deliberately not exact: the preview names the path and omits the
            // content, so it can never reproduce the call. A workspace write is
            // one-shot anyway (its standing "yes" is the chat's Auto mode), and
            // keeping it inexact means no narrow grant or judge candidacy can
            // ever be built from a projection that lost the document.
            "write_file" => false,
            // A tool with no variant projects nothing, so there is nothing a
            // narrower scope could name — and claiming otherwise would invite a
            // narrow grant with nothing to narrow to.
            _ => false,
        }
    }

    /// Whether the call's *place* — the workspace path a location-scoped grant
    /// is built from and matched against — reaches the preview intact.
    ///
    /// Weaker than [`Self::describes_exactly`] on purpose: a workspace write
    /// never describes itself exactly (the document is not projected), but a
    /// grant about a place does not need the document. It needs the path, all
    /// of it, so a grant keyed on a clamped path cannot be stretched over
    /// other paths sharing its first [`MAX_ACTION_FIELD_CHARS`] characters.
    #[must_use]
    pub fn names_place_exactly(tool_name: &str, arguments: &Value) -> bool {
        match tool_name {
            "write_file" => arguments
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(survives_clamp),
            _ => false,
        }
    }
}

/// What one row of a listed result is, which is what picks its icon.
///
/// A closed vocabulary rather than an icon name: the renderer chooses how to
/// draw a folder, and a tool must not be able to name a glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ResultEntryKind {
    /// A file, in the chat's scratch or in a connected folder.
    File,
    /// A directory.
    Folder,
    /// A document added to this conversation as a source.
    Source,
    /// A passage matched inside a source.
    Passage,
    /// A page on the public web.
    Link,
    /// An output this conversation published.
    Output,
    /// A local app this profile published.
    App,
}

/// One thing a call surfaced.
///
/// Three fields because a row reads as three: what it is, where it came from,
/// and how big or how many. A tool that wants to say more than that is
/// describing something this projection does not cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResultEntry {
    pub kind: ResultEntryKind,
    /// The row's name — a file name, a source title, a page title.
    pub label: String,
    /// A secondary hint beside the name — a path, a domain, a section.
    pub detail: Option<String>,
    /// Trailing meta — a size, a count, a status word.
    pub meta: Option<String>,
    /// The document's media type, when the row is a document with one.
    ///
    /// Data rather than display text: the renderer maps it to a type-specific
    /// icon (a PDF mark, a spreadsheet mark) and never prints it. Still not a
    /// glyph name — the vocabulary is media types, and the renderer keeps its
    /// own closed mapping with a generic fallback. `default` because retained
    /// projections predate the field.
    #[serde(default)]
    pub media_type: Option<String>,
    /// The durable record this row names, when the renderer can open one.
    ///
    /// Data rather than display text: the renderer routes a click through its
    /// own panel navigation and never prints the id, and [`ResultEntryKind`]
    /// names where that click goes — an [`Output`] row opens the outputs
    /// panel, an [`App`] row the apps library. A kind with nowhere to go
    /// leaves this `None`.
    ///
    /// Aliased and `default` because retained projections wrote it as
    /// `output_id`, when a published output was the only place a row could
    /// point.
    ///
    /// [`Output`]: ResultEntryKind::Output
    /// [`App`]: ResultEntryKind::App
    #[serde(default, alias = "output_id")]
    pub target_id: Option<String>,
}

impl ResultEntry {
    /// A row that is only its name.
    #[must_use]
    pub fn new(kind: ResultEntryKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            detail: None,
            meta: None,
            media_type: None,
            target_id: None,
        }
    }

    /// Add the secondary hint beside the name.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Add the trailing meta.
    #[must_use]
    pub fn with_meta(mut self, meta: impl Into<String>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    /// Name the document's media type.
    #[must_use]
    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    /// Name the durable record this row points at, which its kind routes.
    #[must_use]
    pub fn with_target_id(mut self, target_id: impl Into<String>) -> Self {
        self.target_id = Some(target_id.into());
        self
    }
}

/// One thing a call could not do.
///
/// A batch tool succeeds and fails in the same breath — five files import, two
/// do not — and a card that lists only what worked is not reporting, it is
/// flattering. Every one of Brightwave's local-file results carries a parallel
/// failures list for exactly this reason.
///
/// Two fields because that is what a failure row reads as, and it is what
/// Brightwave's own card normalizes its three failure shapes down to before
/// rendering them: the thing that failed, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResultFailure {
    /// What failed, when the tool can name it. `None` when the tool cannot —
    /// a folder it could not even read the name of — and the row then leads
    /// with a generic noun rather than being dropped. A failure the reader
    /// never sees is worse than one it cannot fully name.
    pub label: Option<String>,
    /// Why it failed, in the tool's own words.
    ///
    /// This is tool-authored text, and it crosses on the same terms as a
    /// command's stderr already does: what the boundary keeps out is model- and
    /// provider-authored text and private diagnostics, and this is a message
    /// our own tool wrote for a person to act on. Clamped like every other
    /// field; a failure nobody can read is not a report.
    ///
    /// "Our own tool wrote" is the load-bearing part, and it is a habit rather
    /// than something the type can enforce. Write the sentence — "file is not
    /// valid UTF-8" — instead of forwarding a `std::io::Error`, a broker
    /// failure, or any other error whose `Display` you do not control. Those
    /// interpolate whatever they were handed, which is how a host path ends up
    /// on a card nobody meant to put it on.
    pub error: String,
}

impl ResultFailure {
    /// A failure the tool can name.
    #[must_use]
    pub fn new(label: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            error: error.into(),
        }
    }

    /// A failure the tool cannot name.
    #[must_use]
    pub fn unnamed(error: impl Into<String>) -> Self {
        Self {
            label: None,
            error: error.into(),
        }
    }
}

/// A byte count as a card should read it.
///
/// Ported from Brightwave's local-file result rows, which is where this reads
/// as "12.4 KB" rather than as a number nobody scans.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    // One decimal only where it says something: "1.4 MB" is worth the digit,
    // "512.0 KB" and "340.0 MB" are not.
    let unit = UNITS[unit];
    if value >= 10.0 || (value.fract() * 10.0).round() == 0.0 {
        format!("{} {unit}", value.round())
    } else {
        format!("{value:.1} {unit}")
    }
}

/// A way the execution backend ran with less than its intended setup.
///
/// Closed, and deliberately coarse: what a reader needs is what happened and
/// what it costs them, not which vendor API returned which status. A provider
/// that degrades a second way earns a second variant here rather than a free
/// string, because the sentence the card shows is written on this side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecDegradation {
    /// The prepared OpenWave sandbox image could not be used, so commands run
    /// on the backend's stock image and document tooling installs its
    /// dependencies inside the sandbox on every run.
    SandboxImageUnavailable,
}

/// The execution backend that ran a command, as a closed vocabulary.
///
/// Read through this enum rather than surfaced as a string, on the same terms
/// as [`ExecDegradation`]: the card names the backend, and the card's words
/// are written on this side. A backend the renderer does not know projects as
/// nothing rather than as passthrough text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecBackend {
    /// The host's native process sandbox.
    Local,
    /// A managed E2B cloud sandbox.
    E2b,
    /// A managed Daytona cloud sandbox.
    Daytona,
}

/// What a call produced, in a form a human can read.
///
/// A command's output is the whole reason to run it. Withholding it leaves the
/// transcript asserting that something happened without ever showing what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolResultPreview {
    Exec {
        /// Process exit status, or `None` when it was killed by a signal.
        exit_code: Option<i32>,
        /// Whether the provider stopped the command at its time limit.
        timed_out: bool,
        /// Whether the provider dropped output past its capture limit.
        output_truncated: bool,
        stdout: String,
        stderr: String,
        /// Preview images emitted by the command, in model-facing priority order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<crate::ImageRef>,
        /// Durable outputs the command's `output/` files created or updated.
        /// Files that still match their published version are not news and are
        /// not listed. Defaulted so exec rows persisted before the field
        /// existed read back unchanged.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        outputs: Vec<ResultEntry>,
        /// How the execution backend fell short of its intended setup, when it
        /// did. Reported on the first execution that degrades and not on the
        /// ones after it, so a chat says this once rather than on every card.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        degraded: Option<ExecDegradation>,
        /// Which backend ran the command. Defaulted so exec rows persisted
        /// before the field existed read back unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        backend: Option<ExecBackend>,
    },
    /// Web search is available after the reader chooses and configures a
    /// provider. Carries no model- or provider-authored text.
    WebSearchProviderRequired,
    /// An external MCP tool that declared an MCP Apps view for its results.
    ///
    /// This is a *reference*, never markup: the renderer asks the server for
    /// the document by these coordinates and renders it only inside its
    /// sandboxed frame. `server` is the user-configured namespace already
    /// shown in Settings; `resource_uri` passed discovery-time validation.
    /// Remote tool names, descriptions, and raw output still never cross.
    McpApp {
        /// The configured MCP server namespace that serves the view.
        server: String,
        /// The validated `ui://` document reference.
        resource_uri: String,
    },
    /// What a call found, read, or wrote, as the list of things it was.
    ///
    /// One shape for every tool whose result is a list, because what
    /// distinguishes those tools on screen — the icon, the headline, the verb —
    /// already comes from the tool's name. Forty tools do not need forty
    /// variants to say "here is what I found".
    Entries {
        entries: Vec<ResultEntry>,
        /// What the same call could not do. Bounded and counted on the same
        /// terms as `entries`, and elided into the same tally: the card's job
        /// is to be honest about how much it is not showing, and a hidden
        /// failure is the worst thing to hide.
        failures: Vec<ResultFailure>,
        /// Rows past [`MAX_RESULT_ENTRIES`], counted rather than shown. A card
        /// that silently lists the first fifty of two hundred results is
        /// telling the reader something false.
        elided: u32,
    },
}

/// The tools that may project a list of entries.
///
/// Still an allowlist, on the same terms as every other variant: `entries` in a
/// tool's output data is an opt-in, and a tool not named here does not have
/// one. An external MCP tool in particular can write whatever it likes into its
/// data, and must not thereby acquire a card.
const ENTRY_TOOLS: &[&str] = &[
    "search",
    crate::WEB_SEARCH_TOOL,
    crate::WEB_EXTRACT_TOOL,
    "list_sources",
    "read_source",
    "read_file",
    "list_dir",
    "write_file",
    // Executed on the desktop, which authors their rows and posts them back.
    // The allowlist is checked against the *stored* call name rather than
    // anything the client asserts, so a client cannot grant itself a card for a
    // tool that has none.
    crate::LIST_CONNECTED_FOLDERS_TOOL,
    crate::LIST_FOLDER_TOOL,
    crate::READ_CONNECTED_FILE_TOOL,
    crate::IMPORT_CONNECTED_FILE_TOOL,
    crate::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
    // Lists the one app record the call created or revised: name, ordinal, and
    // the app id the card opens it by — never the bundle or the manifest's
    // bindings.
    crate::local_app::CREATE_APP_TOOL,
];

impl ToolResultPreview {
    /// Project the result of a call from the tool's own output.
    ///
    /// A tool opts in through an enumerated error category, by putting the
    /// enumerated fields in [`ToolOutput::data`], or — for an external MCP
    /// tool — by carrying the host-side [`ToolOutput::ui_view`] declaration.
    /// Everything else the output carries stays behind the boundary.
    #[must_use]
    pub fn build(tool_name: &str, output: &ToolOutput) -> Option<Self> {
        if output.is_error
            && output.error_category == Some(crate::ToolErrorCategory::ConfigurationRequired)
            && web_capability_tool(tool_name)
        {
            return Some(Self::WebSearchProviderRequired);
        }
        match tool_name {
            "exec" => {
                let data = output.data.as_ref()?;
                Some(Self::Exec {
                    exit_code: data
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .and_then(|code| i32::try_from(code).ok()),
                    timed_out: data
                        .get("timed_out")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    output_truncated: data
                        .get("output_truncated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    stdout: stream(data.get("stdout")),
                    stderr: stream(data.get("stderr")),
                    images: output.images.clone(),
                    outputs: exec_output_entries(data.get("outputs")),
                    // Read through the closed enum rather than as a string:
                    // a tool's data is not a place the card takes prose from.
                    degraded: data.get("degraded").and_then(|value| {
                        serde_json::from_value::<ExecDegradation>(value.clone()).ok()
                    }),
                    backend: data.get("provider").and_then(|value| {
                        serde_json::from_value::<ExecBackend>(value.clone()).ok()
                    }),
                })
            }
            // Only an external MCP proxy can carry a view declaration, and a
            // failed call has no result worth a rich surface.
            name if name.starts_with("mcp__") && !output.is_error => {
                let view = output.ui_view.as_ref()?;
                if !view.resource_uri.starts_with("ui://")
                    || view.resource_uri.chars().count() > MAX_RESULT_VIEW_URI_CHARS
                    || view.resource_uri.chars().any(char::is_control)
                    || !survives_clamp(&view.server)
                {
                    return None;
                }
                Some(Self::McpApp {
                    server: view.server.clone(),
                    resource_uri: view.resource_uri.clone(),
                })
            }
            // A failed call has no list to show — whatever partial data it left
            // behind describes work that did not happen.
            name if ENTRY_TOOLS.contains(&name) && !output.is_error => {
                let data = output.data.as_ref()?;
                let listed = data.get("entries")?.as_array()?;
                let entries: Vec<_> = listed
                    .iter()
                    .take(MAX_RESULT_ENTRIES)
                    .filter_map(result_entry)
                    .collect();
                // Failures are optional — most tools have none — but a tool
                // that reports them gets them bounded the same way.
                let reported = data
                    .get("failures")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice);
                let failures: Vec<_> = reported
                    .iter()
                    .take(MAX_RESULT_ENTRIES)
                    .filter_map(result_failure)
                    .collect();
                // Everything the call produced that this card is not showing,
                // whether it was cut at the limit or dropped by the clamp.
                // Both are rows the tool returned and the reader will not see,
                // which is the only thing `elided` is claiming.
                let elided = u32::try_from(
                    (listed.len() - entries.len()) + (reported.len() - failures.len()),
                )
                .unwrap_or(u32::MAX);
                // An empty list is a result, not the absence of one: a search
                // that matched nothing and a search that was never projected
                // look identical in the rail, and only one of them is true.
                Some(Self::Entries {
                    entries,
                    failures,
                    elided,
                })
            }
            _ => None,
        }
    }

    /// Rebuild a closed result from the stable failure code stored on a
    /// terminal tool-call row.
    ///
    /// Arbitrary result text stays behind the renderer boundary. Only an exact
    /// enumerated category for a known tool can recover a result here.
    #[must_use]
    pub fn from_stored_error(tool_name: &str, error_code: Option<&str>) -> Option<Self> {
        if error_code == Some(crate::ToolErrorCategory::ConfigurationRequired.as_str())
            && web_capability_tool(tool_name)
        {
            Some(Self::WebSearchProviderRequired)
        } else {
            None
        }
    }

    /// Whether this result has any captured output to show.
    #[must_use]
    pub fn has_output(&self) -> bool {
        match self {
            Self::Exec {
                stdout,
                stderr,
                images,
                ..
            } => !stdout.is_empty() || !stderr.is_empty() || !images.is_empty(),
            Self::WebSearchProviderRequired | Self::McpApp { .. } => true,
            // An empty list is still something to show, and saying so is the
            // point: the card reports that the call found nothing.
            Self::Entries { .. } => true,
        }
    }
}

/// The tools whose configuration-required failure means "web capability needs
/// a provider set up in Settings". Extraction shares web search's card: both
/// are repaired from the same web-search settings section, and the card
/// carries no tool-specific text.
fn web_capability_tool(tool_name: &str) -> bool {
    tool_name == crate::WEB_SEARCH_TOOL || tool_name == crate::WEB_EXTRACT_TOOL
}

/// Bound one listed row, or drop it.
///
/// A row without a readable label is not a row — it would render as an empty
/// line the reader cannot interpret. `detail` and `meta` are genuinely
/// optional, so one that clamps away to nothing simply goes missing rather than
/// taking its row with it.
/// Project the exec tool's published-output rows into bounded entries.
///
/// The tool's data lists every scanned file; only the ones that created or
/// updated a durable output are a result worth a row. Malformed rows are
/// dropped rather than degrading the whole preview, matching how listed
/// entries behave elsewhere.
fn exec_output_entries(value: Option<&Value>) -> Vec<ResultEntry> {
    let Some(rows) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let status = row.get("status")?.as_str()?;
            if status != "created" && status != "updated" {
                return None;
            }
            let label = clamp(row.get("filename")?.as_str()?, MAX_RESULT_ENTRY_CHARS)?;
            let version = row.get("version")?.as_u64()?;
            // The media type is derived from the filename by the same rules
            // output creation uses, so the transcript card and the outputs
            // panel show one file with one icon.
            let media_type = crate::deliverable::deliverable_media_type(&label)
                .unwrap_or_else(|| crate::deliverable::binary_media_type_for_extension(&label));
            let mut entry = ResultEntry::new(ResultEntryKind::Output, label)
                .with_meta(format!("v{version} · {status}"))
                .with_media_type(media_type);
            if let Some(output_id) = row.get("output_id").and_then(Value::as_str) {
                entry = entry.with_target_id(output_id);
            }
            Some(entry)
        })
        .take(MAX_RESULT_ENTRIES)
        .collect()
}

fn result_entry(value: &Value) -> Option<ResultEntry> {
    Some(ResultEntry {
        kind: serde_json::from_value(value.get("kind")?.clone()).ok()?,
        label: clamp(value.get("label")?.as_str()?, MAX_RESULT_ENTRY_CHARS)?,
        detail: entry_field(value.get("detail")),
        meta: entry_field(value.get("meta")),
        media_type: entry_field(value.get("media_type")),
        target_id: entry_field(value.get("target_id")),
    })
}

fn entry_field(value: Option<&Value>) -> Option<String> {
    clamp(value?.as_str()?, MAX_RESULT_ENTRY_CHARS)
}

/// Bound one failure row, or drop it.
///
/// The error is what makes the row worth showing, so a failure without a
/// readable one is dropped — and counted into `elided`, so the card still
/// reports that something went unshown. The label is genuinely optional and
/// clamps away on its own without taking the row with it.
fn result_failure(value: &Value) -> Option<ResultFailure> {
    Some(ResultFailure {
        label: entry_field(value.get("label")),
        error: clamp(value.get("error")?.as_str()?, MAX_RESULT_ENTRY_CHARS)?,
    })
}

/// Bound a captured stream, keeping its newlines: output without line breaks
/// is not output anyone can read.
fn stream(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(|text| {
            text.chars()
                .filter(|character| {
                    !character.is_control() || *character == '\n' || *character == '\t'
                })
                .take(MAX_RESULT_STREAM_CHARS)
                .collect()
        })
        .unwrap_or_default()
}

/// The query a search will actually run, as the card should show it.
fn search_query(arguments: &Value) -> Option<String> {
    clamp(
        arguments.get("query")?.as_str()?.trim(),
        MAX_ACTION_FIELD_CHARS,
    )
}

/// Bound one optional single-line preview field, dropping control characters
/// that could forge card structure. An absent, empty, or all-control field is
/// not presentable.
fn clamped_field(value: Option<&Value>) -> Option<String> {
    clamp(value?.as_str()?, MAX_ACTION_FIELD_CHARS)
}

/// Bound a list of single-line preview fields: surplus entries elided,
/// unreadable ones dropped.
fn clamped_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .take(MAX_ACTION_ARGS)
                .filter_map(|value| {
                    value
                        .as_str()
                        .and_then(|value| clamp(value, MAX_ACTION_FIELD_CHARS))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a query reaches the preview intact.
///
/// The trim is not a loss. Both search tools trim before searching, so padding
/// is not a difference between two calls; anything the clamp would actually
/// remove is.
fn query_survives_clamp(arguments: &Value) -> bool {
    arguments
        .get("query")
        .and_then(Value::as_str)
        .is_some_and(|query| survives_clamp(query.trim()))
}

/// Whether an optional single-line argument reaches the preview intact. An
/// absent one is faithfully the absence the preview shows.
fn field_survives_clamp(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => survives_clamp(text),
        Some(_) => false,
    }
}

/// Whether a list argument reaches the preview intact.
///
/// An absent list is faithfully the empty one; a present one must be an array
/// whose every element survives, since a dropped or elided element silently
/// changes what the call does.
fn list_survives_clamp(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(values)) => {
            values.len() <= MAX_ACTION_ARGS
                && values
                    .iter()
                    .all(|value| value.as_str().is_some_and(survives_clamp))
        }
        Some(_) => false,
    }
}

/// Whether [`clamp`] would return this string unchanged.
///
/// Deliberately mirrors `clamp`'s three lossy steps rather than calling it and
/// comparing, so the two cannot drift into disagreeing about what "unchanged"
/// means: nothing removed, nothing truncated, and nothing that clamps away to
/// nothing.
fn survives_clamp(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(char::is_control)
        && value.chars().count() <= MAX_ACTION_FIELD_CHARS
}

fn clamp(value: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `web_search` action with no filters, which is the common shape.
    fn plain_web_search(query: &str) -> ToolActionPreview {
        ToolActionPreview::WebSearch {
            query: query.into(),
            domains: Vec::new(),
            start_published_at: None,
            end_published_at: None,
        }
    }

    fn output_with_data(data: serde_json::Value) -> crate::ToolOutput {
        crate::ToolOutput::text("done").with_data(data)
    }

    #[test]
    fn a_search_shows_the_query_it_is_asking_permission_for() {
        // Approving `web_search` used to show nothing about the query, which is
        // the one thing that actually leaves the machine.
        assert_eq!(
            ToolActionPreview::build("web_search", &json!({ "query": "quarterly filings" })),
            Some(plain_web_search("quarterly filings"))
        );
        assert_eq!(
            ToolActionPreview::build("search", &json!({ "query": "revenue" })),
            Some(ToolActionPreview::Search {
                query: "revenue".into()
            })
        );
        // Trimmed, because the tool trims before searching.
        assert_eq!(
            ToolActionPreview::build("web_search", &json!({ "query": "  spaced  " })),
            Some(plain_web_search("spaced"))
        );
        // Extra arguments are not part of the action under review.
        assert_eq!(
            ToolActionPreview::build("search", &json!({ "query": "revenue", "k": 8 })),
            Some(ToolActionPreview::Search {
                query: "revenue".into()
            })
        );
        // A query the card cannot show is no preview at all, rather than a card
        // that describes an empty search.
        for arguments in [
            json!({}),
            json!({ "query": "" }),
            json!({ "query": "   " }),
            json!({ "query": 3 }),
        ] {
            assert_eq!(ToolActionPreview::build("search", &arguments), None);
        }
    }

    #[test]
    fn a_web_search_shows_the_filters_that_go_out_with_the_query() {
        // The consent copy promises the query "and its explicit filters", and
        // the filters reach the provider too — a card that showed only the
        // query was describing part of the action it asked about.
        assert_eq!(
            ToolActionPreview::build(
                "web_search",
                &json!({
                    "query": "quarterly filings",
                    "max_results": 10,
                    "domains": ["sec.gov", "ft.com"],
                    "start_published_at": "2024-01-01T00:00:00Z",
                }),
            ),
            Some(ToolActionPreview::WebSearch {
                query: "quarterly filings".into(),
                domains: vec!["sec.gov".into(), "ft.com".into()],
                start_published_at: Some("2024-01-01T00:00:00Z".into()),
                end_published_at: None,
            })
        );
    }

    #[test]
    fn an_action_is_exactly_describable_only_when_the_card_showed_all_of_it() {
        assert!(ToolActionPreview::describes_exactly(
            "exec",
            &json!({ "command": "cargo", "args": ["test"] })
        ));
        // A search names its query exactly, which is what lets an approval
        // offer a grant narrower than every search in the chat.
        for tool in ["search", "web_search"] {
            assert!(ToolActionPreview::describes_exactly(
                tool,
                &json!({ "query": "anything" })
            ));
        }
        // Result counts bound the response, not the disclosure, so they are not
        // part of the action a grant names.
        assert!(ToolActionPreview::describes_exactly(
            "search",
            &json!({ "query": "anything", "k": 8 })
        ));
        assert!(ToolActionPreview::describes_exactly(
            "web_search",
            &json!({ "query": "anything", "max_results": 10 })
        ));
        // Filters are disclosure, and the card carries them, so a filtered
        // search is exact too.
        assert!(ToolActionPreview::describes_exactly(
            "web_search",
            &json!({ "query": "anything", "domains": ["sec.gov"] })
        ));
        // A filter the card could not show is a filter nobody consented to.
        for arguments in [
            json!({ "query": "anything", "domains": ["se\u{0}c.gov"] }),
            json!({ "query": "anything", "domains": [7] }),
            json!({ "query": "anything", "domains": "sec.gov" }),
            json!({ "query": "anything", "end_published_at": 20_240_101 }),
            json!({ "query": "any\u{0}thing" }),
        ] {
            assert!(!ToolActionPreview::describes_exactly(
                "web_search",
                &arguments
            ));
        }
        // A tool with no variant has nothing to be exact about.
        assert!(!ToolActionPreview::describes_exactly(
            "mcp__server__tool",
            &json!({ "query": "anything" })
        ));
    }

    #[test]
    fn exec_action_carries_the_argument_vector_and_defaults_its_directory() {
        assert_eq!(
            ToolActionPreview::build(
                "exec",
                &serde_json::json!({ "command": "python3", "args": ["-c", "print(1)"] }),
            ),
            Some(ToolActionPreview::Exec {
                command: "python3".into(),
                args: vec!["-c".into(), "print(1)".into()],
                cwd: ".".into(),
                files: Vec::new(),
            })
        );
    }

    /// The exec card lists the durable outputs the command published. Only
    /// created/updated files are rows — an unchanged file is not news — and a
    /// stored exec preview from before the field existed reads back unchanged.
    #[test]
    fn exec_result_lists_published_outputs_and_old_rows_read_back() {
        let output = output_with_data(serde_json::json!({
            "exit_code": 0,
            "outputs": [
                { "filename": "report.md", "output_id": "output-report", "version": 3, "status": "updated" },
                { "filename": "chart.png", "version": 1, "status": "created" },
                { "filename": "data.csv", "version": 2, "status": "unchanged" },
                { "filename": 7, "version": "x", "status": "created" },
            ],
        }));
        let Some(ToolResultPreview::Exec { outputs, .. }) =
            ToolResultPreview::build("exec", &output)
        else {
            panic!("exec projects a result");
        };
        assert_eq!(
            outputs
                .iter()
                .map(|entry| (entry.label.as_str(), entry.meta.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("report.md", Some("v3 · updated")),
                ("chart.png", Some("v1 · created")),
            ]
        );
        assert!(outputs
            .iter()
            .all(|entry| entry.kind == ResultEntryKind::Output));
        // The routable id crosses when the scan recorded one — that is what
        // makes the transcript card clickable — and the icon's media type is
        // derived from the filename by the output-creation rules. A row
        // without an id (older journal data) still renders, unrouted.
        assert_eq!(outputs[0].target_id.as_deref(), Some("output-report"));
        assert_eq!(outputs[0].media_type.as_deref(), Some("text/markdown"));
        assert_eq!(outputs[1].target_id, None);
        assert_eq!(outputs[1].media_type.as_deref(), Some("image/png"));

        // A pre-outputs journal row deserializes with the field defaulted, and
        // a row journaled while the target id was still spelled `output_id`
        // reads back onto the field that replaced it.
        let stored: ToolResultPreview = serde_json::from_value(serde_json::json!({
            "tool": "exec",
            "exit_code": 0,
            "timed_out": false,
            "output_truncated": false,
            "stdout": "ok",
            "stderr": "",
        }))
        .unwrap();
        let ToolResultPreview::Exec { outputs, .. } = stored else {
            panic!("stored exec row");
        };
        assert!(outputs.is_empty());
        let retained: ResultEntry = serde_json::from_value(serde_json::json!({
            "kind": "output",
            "label": "report.md",
            "detail": null,
            "meta": "v3 · updated",
            "output_id": "output-report",
        }))
        .unwrap();
        assert_eq!(retained.target_id.as_deref(), Some("output-report"));
    }

    #[test]
    fn only_tools_with_a_variant_project_anything() {
        for (tool, arguments) in [
            ("read_file", serde_json::json!({ "path": "notes.md" })),
            ("mcp__server__tool", serde_json::json!({ "any": "thing" })),
        ] {
            assert_eq!(ToolActionPreview::build(tool, &arguments), None);
            assert_eq!(
                ToolResultPreview::build(tool, &output_with_data(arguments.clone())),
                None
            );
        }

        // A tool on the entries allowlist still projects nothing until it opts
        // in: the allowlist says who *may* have a card, and the `entries` key
        // in the tool's own output is the opt-in.
        for tool in ["search", "web_search", "write_file"] {
            assert_eq!(
                ToolResultPreview::build(
                    tool,
                    &output_with_data(serde_json::json!({ "query": "private" }))
                ),
                None
            );
        }
    }

    /// A gated workspace write names the file it will touch, and nothing else.
    ///
    /// The card used to ask about "files in this chat's workspace" without
    /// saying which one. The projection carries the path — the resource under
    /// review — and deliberately not the content, so the action can never be
    /// exactly describable: no narrow grant and no judge candidacy can be
    /// built from a projection that lost the document.
    #[test]
    fn a_workspace_write_names_its_path_but_is_never_exact() {
        let arguments = json!({ "path": "reports/q3.md", "content": "private document body" });
        let preview = ToolActionPreview::build("write_file", &arguments);
        assert_eq!(
            preview,
            Some(ToolActionPreview::WriteFile {
                path: "reports/q3.md".into()
            })
        );
        // Wire shape: the serialized preview carries the path and never the
        // content, whatever the arguments held.
        let json = serde_json::to_string(&preview.unwrap()).unwrap();
        assert_eq!(json, r#"{"tool":"write_file","path":"reports/q3.md"}"#);
        assert!(!ToolActionPreview::describes_exactly(
            "write_file",
            &arguments
        ));
        // Malformed arguments project nothing rather than an empty card.
        assert_eq!(
            ToolActionPreview::build("write_file", &json!({ "content": "x" })),
            None
        );
    }

    #[test]
    fn only_an_allowlisted_tool_gets_a_card_from_its_own_output() {
        // `entries` is a projection a tool opts into, not a channel anything
        // can write to. An external MCP tool controls its structured data
        // outright, so the allowlist — not the key — is what grants the card.
        let output = ToolOutput::text("done")
            .with_entries(vec![ResultEntry::new(ResultEntryKind::File, "notes.md")]);
        assert_eq!(
            ToolResultPreview::build("mcp__gateway__tool", &output),
            None
        );
        assert_eq!(ToolResultPreview::build("read_tool_result", &output), None);
        assert!(ToolResultPreview::build("read_file", &output).is_some());

        // The connected-folder tools run on the desktop, which authors its own
        // rows and posts them back. The name checked here is the one stored on
        // the call, never one the executor supplied, so an executor cannot
        // award itself a card for a tool that projects nothing.
        assert!(ToolResultPreview::build(crate::LIST_FOLDER_TOOL, &output).is_some());
        assert_eq!(
            ToolResultPreview::build(crate::REQUEST_FOLDER_ACCESS_TOOL, &output),
            None
        );

        // Nor does a call that failed: whatever rows it left behind describe
        // work that did not happen.
        let mut failed = output.clone();
        failed.is_error = true;
        assert_eq!(ToolResultPreview::build("read_file", &failed), None);
    }

    #[test]
    fn a_listed_result_is_bounded_row_by_row_and_counts_what_it_dropped() {
        let long = "a".repeat(MAX_RESULT_ENTRY_CHARS * 2);
        let mut rows: Vec<_> = (0..MAX_RESULT_ENTRIES + 7)
            .map(|index| serde_json::json!({ "kind": "file", "label": index.to_string() }))
            .collect();
        rows[0] = serde_json::json!({
            "kind": "file",
            "label": long,
            // Control characters could otherwise forge row structure.
            "detail": "reports/\nq3.md",
            "meta": 7,
        });
        // One row the clamp will reject outright: no label to lead with.
        rows.push(serde_json::json!({ "kind": "file" }));
        let Some(ToolResultPreview::Entries {
            entries, elided, ..
        }) = ToolResultPreview::build(
            "list_dir",
            &ToolOutput::text("listing").with_data(serde_json::json!({ "entries": rows })),
        )
        else {
            panic!("list_dir projects a list");
        };
        assert_eq!(entries.len(), MAX_RESULT_ENTRIES);
        // Cut at the limit and dropped by the clamp both count: they are rows
        // the call produced that the reader will not see.
        assert_eq!(elided, 8);
        assert_eq!(entries[0].label.chars().count(), MAX_RESULT_ENTRY_CHARS);
        assert_eq!(entries[0].detail.as_deref(), Some("reports/q3.md"));
        // A hint that is not a string is dropped; the row survives without it.
        assert_eq!(entries[0].meta, None);
    }

    #[test]
    fn a_call_that_found_nothing_says_so_rather_than_projecting_nothing() {
        // "Searched and matched nothing" and "searched" are the same muted rail
        // line, and only the first is a fact. An empty list is the difference.
        assert_eq!(
            ToolResultPreview::build(
                "search",
                &ToolOutput::text("No matching passages found.").with_entries(Vec::new()),
            ),
            Some(ToolResultPreview::Entries {
                entries: Vec::new(),
                failures: Vec::new(),
                elided: 0,
            })
        );
    }

    #[test]
    fn a_batch_call_reports_what_it_could_not_do_beside_what_it_did() {
        // Listing only the successes is not reporting, it is flattering: a card
        // that says "Imported 3" for a call that also failed two has told the
        // reader something false by omission.
        let output = ToolOutput::text("imported 3 of 5")
            .with_entries(vec![ResultEntry::new(ResultEntryKind::File, "q3.md")])
            .with_failures(vec![
                ResultFailure::new("q4.md", "file is not valid UTF-8"),
                // A tool that cannot even name what failed still reports it.
                ResultFailure::unnamed("the folder is no longer available"),
            ]);
        assert_eq!(
            ToolResultPreview::build("read_file", &output),
            Some(ToolResultPreview::Entries {
                entries: vec![ResultEntry::new(ResultEntryKind::File, "q3.md")],
                failures: vec![
                    ResultFailure::new("q4.md", "file is not valid UTF-8"),
                    ResultFailure::unnamed("the folder is no longer available"),
                ],
                elided: 0,
            })
        );
    }

    #[test]
    fn a_failure_without_a_readable_reason_is_dropped_but_still_counted() {
        // The reason is the whole of what a failure row is for, so a row
        // without one cannot render — but vanishing silently would leave the
        // card quietly under-reporting how much went wrong.
        let Some(ToolResultPreview::Entries {
            failures, elided, ..
        }) = ToolResultPreview::build(
            "read_file",
            &ToolOutput::text("done").with_data(serde_json::json!({
                "entries": [],
                "failures": [
                    { "label": "q4.md", "error": "unreadable" },
                    { "label": "q5.md" },
                    { "label": "q6.md", "error": "\u{0}\u{1b}" },
                ],
            })),
        )
        else {
            panic!("read_file projects a list");
        };
        assert_eq!(failures, vec![ResultFailure::new("q4.md", "unreadable")]);
        assert_eq!(elided, 2);
    }

    #[test]
    fn a_listed_result_carries_only_the_three_fields_a_row_reads_as() {
        // The projection is closed the same way every other variant is: the
        // tool's output text and its other structured data stay behind it.
        let output = ToolOutput::text("Wrote q3.md (900 bytes).")
            .with_data(serde_json::json!({ "output_id": "41ff", "revision_count": 3 }))
            .with_entries(vec![
                ResultEntry::new(ResultEntryKind::Output, "q3.md").with_meta(format_bytes(900))
            ]);
        let json = serde_json::to_string(&ToolResultPreview::build("write_file", &output).unwrap())
            .unwrap();
        assert!(json.contains(r#""label":"q3.md""#));
        assert!(json.contains(r#""meta":"900 B""#));
        assert!(!json.contains("41ff"));
        assert!(!json.contains("revision_count"));
    }

    #[test]
    fn an_mcp_tool_with_a_declared_view_projects_a_reference_not_markup() {
        let output =
            output_with_data(serde_json::json!({"status": 200})).with_ui_view(crate::ToolUiView {
                server: "gateway".into(),
                resource_uri: "ui://gateway/app.html".into(),
            });
        assert_eq!(
            ToolResultPreview::build("mcp__gateway__tool__proxy_api", &output),
            Some(ToolResultPreview::McpApp {
                server: "gateway".into(),
                resource_uri: "ui://gateway/app.html".into(),
            })
        );

        // The projection is a closed reference: serialized, it carries the
        // coordinates and nothing from the tool's output or structured data.
        let json = serde_json::to_string(
            &ToolResultPreview::build("mcp__gateway__tool", &output).unwrap(),
        )
        .unwrap();
        assert!(!json.contains("200"));
        assert!(!json.contains("done"));
    }

    #[test]
    fn an_unconfigured_web_search_projects_only_a_typed_setup_signal() {
        let output = crate::ToolOutput::failed(
            crate::ToolErrorCategory::ConfigurationRequired,
            "private diagnostic that must not cross",
        );
        let result = ToolResultPreview::build(crate::WEB_SEARCH_TOOL, &output);
        assert_eq!(result, Some(ToolResultPreview::WebSearchProviderRequired));
        assert_eq!(
            serde_json::to_value(result.unwrap()).unwrap(),
            serde_json::json!({"tool": "web_search_provider_required"})
        );

        assert_eq!(
            ToolResultPreview::from_stored_error(
                crate::WEB_SEARCH_TOOL,
                Some(crate::ToolErrorCategory::ConfigurationRequired.as_str()),
            ),
            Some(ToolResultPreview::WebSearchProviderRequired)
        );
        assert_eq!(
            ToolResultPreview::from_stored_error("exec", Some("configuration_required")),
            None
        );
        assert_eq!(
            ToolResultPreview::from_stored_error(crate::WEB_SEARCH_TOOL, Some("tool_failed")),
            None
        );
    }

    #[test]
    fn mcp_views_stay_behind_the_boundary_without_a_valid_declaration() {
        let view = |server: &str, uri: &str| {
            crate::ToolOutput::text("done").with_ui_view(crate::ToolUiView {
                server: server.into(),
                resource_uri: uri.into(),
            })
        };

        // No declaration, non-MCP name, failed call, malformed URI or server.
        assert_eq!(
            ToolResultPreview::build("mcp__gateway__tool", &crate::ToolOutput::text("done")),
            None
        );
        assert_eq!(
            ToolResultPreview::build("write_file", &view("gateway", "ui://gateway/app.html")),
            None
        );
        let mut failed = view("gateway", "ui://gateway/app.html");
        failed.is_error = true;
        assert_eq!(
            ToolResultPreview::build("mcp__gateway__tool", &failed),
            None
        );
        for (server, uri) in [
            ("gateway", "https://evil.example/app"),
            ("gateway", "ui://x\u{1b}[31m"),
            ("bad\nserver", "ui://gateway/app.html"),
            (
                "gateway",
                &format!("ui://{}", "x".repeat(MAX_RESULT_VIEW_URI_CHARS)),
            ),
        ] {
            assert_eq!(
                ToolResultPreview::build("mcp__gateway__tool", &view(server, uri)),
                None,
                "{server} {uri:.40}"
            );
        }
    }

    #[test]
    fn malformed_exec_arguments_yield_no_preview_rather_than_a_raw_dump() {
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({ "command": "" }),
            serde_json::json!({ "command": 7 }),
            serde_json::json!("not an object"),
        ] {
            assert_eq!(ToolActionPreview::build("exec", &arguments), None);
        }
    }

    #[test]
    fn exec_action_bounds_the_card_a_model_can_paint() {
        let long = "a".repeat(MAX_ACTION_FIELD_CHARS * 2);
        let many: Vec<_> = (0..MAX_ACTION_ARGS * 2).map(|i| i.to_string()).collect();
        let Some(ToolActionPreview::Exec {
            command, args, cwd, ..
        }) = ToolActionPreview::build(
            "exec",
            &serde_json::json!({
                "command": long,
                "args": many,
                // Control characters could otherwise forge card structure.
                "cwd": "scratch\n\u{1b}[31mapproved",
            }),
        )
        else {
            panic!("exec projects an action");
        };
        assert_eq!(command.chars().count(), MAX_ACTION_FIELD_CHARS);
        assert_eq!(args.len(), MAX_ACTION_ARGS);
        assert_eq!(cwd, "scratch[31mapproved");
    }

    #[test]
    fn exec_result_carries_the_streams_and_the_exit_status() {
        let result = ToolResultPreview::build(
            "exec",
            &output_with_data(serde_json::json!({
                "exit_code": 1,
                "timed_out": false,
                "output_truncated": true,
                "stdout": "line one\nline two\n",
                "stderr": "boom\n",
                "provider": "e2b",
            })),
        );
        assert_eq!(
            result,
            Some(ToolResultPreview::Exec {
                exit_code: Some(1),
                timed_out: false,
                output_truncated: true,
                stdout: "line one\nline two\n".into(),
                stderr: "boom\n".into(),
                images: Vec::new(),
                outputs: Vec::new(),
                degraded: None,
                backend: Some(ExecBackend::E2b),
            })
        );
        assert!(result.unwrap().has_output());
    }

    #[test]
    fn a_signal_kill_has_no_exit_code_and_a_silent_run_has_no_output() {
        let Some(result) = ToolResultPreview::build(
            "exec",
            &output_with_data(serde_json::json!({ "exit_code": null, "timed_out": true })),
        ) else {
            panic!("exec projects a result");
        };
        assert_eq!(
            result,
            ToolResultPreview::Exec {
                exit_code: None,
                timed_out: true,
                output_truncated: false,
                stdout: String::new(),
                stderr: String::new(),
                images: Vec::new(),
                outputs: Vec::new(),
                degraded: None,
                backend: None,
            }
        );
        assert!(!result.has_output());
    }

    #[test]
    fn captured_streams_keep_their_line_breaks_but_stay_bounded() {
        let Some(ToolResultPreview::Exec { stdout, stderr, .. }) = ToolResultPreview::build(
            "exec",
            &output_with_data(serde_json::json!({
                "stdout": "a".repeat(MAX_RESULT_STREAM_CHARS * 2),
                "stderr": "kept\nlines\n\u{1b}[31mbut not escapes",
            })),
        ) else {
            panic!("exec projects a result");
        };
        assert_eq!(stdout.chars().count(), MAX_RESULT_STREAM_CHARS);
        assert_eq!(stderr, "kept\nlines\n[31mbut not escapes");
    }
}
