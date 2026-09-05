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
use std::io::Write as _;
use std::path::{Path, PathBuf};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tauri::{AppHandle, Manager, State};
use tidebreak_core::event::{AgentEvent, SequencedAgentEvent};
use tidebreak_core::model::{Chat, Message, TurnRun};
use tidebreak_core::SessionId;
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
    let input = redact_private_key_blocks(input);
    let input = redact_credential_urls(&input);
    let input = redact_keyed_values(&input);
    redact_intrinsic_tokens(&input)
}

/// Redact the complete scalar introduced by a credential key. Treating a
/// keyed value as one token is unsafe: authorization schemes and quoted shell,
/// JSON, or TOML values can all contain spaces and punctuation.
fn redact_keyed_values(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut copied_through = 0;
    let mut cursor = 0;

    while cursor < input.len() {
        let next = input[cursor..]
            .chars()
            .next()
            .expect("cursor remains on a character boundary");
        if !is_token_char(next) {
            cursor += next.len_utf8();
            continue;
        }
        let token_end = input[cursor..]
            .find(|character| !is_token_char(character))
            .map_or(input.len(), |offset| cursor + offset);
        let key = input[cursor..token_end].to_ascii_lowercase();
        if is_secret_key_name(&key) {
            if let Some(value) = keyed_value(input, token_end, &key) {
                if let Some(redacted) = redact_scalar(&key, &input[value.start..value.end]) {
                    out.push_str(&input[copied_through..value.start]);
                    out.push_str(&redacted);
                    copied_through = value.end;
                    cursor = value.end;
                    continue;
                }
            }
        }
        cursor = token_end;
    }

    out.push_str(&input[copied_through..]);
    out
}

#[derive(Clone, Copy)]
struct ScalarRange {
    start: usize,
    end: usize,
}

fn keyed_value(input: &str, key_end: usize, key: &str) -> Option<ScalarRange> {
    let mut cursor = key_end;
    cursor = skip_horizontal_space(input, cursor);
    cursor = skip_quote_delimiter(input, cursor);
    cursor = skip_horizontal_space(input, cursor);
    let delimiter = input[cursor..].chars().next()?;
    if !matches!(delimiter, ':' | '=') {
        return None;
    }
    cursor += delimiter.len_utf8();
    cursor = skip_horizontal_space(input, cursor);

    if input[cursor..].starts_with("\\\"") || input[cursor..].starts_with("\\'") {
        let quote = input[cursor + 1..]
            .chars()
            .next()
            .expect("escaped quote exists");
        let start = cursor + 2;
        let end = find_rendered_quote(input, start, quote).unwrap_or(input.len());
        return Some(ScalarRange { start, end });
    }
    if matches!(input[cursor..].chars().next(), Some('"' | '\'')) {
        let quote = input[cursor..].chars().next().expect("quote exists");
        let start = cursor + quote.len_utf8();
        let end = find_unescaped_quote(input, start, quote).unwrap_or(input.len());
        return Some(ScalarRange { start, end });
    }

    let end = if delimiter == ':' || is_authorization_key(key) {
        find_line_end(input, cursor)
    } else {
        input[cursor..]
            .find(is_unquoted_value_boundary)
            .map_or(input.len(), |offset| cursor + offset)
    };
    Some(ScalarRange { start: cursor, end })
}

fn skip_horizontal_space(input: &str, mut cursor: usize) -> usize {
    while matches!(input[cursor..].chars().next(), Some(' ' | '\t')) {
        cursor += 1;
    }
    cursor
}

fn skip_quote_delimiter(input: &str, cursor: usize) -> usize {
    if input[cursor..].starts_with("\\\"") || input[cursor..].starts_with("\\'") {
        cursor + 2
    } else if matches!(input[cursor..].chars().next(), Some('"' | '\'')) {
        cursor + 1
    } else {
        cursor
    }
}

fn find_unescaped_quote(input: &str, start: usize, quote: char) -> Option<usize> {
    let mut backslashes = 0;
    for (offset, character) in input[start..].char_indices() {
        if character == quote && backslashes % 2 == 0 {
            return Some(start + offset);
        }
        if character == '\\' {
            backslashes += 1;
        } else {
            backslashes = 0;
        }
    }
    None
}

/// Find the closing quote in text that has itself been escaped for rendering,
/// such as `\"password\": \"value\"`. A quote preceded by three or more
/// backslashes is content escaped inside that scalar, not its terminator.
fn find_rendered_quote(input: &str, start: usize, quote: char) -> Option<usize> {
    let mut backslashes = 0;
    for (offset, character) in input[start..].char_indices() {
        if character == quote && backslashes == 1 {
            return Some(start + offset - 1);
        }
        if character == '\\' {
            backslashes += 1;
        } else {
            backslashes = 0;
        }
    }
    None
}

fn find_line_end(input: &str, start: usize) -> usize {
    let mut cursor = start;
    while cursor < input.len() {
        if input[cursor..].starts_with("\\n") || input[cursor..].starts_with("\\r") {
            return cursor;
        }
        let character = input[cursor..]
            .chars()
            .next()
            .expect("cursor remains on a character boundary");
        if matches!(character, '\n' | '\r') {
            return cursor;
        }
        cursor += character.len_utf8();
    }
    input.len()
}

const fn is_unquoted_value_boundary(character: char) -> bool {
    // Unquoted `=` assignments end at shell whitespace or common collection
    // punctuation. Values containing those characters need quotes; header and
    // YAML-style `:` values are instead consumed through the line ending.
    character.is_ascii_whitespace() || matches!(character, ',' | ';' | ')' | ']' | '}' | '"' | '\'')
}

fn redact_scalar(key: &str, value: &str) -> Option<String> {
    if is_authorization_key(key) {
        let trailing_start = value.trim_end_matches([' ', '\t']).len();
        let (credential, trailing) = value.split_at(trailing_start);
        let scheme_end = credential.find(char::is_whitespace);
        if let Some(scheme_end) = scheme_end {
            let scheme = &credential[..scheme_end];
            let separator_end = credential[scheme_end..]
                .find(|character: char| !character.is_whitespace())
                .map_or(credential.len(), |offset| scheme_end + offset);
            if is_preserved_authorization_scheme(scheme) && separator_end < credential.len() {
                return Some(format!(
                    "{}{}{}{}",
                    scheme,
                    &credential[scheme_end..separator_end],
                    REDACTED,
                    trailing
                ));
            }
        }
        return (!credential.is_empty()).then(|| format!("{REDACTED}{trailing}"));
    }

    (value.trim().len() >= MIN_KEYED_SECRET_LEN).then(|| REDACTED.to_owned())
}

fn is_secret_key_name(key: &str) -> bool {
    SECRET_KEYS.contains(&key)
        || SECRET_KEY_SUFFIXES
            .iter()
            .any(|suffix| key.ends_with(suffix))
}

fn is_authorization_key(key: &str) -> bool {
    matches!(key, "authorization" | "proxy-authorization")
}

fn is_preserved_authorization_scheme(scheme: &str) -> bool {
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "bearer" | "basic" | "digest" | "negotiate" | "token"
    )
}

fn redact_intrinsic_tokens(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(next) = rest.chars().next() {
        if is_token_char(next) {
            let end = rest.find(|c| !is_token_char(c)).unwrap_or(rest.len());
            let (token, tail) = rest.split_at(end);
            if is_intrinsic_secret(token) {
                out.push_str(REDACTED);
            } else {
                out.push_str(token);
            }
            rest = tail;
        } else {
            out.push(next);
            rest = &rest[next.len_utf8()..];
        }
    }
    out
}

/// Remove complete PEM/OpenSSH/PGP private-key blocks before token scanning.
/// Their base64 bodies have no vendor prefix, and JSON rendering may represent
/// their newlines as `\n`, so line-based token heuristics cannot recognize them
/// reliably. An unterminated block is redacted through EOF rather than risking
/// disclosure of a damaged or partially copied key.
fn redact_private_key_blocks(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    const BEGIN: &str = "-----BEGIN ";
    const MARKER_END: &str = "-----";

    while let Some(relative_begin) = input[cursor..].find(BEGIN) {
        let begin = cursor + relative_begin;
        let label_start = begin + BEGIN.len();
        let line_end = find_line_end(input, label_start);
        let Some(relative_label_end) = input[label_start..line_end].find(MARKER_END) else {
            // A damaged private-key BEGIN marker must redact its body through
            // EOF. A non-private marker still yields to a later valid key.
            if input[label_start..line_end].contains("PRIVATE KEY") {
                out.push_str(&input[cursor..begin]);
                out.push_str(REDACTED);
                return out;
            }
            out.push_str(&input[cursor..label_start]);
            cursor = label_start;
            continue;
        };
        let label_end = label_start + relative_label_end;
        let label = &input[label_start..label_end];
        let begin_end = label_end + MARKER_END.len();
        if !label.contains("PRIVATE KEY") {
            out.push_str(&input[cursor..begin_end]);
            cursor = begin_end;
            continue;
        }

        out.push_str(&input[cursor..begin]);
        out.push_str(REDACTED);
        let end_marker = format!("-----END {label}-----");
        let Some(relative_end) = input[begin_end..].find(&end_marker) else {
            return out;
        };
        cursor = begin_end + relative_end + end_marker.len();
    }

    out.push_str(&input[cursor..]);
    out
}

/// Redact URLs whose authority embeds userinfo. Database, message-broker,
/// proxy, and ordinary HTTP URLs all use this standard syntax for passwords or
/// bearer tokens, and retaining the hostname is less important than making a
/// copied connection string safe to share.
fn redact_credential_urls(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(relative_separator) = input[cursor..].find("://") {
        let separator = cursor + relative_separator;
        let scheme_start = input[..separator]
            .rfind(|character: char| !is_url_scheme_char(character))
            .map_or(0, |index| index + 1);
        let scheme = &input[scheme_start..separator];
        if scheme.is_empty()
            || !scheme
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic())
        {
            out.push_str(&input[cursor..separator + 3]);
            cursor = separator + 3;
            continue;
        }

        let url_end = find_url_end(input, separator + 3);
        let candidate = &input[scheme_start..url_end];
        let has_credentials = url::Url::parse(candidate)
            .ok()
            .is_some_and(|url| !url.username().is_empty() || url.password().is_some());
        if has_credentials {
            out.push_str(&input[cursor..scheme_start]);
            out.push_str(REDACTED);
        } else {
            out.push_str(&input[cursor..url_end]);
        }
        cursor = url_end;
    }

    out.push_str(&input[cursor..]);
    out
}

/// Find a URL boundary without mistaking brackets around an IPv6 authority
/// host for surrounding prose delimiters.
fn find_url_end(input: &str, authority_start: usize) -> usize {
    let mut in_ipv6_host = false;
    for (offset, character) in input[authority_start..].char_indices() {
        match character {
            '[' => in_ipv6_host = true,
            ']' if in_ipv6_host => in_ipv6_host = false,
            _ if !in_ipv6_host && is_url_boundary(character) => return authority_start + offset,
            _ => {}
        }
    }
    input.len()
}

const fn is_url_scheme_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
}

const fn is_url_boundary(character: char) -> bool {
    character.is_ascii_whitespace()
        || matches!(
            character,
            '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '`' | '\\'
        )
}

/// Whether a character continues the current token. `:` and `=` are excluded
/// so `key: value` and `KEY=value` each stay two tokens; `.` `/` `+` `~` are
/// included so JWTs, base64 payloads, and `ya29.` tokens are not split into
/// fragments.
const fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '~')
}

fn is_intrinsic_secret(token: &str) -> bool {
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
    pub(crate) events: Vec<SequencedAgentEvent>,
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
    let _ = writeln!(out, "# Tidebreak chat debug bundle\n");
    let _ = writeln!(
        out,
        "Generated {} by Tidebreak {} on {}/{}.\n",
        environment.generated_at.to_rfc3339(),
        environment.app_version,
        environment.os,
        environment.arch,
    );
    let _ = writeln!(
        out,
        "This bundle contains the full conversation — every message, tool \
         argument, tool result, and file path in this chat. Credential \
         redaction is best-effort, including common tokens, credential URLs, \
         and private-key blocks. Read the entire bundle before sharing it.\n",
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
        chat.permission_mode
            .map_or("ask (default)", tidebreak_core::PermissionMode::as_str),
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

fn serde_plain_status(status: tidebreak_core::model::TurnRunStatus) -> String {
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

fn role_label(role: tidebreak_core::model::Role) -> String {
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

fn coalesce(events: &[SequencedAgentEvent]) -> Vec<JournalEntry<'_>> {
    let mut entries: Vec<JournalEntry<'_>> = Vec::new();
    for SequencedAgentEvent { seq, event } in events {
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

fn render_journal(out: &mut String, events: &[SequencedAgentEvent], limit: BundleLimit) {
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
    let filename = format!("tidebreak-chat-{}.md", input.chat.id);
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
    let chat_id = SessionId(chat_id);
    let store = host_access
        .store()
        .ok_or_else(|| "Tidebreak is still starting".to_owned())?;
    let chat = store
        .get_chat(chat_id)
        .await
        .map_err(|_| "Could not read this conversation".to_owned())?
        .ok_or_else(|| "This conversation no longer exists".to_owned())?;
    // Turn history is best-effort: a store without durable turn state still
    // produces a useful journal-and-messages bundle.
    let turns = store.list_turns(chat_id).await.unwrap_or_default();
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
    if let Some(window) = app.get_window("main") {
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
    let parent = destination
        .parent()
        .ok_or_else(|| "The save destination is invalid".to_owned())?;
    let filename = destination
        .file_name()
        .ok_or_else(|| "The save destination is invalid".to_owned())?;
    let directory = Dir::open_ambient_dir(parent, ambient_authority())
        .map_err(|_| "Could not open the selected folder".to_owned())?;
    match directory.symlink_metadata(filename) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("The selected destination is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not inspect the selected destination".to_owned()),
    }

    let temporary = format!(".tidebreak-debug-export-{}.tmp", Uuid::new_v4());
    let result = (|| -> std::io::Result<()> {
        #[cfg(windows)]
        let mut file = create_private_windows_file(parent, &temporary)?;
        #[cfg(not(windows))]
        let mut file = {
            let mut options = OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .follow(FollowSymlinks::No);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            directory.open_with(&temporary, &options)?
        };
        #[cfg(target_os = "macos")]
        clear_macos_extended_acl(&file)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        directory.rename(&temporary, &directory, filename)?;
        #[cfg(unix)]
        directory.open(".")?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result.map_err(|_| "Could not write the debug bundle".to_owned())
}

/// macOS inherits extended ACL entries even when the new file has mode 0600.
/// Clear and verify them before sensitive content reaches the file.
#[cfg(target_os = "macos")]
fn clear_macos_extended_acl(file: &cap_std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;

    let acl = unsafe { acl_init(1) };
    if acl.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let status = unsafe { acl_set_fd_np(file.as_raw_fd(), acl, ACL_TYPE_EXTENDED) };
    unsafe { acl_free(acl) };
    if status != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if macos_file_has_extended_acl(file)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the selected filesystem did not clear inherited export ACLs",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_file_has_extended_acl(file: &cap_std::fs::File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd as _;

    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(false)
        } else {
            Err(error)
        };
    }
    let mut entry = std::ptr::null_mut();
    let status = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &mut entry) };
    unsafe { acl_free(acl) };
    match status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(std::io::Error::last_os_error()),
    }
}

#[cfg(target_os = "macos")]
const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
#[cfg(target_os = "macos")]
const ACL_FIRST_ENTRY: libc::c_int = 0;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_init(count: libc::c_int) -> *mut libc::c_void;
    fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    fn acl_set_fd_np(fd: libc::c_int, acl: *mut libc::c_void, acl_type: libc::c_int)
        -> libc::c_int;
    fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut libc::c_void;
    fn acl_get_entry(
        acl: *mut libc::c_void,
        entry_id: libc::c_int,
        entry: *mut *mut libc::c_void,
    ) -> libc::c_int;
}

/// Create the temporary export with a protected DACL before any sensitive byte
/// is written. `OW` is the Windows Owner Rights SID; the creator owns this new
/// file, while LocalSystem and administrators retain recovery access.
#[cfg(windows)]
fn create_private_windows_file(parent: &Path, temporary: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{FromRawHandle as _, RawHandle};

    use windows_sys::Win32::Foundation::{
        LocalFree, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_NONE,
    };

    let path: Vec<u16> = parent
        .join(temporary)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let sddl: Vec<u16> = "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: Windows owns the descriptor allocated by the conversion call;
    // it remains live through CreateFileW and is released exactly once below.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let handle = CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_NONE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        let result = if handle == INVALID_HANDLE_VALUE {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(std::fs::File::from_raw_handle(handle as RawHandle))
        };
        LocalFree(descriptor);
        let file = result?;
        if !windows_file_has_private_dacl(&file)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the selected filesystem did not preserve the private export ACL",
            ));
        }
        Ok(file)
    }
}

#[cfg(windows)]
fn windows_file_has_private_dacl(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        GetAce, GetSecurityDescriptorControl, IsWellKnownSid, WinBuiltinAdministratorsSid,
        WinCreatorOwnerRightsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    let mut dacl = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: GetSecurityInfo owns the descriptor allocation. Every pointer
    // read below is part of that descriptor and remains live until LocalFree.
    unsafe {
        let status = GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        let result = (|| -> std::io::Result<bool> {
            let mut control = 0;
            let mut revision = 0;
            if GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if control & SE_DACL_PROTECTED == 0 || dacl.is_null() || (*dacl).AceCount != 3 {
                return Ok(false);
            }

            let mut allowed = [false; 3];
            for index in 0..u32::from((*dacl).AceCount) {
                let mut raw_ace = std::ptr::null_mut();
                if GetAce(dacl, index, &mut raw_ace) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let ace = &*raw_ace.cast::<ACCESS_ALLOWED_ACE>();
                if u32::from(ace.Header.AceType) != ACCESS_ALLOWED_ACE_TYPE
                    || ace.Header.AceFlags != 0
                    || ace.Mask != FILE_ALL_ACCESS
                {
                    return Ok(false);
                }
                let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
                let slot = if IsWellKnownSid(sid, WinLocalSystemSid) != 0 {
                    0
                } else if IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0 {
                    1
                } else if IsWellKnownSid(sid, WinCreatorOwnerRightsSid) != 0 {
                    2
                } else {
                    return Ok(false);
                };
                if allowed[slot] {
                    return Ok(false);
                }
                allowed[slot] = true;
            }
            Ok(allowed.into_iter().all(|present| present))
        })();
        LocalFree(descriptor);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::error::AgentErrorInfo;
    use tidebreak_core::id::{CallId, MessageId, TurnId};
    use tidebreak_core::model::{NetworkPolicy, Role, TurnRunStatus};
    use tidebreak_core::provider::Usage;
    use tidebreak_core::tool::ToolOutput;
    use tidebreak_core::PermissionMode;
    use tidebreak_core::{AgentRunId, ProjectId};

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("a valid timestamp")
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// A chat that failed a turn after one tool call, which is the shape most
    /// bug reports actually have.
    fn sample_input() -> BundleInput {
        let chat_id = SessionId(uuid(1));
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
                memory_incognito: false,
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
                invoked_skills: Vec::new(),
                voice_input_used: false,
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
                content: "run git status in ~/code/tidebreak".to_owned(),
                llm_content: None,
                created_at: at(0),
            }],
            events: vec![
                SequencedAgentEvent {
                    seq: 1,
                    event: AgentEvent::TurnStarted { turn_id },
                },
                SequencedAgentEvent {
                    seq: 2,
                    event: AgentEvent::TextDelta {
                        text: "Check".to_owned(),
                    },
                },
                SequencedAgentEvent {
                    seq: 3,
                    event: AgentEvent::TextDelta {
                        text: "ing.".to_owned(),
                    },
                },
                SequencedAgentEvent {
                    seq: 4,
                    event: AgentEvent::ToolCallStarted {
                        call_id,
                        name: "exec".to_owned(),
                    },
                },
                SequencedAgentEvent {
                    seq: 5,
                    event: AgentEvent::ToolCallArgsDelta {
                        call_id,
                        fragment: "{\"command\":".to_owned(),
                    },
                },
                SequencedAgentEvent {
                    seq: 6,
                    event: AgentEvent::ToolCallArgsDelta {
                        call_id,
                        fragment: "\"git status\"}".to_owned(),
                    },
                },
                SequencedAgentEvent {
                    seq: 7,
                    event: AgentEvent::ToolCallCompleted {
                        call_id,
                        output: ToolOutput::text("On branch main"),
                        action: None,
                        result: None,
                    },
                },
                SequencedAgentEvent {
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
        assert!(bundle.contains("Tidebreak 1.2.3 on macos/aarch64"));
        assert!(bundle.contains("`anthropic::claude-sonnet-4-5`"));
        assert!(bundle.contains("| Permission mode | auto |"));
        // The user's prompt, which lives in messages rather than the journal.
        assert!(bundle.contains("run git status in ~/code/tidebreak"));
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
            // Vendor-issued prefixes are assembled after this table so the
            // source never contains a contiguous token a secret scanner
            // treats as live.
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
            (
                "Authorization: ApiKey supersecretvalue",
                "Authorization: [redacted]",
            ),
            (
                "password=\"correct horse battery staple\"",
                "password=\"[redacted]\"",
            ),
            ("secret=p@$$w0rd:with/slashes", "secret=[redacted]"),
            (
                r#"{\"proxy-authorization\": \"Negotiate opaque credential value\"}"#,
                r#"{\"proxy-authorization\": \"Negotiate [redacted]\"}"#,
            ),
            (
                r#"{\"password\": \"correct \\\"horse\\\" battery staple\"}"#,
                r#"{\"password\": \"[redacted]\"}"#,
            ),
            // Ordinary conversation and file paths are left alone: the chat
            // is the diagnostic.
            (
                "please read /Users/ada/code/tidebreak/README.md",
                "please read /Users/ada/code/tidebreak/README.md",
            ),
            ("the exit code was 1", "the exit code was 1"),
        ];
        for (input, expected) in cases {
            assert_eq!(scrub_credentials(input), expected, "scrubbing {input:?}");
        }

        // Assembled rather than written out: a contiguous vendor token, JSON
        // api-key pair, JWT literal, or Negotiate blob in this file fails the
        // repository's secret-scan lane.
        let negotiate = format!(
            "Authorization: Negotiate {}{}=",
            "YIIG", "eAYGKwYBBQUCoIIGbDCCBmg",
        );
        assert_eq!(
            scrub_credentials(&negotiate),
            "Authorization: Negotiate [redacted]",
            "scrubbing {negotiate:?}"
        );
        let anthropic = format!("key is sk-ant-api03-{}", "AAAABBBBCCCCDDDD");
        assert_eq!(
            scrub_credentials(&anthropic),
            "key is [redacted]",
            "scrubbing {anthropic:?}"
        );
        let github = format!("token=ghp_{}", "AAAABBBBCCCCDDDDEEEE");
        assert_eq!(
            scrub_credentials(&github),
            "token=[redacted]",
            "scrubbing {github:?}"
        );
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

    #[test]
    fn private_key_blocks_and_credential_urls_are_redacted() {
        for label in [
            "PRIVATE KEY",
            "RSA PRIVATE KEY",
            "OPENSSH PRIVATE KEY",
            "PGP PRIVATE KEY BLOCK",
        ] {
            let begin = format!("-----BEGIN {label}-----");
            let end = format!("-----END {label}-----");
            let private_key = format!("before\n{begin}\nopaque-key-material\n{end}\nafter");
            assert_eq!(
                scrub_credentials(&private_key),
                "before\n[redacted]\nafter",
                "redacting {label}"
            );
        }

        let begin = format!("-----BEGIN {} KEY-----", "OPENSSH PRIVATE");
        let unterminated = format!("before\n{begin}\nopaque-key-material");
        assert_eq!(scrub_credentials(&unterminated), "before\n[redacted]");

        let label = "PRIVATE KEY";
        let escaped = format!(
            r#"{{"output":"-----BEGIN {label}-----\nopaque-key-material\n-----END {label}-----"}}"#
        );
        assert_eq!(scrub_credentials(&escaped), r#"{"output":"[redacted]"}"#);

        let malformed_then_private = format!(
            "-----BEGIN CERTIFICATE\ntruncated\n-----BEGIN {label}-----\nLIVEPRIVATEKEYBODY\n-----END {label}-----"
        );
        assert_eq!(
            scrub_credentials(&malformed_then_private),
            "-----BEGIN CERTIFICATE\ntruncated\n[redacted]"
        );

        let damaged_private = format!("-----BEGIN {label}\nLIVEPRIVATEKEYBODY");
        assert_eq!(scrub_credentials(&damaged_private), "[redacted]");

        let password = "correct-horse-battery-staple";
        let connection = format!("database=postgres://alice:{password}@db.example.test/tidebreak");
        assert_eq!(scrub_credentials(&connection), "database=[redacted]");

        let ipv4_connection = format!("database=postgres://alice:{password}@127.0.0.1/tidebreak");
        assert_eq!(scrub_credentials(&ipv4_connection), "database=[redacted]");

        let ipv6_connection = format!("database=postgres://alice:{password}@[::1]/tidebreak");
        assert_eq!(scrub_credentials(&ipv6_connection), "database=[redacted]");

        let token_url = format!(
            "remote=https://ghp_{}@github.com/tidebreak.git",
            "abcdefghijklmnopqrstuvwxyz"
        );
        assert_eq!(scrub_credentials(&token_url), "remote=[redacted]");

        assert_eq!(
            scrub_credentials("docs=https://example.test/reference"),
            "docs=https://example.test/reference"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn debug_bundle_export_clears_inherited_macos_acl() {
        use std::process::Command;

        let directory = tempfile::tempdir().unwrap();
        let acl =
            "everyone allow read,readattr,readextattr,readsecurity,file_inherit,directory_inherit";
        assert!(Command::new("chmod")
            .args(["+a", acl, directory.path().to_str().unwrap()])
            .status()
            .unwrap()
            .success());

        let destination = directory.path().join("bundle.md");
        write_bundle(&destination, b"private").unwrap();
        let file = Dir::open_ambient_dir(directory.path(), ambient_authority())
            .unwrap()
            .open("bundle.md")
            .unwrap();
        assert!(!macos_file_has_extended_acl(&file).unwrap());
    }

    #[test]
    fn debug_bundle_export_is_private_and_atomic() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("bundle.md");
        write_bundle(&destination, b"first").unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"first");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_bundle(&destination, b"replacement").unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"replacement");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&destination)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        #[cfg(windows)]
        {
            let file = std::fs::File::open(&destination).unwrap();
            assert!(windows_file_has_private_dacl(&file).unwrap());
        }
    }

    #[cfg(unix)]
    #[test]
    fn debug_bundle_export_rejects_destination_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.md");
        let destination = directory.path().join("bundle.md");
        std::fs::write(&target, b"private target").unwrap();
        symlink(&target, &destination).unwrap();

        assert!(write_bundle(&destination, b"replacement").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"private target");
    }

    /// The scrubber runs over the rendered document, not only over the
    /// journal, so a key echoed in a stored provider error body is gone too.
    #[test]
    fn scrubbing_covers_the_rendered_document() {
        let mut input = sample_input();
        let leaked = format!("sk-ant-api03-{}", "LIVEKEY0123456789");
        input.turns[0].last_error_detail = Some(format!("401: header x-api-key {leaked}"));
        let bundle = render_bundle(&environment(), &input, BundleLimit::Clipboard);
        assert!(!bundle.contains(&leaked));
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
