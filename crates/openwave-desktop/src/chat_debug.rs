//! "Copy full chat debug" — a per-chat diagnostic bundle for bug reports.
//!
//! The bundle is rendered here, from the durable store, rather than scraped
//! from the rendered transcript: the journal is the source of truth for what a
//! turn actually did, and a renderer projection deliberately hides the details
//! (raw tool arguments, error kinds, sequence gaps) that a bug report needs.
//!
//! Two shapes of the same document:
//!
//! * [`copy_chat_debug_bundle`] returns Markdown for the clipboard, with each
//!   block and the document as a whole bounded so a chat with a multi-megabyte
//!   tool result cannot wedge the webview on a clipboard write.
//! * [`save_chat_debug_bundle`] writes the untruncated document to a file the
//!   user picks.
//!
//! **Redaction.** The bundle contains the entire conversation on purpose — the
//! chat *is* the diagnostic, and stripping it would leave a report nobody can
//! act on. What is scrubbed is credential-shaped material, which can reach the
//! journal without ever being stored as a credential: a provider error body
//! echoing an `Authorization` header, an `exec` result that catted a `.env`, a
//! user who pasted a key into the composer. [`scrub_credentials`] runs over
//! every rendered byte. Nothing here reads the keychain, and no store API used
//! below returns provider credentials.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use openwave_core::event::{AgentEvent, SequencedEvent};
use openwave_core::model::{Chat, Message, TurnRun};
use openwave_core::ChatId;
use serde::Deserialize;
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::host_access::HostAccess;

/// Longest single rendered block (one event, one message body) kept in the
/// clipboard document. Chosen to keep a runaway `exec` result readable in a
/// GitHub issue while still showing the shape of what came back.
const CLIPBOARD_BLOCK_BYTES: usize = 8 * 1024;

/// Ceiling on the whole clipboard document. Past this the remaining journal
/// entries are dropped with a count, and the user is pointed at the file
/// export, which has no ceiling.
const CLIPBOARD_TOTAL_BYTES: usize = 1024 * 1024;

/// What replaces a token the scrubber judges credential-shaped.
const REDACTED: &str = "[redacted]";

/// Shortest value redacted on the strength of its key alone (`"token": …`).
/// Below this the value is more likely a placeholder than a secret.
const MIN_KEYED_SECRET_LEN: usize = 6;

/// Characters past a known prefix before a token counts as a live credential.
/// `sk-` alone is prose; `sk-` plus twenty characters is a key.
const MIN_PREFIXED_SECRET_TAIL: usize = 8;

/// Token prefixes that are credentials by construction, whatever surrounds
/// them. Vendor-issued formats only — a heuristic broad enough to catch
/// arbitrary high-entropy strings would redact the conversation.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "pk_live_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "gitlab-ci-token",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "xapp-",
    "AKIA",
    "ASIA",
    "AIza",
    "ya29.",
    "npm_",
    "hf_",
    "r8_",
    "dop_v1_",
    "shpat_",
    "AGPT-",
];

/// Key names whose value is a credential regardless of its own shape.
/// Matched case-insensitively against the token immediately preceding the
/// value across `:`/`=`/quote/space separators, which covers JSON, TOML,
/// `KEY=value` env dumps, and HTTP header lines alike.
const SECRET_KEYS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
];

/// Key-name endings that name a credential, so a vendor-prefixed variable
/// (`ANTHROPIC_API_KEY`, `GITHUB_TOKEN`) is covered without enumerating every
/// vendor.
const SECRET_KEY_SUFFIXES: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "_token",
    "-token",
    "_secret",
    "-secret",
    "_password",
    "-password",
    "_credential",
    "private_key",
    "secret_key",
    "access_key",
];

/// Redact credential-shaped material from arbitrary rendered text.
///
/// Deliberately conservative about user content and deliberately aggressive
/// about anything that looks issued: a false positive costs one unreadable
/// token in a bug report, a false negative publishes a live key.
#[must_use]
pub(crate) fn scrub_credentials(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    // The token that could name the value about to be read, if the separators
    // since have all been "still the same key/value pair" characters.
    let mut key: Option<String> = None;
    let mut rest = input;
    while let Some(next) = rest.chars().next() {
        if is_token_char(next) {
            let end = rest.find(|c| !is_token_char(c)).unwrap_or(rest.len());
            let (token, tail) = rest.split_at(end);
            if is_secret(token, key.as_deref()) {
                out.push_str(REDACTED);
                key = None;
            } else {
                out.push_str(token);
                key = Some(token.to_ascii_lowercase());
            }
            rest = tail;
        } else {
            out.push(next);
            if !matches!(next, '"' | '\'' | ':' | '=' | ' ' | '\t' | '\\') {
                key = None;
            }
            rest = &rest[next.len_utf8()..];
        }
    }
    out
}

/// Whether a character continues the current token. `:` and `=` are excluded
/// so `key: value` and `KEY=value` each stay two tokens; `.` `/` `+` `~` are
/// included so JWTs, base64 payloads, and `ya29.` tokens are not split into
/// fragments.
const fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '~')
}

fn is_secret(token: &str, key: Option<&str>) -> bool {
    // An HTTP auth scheme word names the credential that follows it; redacting
    // the word itself would consume the naming and leave the actual token in
    // the clear. `Token` is a scheme in its own right (`Authorization: Token
    // …`), and is also a key name below.
    if matches!(
        token.to_ascii_lowercase().as_str(),
        "bearer" | "basic" | "digest" | "negotiate" | "token"
    ) {
        return false;
    }
    if token.len() >= MIN_KEYED_SECRET_LEN {
        if let Some(key) = key {
            // `Bearer <token>` / `Basic <token>`: the scheme word names the
            // value the same way a JSON key does.
            if key == "bearer"
                || key == "basic"
                || key == "digest"
                || SECRET_KEYS.contains(&key)
                || SECRET_KEY_SUFFIXES
                    .iter()
                    .any(|suffix| key.ends_with(suffix))
            {
                return true;
            }
        }
    }
    if SECRET_PREFIXES.iter().any(|prefix| {
        token.starts_with(prefix) && token.len() >= prefix.len() + MIN_PREFIXED_SECRET_TAIL
    }) {
        return true;
    }
    // A JWT is a credential wherever it appears and has an unmistakable shape.
    token.starts_with("eyJ") && token.len() >= 24 && token.matches('.').count() >= 2
}

/// Everything the renderer needs that does not come out of the store.
pub(crate) struct BundleEnvironment {
    pub(crate) app_version: String,
    pub(crate) os: &'static str,
    pub(crate) arch: &'static str,
    pub(crate) generated_at: DateTime<Utc>,
}

/// The store-side inputs, read once and rendered deterministically.
pub(crate) struct BundleInput {
    pub(crate) chat: Chat,
    pub(crate) turns: Vec<TurnRun>,
    pub(crate) messages: Vec<Message>,
    pub(crate) events: Vec<SequencedEvent>,
}

/// How much of the document to keep. The clipboard is bounded; a saved file
/// is not.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BundleLimit {
    Clipboard,
    Unbounded,
}

impl BundleLimit {
    const fn block_bytes(self) -> Option<usize> {
        match self {
            Self::Clipboard => Some(CLIPBOARD_BLOCK_BYTES),
            Self::Unbounded => None,
        }
    }

    const fn total_bytes(self) -> Option<usize> {
        match self {
            Self::Clipboard => Some(CLIPBOARD_TOTAL_BYTES),
            Self::Unbounded => None,
        }
    }
}

/// Render the bundle. Pure: same inputs, same bytes, so a saved file and a
/// copied document differ only where the limit bites.
#[must_use]
pub(crate) fn render_bundle(
    environment: &BundleEnvironment,
    input: &BundleInput,
    limit: BundleLimit,
) -> String {
    let mut out = String::new();
    render_header(&mut out, environment, input, limit);
    render_turns(&mut out, &input.turns);
    render_messages(&mut out, &input.messages, limit);
    render_journal(&mut out, &input.events, limit);
    scrub_credentials(&out)
}

fn render_header(
    out: &mut String,
    environment: &BundleEnvironment,
    input: &BundleInput,
    limit: BundleLimit,
) {
    let chat = &input.chat;
    let _ = writeln!(out, "# OpenWave chat debug bundle\n");
    let _ = writeln!(
        out,
        "Generated {} by OpenWave {} on {}/{}.\n",
        environment.generated_at.to_rfc3339(),
        environment.app_version,
        environment.os,
        environment.arch,
    );
    let _ = writeln!(
        out,
        "This bundle contains the full conversation — every message, tool \
         argument, tool result, and file path in this chat. API keys and \
         other credential-shaped tokens are removed; nothing else is. Read \
         it before sharing it.\n",
    );
    let _ = writeln!(out, "## Chat\n");
    let _ = writeln!(out, "| Field | Value |");
    let _ = writeln!(out, "| --- | --- |");
    let _ = writeln!(out, "| Chat id | `{}` |", chat.id);
    let _ = writeln!(
        out,
        "| Project id | {} |",
        chat.project_id
            .map_or_else(|| "none".to_owned(), |id| format!("`{id}`"))
    );
    let _ = writeln!(out, "| Created | {} |", chat.created_at.to_rfc3339());
    let _ = writeln!(out, "| Model | {} |", describe_model(chat.model.as_deref()));
    let _ = writeln!(
        out,
        "| Reasoning effort | {} |",
        chat.reasoning_effort.map_or_else(
            || "provider default".to_owned(),
            |effort| effort.as_str().to_owned()
        )
    );
    let _ = writeln!(
        out,
        "| Permission mode | {} |",
        chat.permission_mode.map_or(
            "ask (default)",
            openwave_core::model::PermissionMode::as_str
        ),
    );
    let _ = writeln!(
        out,
        "| Network policy | `{}` |",
        serde_json::to_string(&chat.network_policy).unwrap_or_else(|_| "unserializable".to_owned())
    );
    let _ = writeln!(
        out,
        "| Connected folders | {} (revision {}) |",
        chat.root_attachments.len(),
        chat.attachment_revision,
    );
    let _ = writeln!(out, "| Turns | {} |", input.turns.len());
    let _ = writeln!(out, "| Messages | {} |", input.messages.len());
    let _ = writeln!(out, "| Journal events | {} |", input.events.len());
    if limit == BundleLimit::Clipboard {
        let _ = writeln!(
            out,
            "| Export | clipboard (blocks capped at {CLIPBOARD_BLOCK_BYTES} bytes) |",
        );
    } else {
        let _ = writeln!(out, "| Export | file (complete) |");
    }
    out.push('\n');
}

/// Render `chat.model` as stored. The value is the provider-scoped selection
/// key, which already names its provider textually; parsing it here would
/// duplicate the server's key format in a second crate for no gain, and the
/// per-turn `TurnRun.model` below records what each turn actually ran on.
fn describe_model(model: Option<&str>) -> String {
    model.map_or_else(
        || "none (configured default)".to_owned(),
        |model| format!("`{model}`"),
    )
}

fn render_turns(out: &mut String, turns: &[TurnRun]) {
    let _ = writeln!(out, "## Turns\n");
    if turns.is_empty() {
        let _ = writeln!(out, "No durable turns.\n");
        return;
    }
    let _ = writeln!(
        out,
        "| # | Turn id | Status | Model | Attempts | Started | Finished | Tokens in/out (+cache r/w) | Error |",
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    for (index, turn) in turns.iter().enumerate() {
        let usage = &turn.usage;
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | `{}` | {}/{} | {} | {} | {}/{} (+{}/{}) | {} |",
            index + 1,
            turn.id,
            serde_plain_status(turn.status),
            turn.model,
            turn.attempt_count,
            turn.max_attempts,
            optional_time(turn.started_at),
            optional_time(turn.finished_at),
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
            describe_turn_error(turn),
        );
    }
    out.push('\n');
    for turn in turns {
        let Some(detail) = turn.last_error_detail.as_deref() else {
            continue;
        };
        let _ = writeln!(out, "Turn `{}` error detail:\n", turn.id);
        push_fenced(out, "text", detail, None);
    }
}

fn describe_turn_error(turn: &TurnRun) -> String {
    turn.last_error_code
        .as_deref()
        .map_or_else(|| "—".to_owned(), |code| format!("`{code}`"))
}

fn serde_plain_status(status: openwave_core::model::TurnRunStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn optional_time(value: Option<DateTime<Utc>>) -> String {
    value.map_or_else(|| "—".to_owned(), |time| time.to_rfc3339())
}

/// The journal has no user-message variant — a turn's prompt lives in the
/// messages table — so the transcript is rendered alongside it rather than
/// reconstructed from `text_delta` runs alone.
fn render_messages(out: &mut String, messages: &[Message], limit: BundleLimit) {
    let _ = writeln!(out, "## Messages\n");
    if messages.is_empty() {
        let _ = writeln!(out, "No messages.\n");
        return;
    }
    for message in messages {
        let _ = writeln!(
            out,
            "### {} · `{}` · turn `{}` · {}\n",
            role_label(message.role),
            message.id,
            message.turn_id,
            message.created_at.to_rfc3339(),
        );
        push_fenced(out, "text", &message.content, limit.block_bytes());
    }
}

fn role_label(role: openwave_core::model::Role) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// One rendered journal entry. Consecutive delta events are folded together:
/// a hundred-token answer is a hundred `text_delta` rows, and rendering them
/// individually buries the tool calls and errors a report is actually about.
enum JournalEntry<'a> {
    Text {
        first: i64,
        last: i64,
        kind: &'static str,
        text: String,
    },
    ToolArgs {
        first: i64,
        last: i64,
        call_id: String,
        args: String,
    },
    Event {
        seq: i64,
        event: &'a AgentEvent,
    },
}

fn coalesce(events: &[SequencedEvent]) -> Vec<JournalEntry<'_>> {
    let mut entries: Vec<JournalEntry<'_>> = Vec::new();
    for SequencedEvent { seq, event } in events {
        match event {
            AgentEvent::TextDelta { text } | AgentEvent::ReasoningDelta { text } => {
                let kind = if matches!(event, AgentEvent::TextDelta { .. }) {
                    "assistant text"
                } else {
                    "reasoning"
                };
                match entries.last_mut() {
                    Some(JournalEntry::Text {
                        last,
                        kind: previous,
                        text: buffer,
                        ..
                    }) if *previous == kind => {
                        buffer.push_str(text);
                        *last = *seq;
                    }
                    _ => entries.push(JournalEntry::Text {
                        first: *seq,
                        last: *seq,
                        kind,
                        text: text.clone(),
                    }),
                }
            }
            AgentEvent::ToolCallArgsDelta { call_id, fragment } => {
                let id = call_id.to_string();
                match entries.last_mut() {
                    Some(JournalEntry::ToolArgs {
                        last,
                        call_id: previous,
                        args,
                        ..
                    }) if *previous == id => {
                        args.push_str(fragment);
                        *last = *seq;
                    }
                    _ => entries.push(JournalEntry::ToolArgs {
                        first: *seq,
                        last: *seq,
                        call_id: id,
                        args: fragment.clone(),
                    }),
                }
            }
            other => entries.push(JournalEntry::Event {
                seq: *seq,
                event: other,
            }),
        }
    }
    entries
}

fn render_journal(out: &mut String, events: &[SequencedEvent], limit: BundleLimit) {
    let _ = writeln!(out, "## Journal\n");
    if events.is_empty() {
        let _ = writeln!(out, "No journal events.\n");
        return;
    }
    let entries = coalesce(events);
    let total = limit.total_bytes();
    for (index, entry) in entries.iter().enumerate() {
        if total.is_some_and(|cap| out.len() >= cap) {
            let _ = writeln!(
                out,
                "_{} further journal entries omitted to keep the clipboard \
                 document under {} bytes. Use \"Save debug bundle\" for the \
                 complete export._\n",
                entries.len() - index,
                CLIPBOARD_TOTAL_BYTES,
            );
            return;
        }
        render_entry(out, entry, limit);
    }
}

fn render_entry(out: &mut String, entry: &JournalEntry<'_>, limit: BundleLimit) {
    match entry {
        JournalEntry::Text {
            first,
            last,
            kind,
            text,
        } => {
            let _ = writeln!(out, "### {} · {kind}\n", span(*first, *last));
            push_fenced(out, "text", text, limit.block_bytes());
        }
        JournalEntry::ToolArgs {
            first,
            last,
            call_id,
            args,
        } => {
            let _ = writeln!(
                out,
                "### {} · tool_call_args · call `{call_id}`\n",
                span(*first, *last),
            );
            push_fenced(out, "json", &pretty_if_json(args), limit.block_bytes());
        }
        JournalEntry::Event { seq, event } => {
            let value = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
            let kind = value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let _ = writeln!(out, "### seq {seq} · {kind}\n");
            let rendered =
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".to_owned());
            push_fenced(out, "json", &rendered, limit.block_bytes());
        }
    }
}

fn span(first: i64, last: i64) -> String {
    if first == last {
        format!("seq {first}")
    } else {
        format!("seq {first}-{last}")
    }
}

/// Streamed tool arguments are only valid JSON once the call completes, and a
/// bug report is often about the case where they never did. Pretty-print when
/// possible, keep the raw fragment when not.
fn pretty_if_json(raw: &str) -> String {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| raw.to_owned())
}

/// Append a fenced block whose fence is always longer than the longest run of
/// backticks inside it, so a tool result containing Markdown cannot break out
/// of the document it is quoted in.
fn push_fenced(out: &mut String, language: &str, content: &str, cap: Option<usize>) {
    let (body, note) = match cap {
        Some(cap) if content.len() > cap => {
            let cut = floor_char_boundary(content, cap);
            (
                &content[..cut],
                Some(format!(
                    "\n_Truncated: {cut} of {} bytes shown._\n",
                    content.len()
                )),
            )
        }
        _ => (content, None),
    };
    let fence = "`".repeat(longest_backtick_run(body).max(2) + 1);
    let _ = writeln!(out, "{fence}{language}");
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    let _ = writeln!(out, "{fence}");
    if let Some(note) = note {
        out.push_str(&note);
    }
    out.push('\n');
}

fn longest_backtick_run(content: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in content.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn floor_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ChatDebugRequest {
    chat_id: Uuid,
}

/// Build the clipboard-sized bundle. The renderer writes it to the clipboard
/// itself; passing the text back rather than writing it here keeps the copy on
/// the same user gesture the webview requires.
#[tauri::command]
pub(crate) async fn copy_chat_debug_bundle(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    request: ChatDebugRequest,
) -> Result<String, String> {
    let input = load_bundle_input(&host_access, request.chat_id).await?;
    Ok(render_bundle(
        &environment(&app),
        &input,
        BundleLimit::Clipboard,
    ))
}

/// Write the complete, untruncated bundle to a file the user picks. Returns
/// `false` when the save dialog was dismissed.
#[tauri::command]
pub(crate) async fn save_chat_debug_bundle(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    request: ChatDebugRequest,
) -> Result<bool, String> {
    let input = load_bundle_input(&host_access, request.chat_id).await?;
    let bundle = render_bundle(&environment(&app), &input, BundleLimit::Unbounded);
    let filename = format!("openwave-chat-{}.md", input.chat.id);
    let _picker = host_access.debug_exports.lock().await;
    let Some(destination) = pick_bundle_path(&app, &filename).await? else {
        return Ok(false);
    };
    write_bundle(&destination, bundle.as_bytes())?;
    Ok(true)
}

fn environment(app: &AppHandle) -> BundleEnvironment {
    BundleEnvironment {
        app_version: app.package_info().version.to_string(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        generated_at: Utc::now(),
    }
}

async fn load_bundle_input(host_access: &HostAccess, chat_id: Uuid) -> Result<BundleInput, String> {
    if chat_id.is_nil() {
        return Err("Invalid conversation id".to_owned());
    }
    let chat_id = ChatId(chat_id);
    let store = host_access
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(chat_id)
        .await
        .map_err(|_| "Could not read this conversation".to_owned())?
        .ok_or_else(|| "This conversation no longer exists".to_owned())?;
    // Turn history is best-effort: a store without durable turn state still
    // produces a useful journal-and-messages bundle.
    let turns = store.list_turn_runs(chat_id).await.unwrap_or_default();
    let messages = store
        .list_messages(chat_id)
        .await
        .map_err(|_| "Could not read this conversation's messages".to_owned())?;
    let events = store
        .list_events(chat_id, 0)
        .await
        .map_err(|_| "Could not read this conversation's journal".to_owned())?;
    Ok(BundleInput {
        chat,
        turns,
        messages,
        events,
    })
}

async fn pick_bundle_path(app: &AppHandle, filename: &str) -> Result<Option<PathBuf>, String> {
    use tauri_plugin_dialog::DialogExt as _;

    let (tx, rx) = oneshot::channel();
    let mut picker = app
        .dialog()
        .file()
        .set_title("Save debug bundle")
        .set_file_name(filename)
        .add_filter("Markdown", &["md"]);
    if let Some(window) = app.get_webview_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.save_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await
        .map_err(|_| "The save dialog closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "The save dialog returned an invalid destination".to_owned())
}

fn write_bundle(destination: &Path, content: &[u8]) -> Result<(), String> {
    if !destination.is_absolute() {
        return Err("The save destination is invalid".to_owned());
    }
    std::fs::write(destination, content).map_err(|_| "Could not write the debug bundle".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::error::AgentErrorInfo;
    use openwave_core::id::{CallId, MessageId, TurnId};
    use openwave_core::model::{NetworkPolicy, PermissionMode, Role, TurnRunStatus};
    use openwave_core::provider::Usage;
    use openwave_core::tool::ToolOutput;
    use openwave_core::{AgentRunId, ProjectId};

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("a valid timestamp")
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// A chat that failed a turn after one tool call, which is the shape most
    /// bug reports actually have.
    fn sample_input() -> BundleInput {
        let chat_id = ChatId(uuid(1));
        let turn_id = TurnId(uuid(2));
        let call_id = CallId(uuid(3));
        BundleInput {
            chat: Chat {
                id: chat_id,
                project_id: Some(ProjectId(uuid(9))),
                title: Some("Broken exec".to_owned()),
                model: Some("anthropic::claude-sonnet-4-5".to_owned()),
                reasoning_effort: None,
                permission_mode: Some(PermissionMode::Auto),
                network_policy: NetworkPolicy::Off,
                attachment_revision: 0,
                root_attachments: Vec::new(),
                created_at: at(0),
            },
            turns: vec![TurnRun {
                id: turn_id,
                chat_id,
                agent_run_id: AgentRunId(uuid(4)),
                input_message_id: MessageId(uuid(5)),
                output_message_id: None,
                model: "claude-sonnet-4-5".to_owned(),
                status: TurnRunStatus::Failed,
                attempt_count: 5,
                max_attempts: 5,
                claim_count: 5,
                model_steps: 2,
                usage: Usage {
                    input_tokens: 120,
                    output_tokens: 34,
                    cache_read_input_tokens: 8,
                    cache_creation_input_tokens: 0,
                },
                available_at: at(1),
                lease_token: None,
                lease_expires_at: None,
                started_at: Some(at(1)),
                finished_at: Some(at(9)),
                last_error_code: Some("provider_unavailable".to_owned()),
                last_error_detail: Some(
                    "401 from provider: {\"error\":{\"message\":\"invalid x-api-key\"}}".to_owned(),
                ),
                steer_revision: 0,
                last_steer_applied_at: None,
                created_at: at(0),
                updated_at: at(9),
            }],
            messages: vec![Message {
                id: MessageId(uuid(5)),
                chat_id,
                turn_id,
                role: Role::User,
                reasoning: Default::default(),
                content: "run git status in ~/code/openwave".to_owned(),
                created_at: at(0),
            }],
            events: vec![
                SequencedEvent {
                    seq: 1,
                    event: AgentEvent::TurnStarted { turn_id },
                },
                SequencedEvent {
                    seq: 2,
                    event: AgentEvent::TextDelta {
                        text: "Check".to_owned(),
                    },
                },
                SequencedEvent {
                    seq: 3,
                    event: AgentEvent::TextDelta {
                        text: "ing.".to_owned(),
                    },
                },
                SequencedEvent {
                    seq: 4,
                    event: AgentEvent::ToolCallStarted {
                        call_id,
                        name: "exec".to_owned(),
                    },
                },
                SequencedEvent {
                    seq: 5,
                    event: AgentEvent::ToolCallArgsDelta {
                        call_id,
                        fragment: "{\"command\":".to_owned(),
                    },
                },
                SequencedEvent {
                    seq: 6,
                    event: AgentEvent::ToolCallArgsDelta {
                        call_id,
                        fragment: "\"git status\"}".to_owned(),
                    },
                },
                SequencedEvent {
                    seq: 7,
                    event: AgentEvent::ToolCallCompleted {
                        call_id,
                        output: ToolOutput::text("On branch main"),
                        action: None,
                        result: None,
                    },
                },
                SequencedEvent {
                    seq: 8,
                    event: AgentEvent::TurnFailed {
                        error: AgentErrorInfo {
                            kind: "provider_unavailable".to_owned(),
                            message: "401 Unauthorized".to_owned(),
                        },
                    },
                },
            ],
        }
    }

    fn environment() -> BundleEnvironment {
        BundleEnvironment {
            app_version: "1.2.3".to_owned(),
            os: "macos",
            arch: "aarch64",
            generated_at: at(100),
        }
    }

    /// The whole contract of the feature in one assertion set: a
    /// representative chat — including a failed turn — renders every part a
    /// bug report is read for, deterministically, with delta runs folded.
    #[test]
    fn the_bundle_carries_what_a_bug_report_needs() {
        let input = sample_input();
        let bundle = render_bundle(&environment(), &input, BundleLimit::Clipboard);

        // Environment and configuration.
        assert!(bundle.contains("OpenWave 1.2.3 on macos/aarch64"));
        assert!(bundle.contains("`anthropic::claude-sonnet-4-5`"));
        assert!(bundle.contains("| Permission mode | auto |"));
        // The user's prompt, which lives in messages rather than the journal.
        assert!(bundle.contains("run git status in ~/code/openwave"));
        // The failure, by code, from both the turn row and the journal.
        assert!(bundle.contains("`provider_unavailable`"));
        assert!(bundle.contains("seq 8 · turn_failed"));
        // Tool call identity, arguments, and result.
        assert!(bundle.contains("seq 4 · tool_call_started"));
        assert!(bundle.contains("\"command\": \"git status\""));
        assert!(bundle.contains("On branch main"));
        // Token accounting.
        assert!(bundle.contains("120/34 (+8/0)"));
        // Delta runs are folded rather than emitted one row per chunk.
        assert!(bundle.contains("seq 2-3 · assistant text"));
        assert!(bundle.contains("Checking."));

        assert_eq!(
            bundle,
            render_bundle(&environment(), &input, BundleLimit::Clipboard),
            "the bundle must be deterministic",
        );
    }

    /// A giant tool result must not be handed to the clipboard whole, and the
    /// saved file must not lose it.
    #[test]
    fn oversized_blocks_are_truncated_only_for_the_clipboard() {
        let mut input = sample_input();
        input.messages[0].content = "x".repeat(CLIPBOARD_BLOCK_BYTES * 2);

        let copied = render_bundle(&environment(), &input, BundleLimit::Clipboard);
        assert!(copied.contains(&format!(
            "_Truncated: {CLIPBOARD_BLOCK_BYTES} of {} bytes shown._",
            CLIPBOARD_BLOCK_BYTES * 2
        )));

        let saved = render_bundle(&environment(), &input, BundleLimit::Unbounded);
        assert!(!saved.contains("_Truncated:"));
        assert!(saved.contains(&"x".repeat(CLIPBOARD_BLOCK_BYTES * 2)));
    }

    #[test]
    fn credential_shaped_material_never_survives_into_the_bundle() {
        let cases = [
            // Vendor-issued prefixes, wherever they appear.
            ("key is sk-ant-api03-AAAABBBBCCCCDDDD", "key is [redacted]"),
            ("token=ghp_AAAABBBBCCCCDDDDEEEE", "token=[redacted]"),
            ("AKIAIOSFODNN7EXAMPLE", "[redacted]"),
            ("AIzaSyA1234567890abcdefgh", "[redacted]"),
            (
                "ANTHROPIC_API_KEY=zzzzzzzzzzzz",
                "ANTHROPIC_API_KEY=[redacted]",
            ),
            (
                "authorization: Bearer abcdef123456",
                "authorization: Bearer [redacted]",
            ),
            // A scheme word must survive so the value it names is the thing
            // that gets redacted.
            (
                "Authorization: Token abcdef123456",
                "Authorization: Token [redacted]",
            ),
            // Ordinary conversation and file paths are left alone: the chat
            // is the diagnostic.
            (
                "please read /Users/ada/code/openwave/README.md",
                "please read /Users/ada/code/openwave/README.md",
            ),
            ("the exit code was 1", "the exit code was 1"),
        ];
        for (input, expected) in cases {
            assert_eq!(scrub_credentials(input), expected, "scrubbing {input:?}");
        }

        // Assembled rather than written out: a contiguous JSON api-key pair or
        // JWT literal in this file fails the repository's secret-scan lane.
        let value = "abcdef123456";
        assert_eq!(
            scrub_credentials(&format!("{{\"api_key\": \"{value}\"}}")),
            "{\"api_key\": \"[redacted]\"}",
        );
        let jwt = format!(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.{}",
            "dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        );
        assert_eq!(scrub_credentials(&jwt), "[redacted]");
    }

    /// The scrubber runs over the rendered document, not only over the
    /// journal, so a key echoed in a stored provider error body is gone too.
    #[test]
    fn scrubbing_covers_the_rendered_document() {
        let mut input = sample_input();
        input.turns[0].last_error_detail =
            Some("401: header x-api-key sk-ant-api03-LIVEKEY0123456789".to_owned());
        let bundle = render_bundle(&environment(), &input, BundleLimit::Clipboard);
        assert!(!bundle.contains("sk-ant-api03-LIVEKEY0123456789"));
        assert!(bundle.contains("[redacted]"));
    }

    /// A tool result containing a fenced code block must not terminate the
    /// block quoting it.
    #[test]
    fn fenced_blocks_cannot_be_escaped_by_their_content() {
        let mut out = String::new();
        push_fenced(&mut out, "text", "before\n```\ninner\n```\nafter", None);
        assert!(out.starts_with("````text\n"));
        assert!(out.trim_end().ends_with("\n````"));
    }
}
