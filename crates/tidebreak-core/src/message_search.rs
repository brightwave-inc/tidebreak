//! Full-text message search: how conversation text becomes search terms, how
//! a query becomes a match expression, and what a search answers.
//!
//! Both database backends index the same thing: the terms this module derives
//! from a piece of text, written as one space-separated string. SQLite feeds
//! that string to an FTS5 table with the `ascii` tokenizer, which splits only
//! on the spaces written here. PostgreSQL casts it to a `tsvector`, which takes
//! each term verbatim. Neither backend's own word rules, stemming, or stop
//! words run, so a query matches the same rows on both, and [`SearchTerms`]
//! finds the same words again when it cuts a snippet.
//!
//! A term is a run of letters and digits, folded: compatibility-decomposed,
//! stripped of combining marks, and lowercased, so `Café` and `cafe` are one
//! term and `ﬁle` is `file`. A Han or kana character is a term by itself,
//! because those scripts do not put spaces between words. Everything else
//! separates terms. A query's quotes, parentheses, `*`, `-`, and `:` are
//! therefore never operators, and `AND`, `OR`, `NOT`, and `NEAR` are ordinary
//! words: every term must appear, and the last one also matches as a prefix.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use unicode_normalization::char::{decompose_compatible, is_combining_mark};

use crate::code::{Event, ToolDetail, WorkspaceId};
use crate::id::{MessageId, SessionId, TurnId};

/// How much of one piece of text is indexed, in characters. Text past this
/// is stored with its message but is not searchable.
pub const MAX_INDEXED_CHARS: usize = 65_536;
/// Longest term, in folded characters. A longer run of letters and digits is
/// indexed as its first this-many characters, so a prefix still finds it.
pub const MAX_TERM_CHARS: usize = 64;
/// Longest query the search route accepts, in characters.
pub const MAX_QUERY_CHARS: usize = 500;
/// Most terms one query matches on. Past this, the query keeps its first
/// terms and its last one, which is the one being typed.
pub const MAX_QUERY_TERMS: usize = 12;
/// Shortest last word that matches as a prefix. A one-letter prefix matches
/// nearly every row, so one letter matches only itself.
pub const MIN_PREFIX_CHARS: usize = 2;
/// Hits one search answers when the caller names no limit.
pub const DEFAULT_SEARCH_LIMIT: u32 = 20;
/// Most hits one search answers.
pub const MAX_SEARCH_LIMIT: u32 = 50;
/// Longest snippet, in characters, before the ellipses.
pub const SNIPPET_CHARS: usize = 240;
/// How much text a snippet keeps before its first match, in characters.
pub const SNIPPET_LEAD_CHARS: usize = 60;

/// The journal event types that can carry searchable text. Everything else
/// in a code session's journal is skipped without being decoded.
pub const INDEXED_EVENT_TYPES: &[&str] = &[
    "assistant_message",
    "user_steered",
    "tool_started",
    "tool_completed",
    "background_activity",
];

/// Which kind of conversation a hit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MessageSearchKind {
    /// A Work chat. Open it with `session_id` as the chat id.
    Chat,
    /// A code session. Open it with `session_id`, inside `workspace_id` when
    /// the session has a workspace.
    Code,
}

/// Who wrote the matched text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MessageSearchSource {
    /// A message the person sent, or text they steered a running turn with.
    User,
    /// Text the assistant wrote.
    Assistant,
    /// What a code session's tool call acted on: a command, a path, or a
    /// search query.
    Tool,
}

impl MessageSearchSource {
    /// Stable database representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }

    /// Parse the database representation.
    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }
}

/// One matched word in a snippet.
///
/// `start` and `end` count UTF-16 code units into the snippet, so
/// `snippet.slice(start, end)` in JavaScript is the matched text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MessageSearchRange {
    /// First code unit of the match.
    pub start: u32,
    /// One past the last code unit of the match.
    pub end: u32,
}

/// One message, event, or tool call that matched a search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MessageSearchHit {
    /// Whether the hit is in a Work chat or a code session.
    pub kind: MessageSearchKind,
    /// The chat or code session that holds the hit.
    pub session_id: SessionId,
    /// The workspace a code session belongs to, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<WorkspaceId>,
    /// The conversation's title, or its workspace's title when the session
    /// has none of its own. Absent for a conversation nobody has named yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    /// The turn that holds the hit, as the transcript shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<TurnId>,
    /// The chat message to scroll to. Chat hits only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message_id: Option<MessageId>,
    /// The code journal event to scroll to: its sequence number in the
    /// session's journal. Code hits from the journal only; a code session's
    /// own message carries `turn_id` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub event_seq: Option<i64>,
    /// Who wrote the matched text.
    pub source: MessageSearchSource,
    /// Plain text around the first match, never markup. It starts or ends
    /// with `…` where it was cut.
    pub snippet: String,
    /// The matched words in `snippet`, in order.
    pub ranges: Vec<MessageSearchRange>,
    /// When the matched message or event was written.
    pub created_at: DateTime<Utc>,
    /// Whether the conversation, or the workspace a code session belongs to,
    /// is archived.
    pub archived: bool,
}

/// How far the index has caught up with conversations that existed before
/// it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MessageSearchIndexing {
    /// `false` while some of the caller's older conversations are still being
    /// added. Until then a search can miss matches in them.
    pub complete: bool,
    /// How many of the caller's conversations are still waiting to be added.
    pub pending_conversations: u64,
}

/// One page of search results, newest match first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MessageSearchPage {
    /// The matches on this page.
    pub hits: Vec<MessageSearchHit>,
    /// Pass this back as `cursor` to read the next page. Absent on the last
    /// page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
    /// Whether the index covers every conversation yet.
    pub indexing: MessageSearchIndexing,
}

/// A search as the store runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSearchRequest {
    /// The words to find, as the person typed them.
    pub query: String,
    /// Most hits to answer.
    pub limit: u32,
    /// Where the previous page stopped.
    pub cursor: Option<MessageSearchCursor>,
}

/// Where a page of results stopped: the last hit's time and index row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageSearchCursor {
    /// The last hit's time, in microseconds since the Unix epoch.
    pub created_at_micros: i64,
    /// The last hit's index row.
    pub row: i64,
}

impl MessageSearchCursor {
    /// The opaque form a client passes back.
    #[must_use]
    pub fn encode(self) -> String {
        format!("{}.{}", self.created_at_micros, self.row)
    }

    /// Read a cursor this module encoded, or `None` for anything else.
    #[must_use]
    pub fn decode(value: &str) -> Option<Self> {
        let (at, row) = value.split_once('.')?;
        let cursor = Self {
            created_at_micros: at.parse().ok()?,
            row: row.parse().ok()?,
        };
        (cursor.encode() == value).then_some(cursor)
    }
}

/// Whether `c` is a term by itself: a Han or kana character.
fn stands_alone(c: char) -> bool {
    matches!(
        u32::from(c),
        0x3040..=0x30FF // Hiragana and Katakana
            | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
            | 0x4E00..=0x9FFF // CJK Unified Ideographs
            | 0xF900..=0xFAFF // CJK Compatibility Ideographs
            | 0x2_0000..=0x3_134F // Extensions B through G and the supplement
    )
}

/// Append the folded form of `c` to `out`: compatibility-decomposed, without
/// combining marks, lowercased, and only letters and digits.
fn fold_into(c: char, out: &mut String) {
    if c.is_ascii() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        }
        return;
    }
    decompose_compatible(c, |part| {
        if is_combining_mark(part) {
            return;
        }
        for lower in part.to_lowercase() {
            if lower.is_alphanumeric() {
                out.push(lower);
            }
        }
    });
}

/// One term, with where it sits in the text it came from.
struct Token {
    /// Index of its first character.
    start: usize,
    /// Index one past its last character.
    end: usize,
    /// The folded term, at most [`MAX_TERM_CHARS`] characters.
    folded: String,
}

fn push_token(out: &mut Vec<Token>, chars: &[char], start: usize, end: usize) {
    let mut folded = String::new();
    for &c in &chars[start..end] {
        fold_into(c, &mut folded);
    }
    if let Some((cut, _)) = folded.char_indices().nth(MAX_TERM_CHARS) {
        folded.truncate(cut);
    }
    if !folded.is_empty() {
        out.push(Token { start, end, folded });
    }
}

/// Split `chars` into terms.
fn tokens(chars: &[char]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for (index, &c) in chars.iter().enumerate() {
        if stands_alone(c) {
            if let Some(start) = open.take() {
                push_token(&mut out, chars, start, index);
            }
            push_token(&mut out, chars, index, index + 1);
        } else if c.is_alphanumeric() || (open.is_some() && is_combining_mark(c)) {
            open.get_or_insert(index);
        } else if let Some(start) = open.take() {
            push_token(&mut out, chars, start, index);
        }
    }
    if let Some(start) = open {
        push_token(&mut out, chars, start, chars.len());
    }
    out
}

/// The terms the index stores for `text`, space-separated, in order.
///
/// Empty when the text holds nothing searchable, and then nothing is indexed.
#[must_use]
pub fn index_terms(text: &str) -> String {
    let chars: Vec<char> = text.chars().take(MAX_INDEXED_CHARS).collect();
    let mut terms = String::new();
    for token in tokens(&chars) {
        if !terms.is_empty() {
            terms.push(' ');
        }
        terms.push_str(&token.folded);
    }
    terms
}

/// One term of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTerm {
    /// The folded term.
    pub text: String,
    /// Whether it also matches longer terms that start with it.
    pub prefix: bool,
}

/// A query, read as literal words.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchTerms(Vec<SearchTerm>);

impl SearchTerms {
    /// Read `query` as words. Every word must match, and the last one also
    /// matches as a prefix when it has at least [`MIN_PREFIX_CHARS`]
    /// characters.
    #[must_use]
    pub fn parse(query: &str) -> Self {
        let chars: Vec<char> = query.chars().take(MAX_QUERY_CHARS).collect();
        let tokens = tokens(&chars);
        let last = tokens.len().checked_sub(1);
        let mut terms: Vec<SearchTerm> = Vec::new();
        for (index, token) in tokens.into_iter().enumerate() {
            let prefix =
                Some(index) == last && token.folded.chars().count() >= MIN_PREFIX_CHARS;
            let term = SearchTerm {
                text: token.folded,
                prefix,
            };
            if !terms.contains(&term) {
                terms.push(term);
            }
        }
        if terms.len() > MAX_QUERY_TERMS {
            let typing = terms.pop();
            terms.truncate(MAX_QUERY_TERMS - 1);
            terms.extend(typing);
        }
        Self(terms)
    }

    /// Whether the query holds no searchable word at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The terms, in query order.
    #[must_use]
    pub fn terms(&self) -> &[SearchTerm] {
        &self.0
    }

    /// The FTS5 `MATCH` expression: each term a quoted string, all of them
    /// required. A quoted string is never an operator, and the terms hold no
    /// quote to escape, but one would be doubled all the same.
    #[must_use]
    pub fn fts5_match(&self) -> String {
        self.0
            .iter()
            .map(|term| {
                let quoted = term.text.replace('"', "\"\"");
                if term.prefix {
                    format!("\"{quoted}\"*")
                } else {
                    format!("\"{quoted}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The PostgreSQL `tsquery` literal: each term a quoted lexeme, all of
    /// them required. The literal is cast, not parsed by a text search
    /// configuration, so no dictionary rewrites a term.
    #[must_use]
    pub fn tsquery(&self) -> String {
        self.0
            .iter()
            .map(|term| {
                let quoted = term.text.replace('\\', "\\\\").replace('\'', "''");
                if term.prefix {
                    format!("'{quoted}':*")
                } else {
                    format!("'{quoted}'")
                }
            })
            .collect::<Vec<_>>()
            .join(" & ")
    }

    /// How many folded characters of `token` a term matches: all of them for
    /// an exact match, the term's length for a prefix, `None` for no match.
    fn matched_chars(&self, token: &str) -> Option<usize> {
        let token_chars = token.chars().count();
        self.0
            .iter()
            .filter_map(|term| {
                if term.text == token {
                    Some(token_chars)
                } else if term.prefix && token.starts_with(&term.text) {
                    Some(term.text.chars().count())
                } else {
                    None
                }
            })
            .max()
    }

    /// Plain text around the first match in `text`, with every matched word
    /// in it.
    ///
    /// Runs of whitespace become one space. The excerpt keeps up to `lead`
    /// characters before the first match and is at most `width` characters
    /// long, cut at spaces, with `…` where it was cut. Text with no match
    /// gives its start and no ranges.
    #[must_use]
    pub fn excerpt(&self, text: &str, width: usize, lead: usize) -> Excerpt {
        let capped: String = text.chars().take(MAX_INDEXED_CHARS).collect();
        let compact: Vec<char> = capped
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .collect();
        let mut matches: Vec<(usize, usize)> = Vec::new();
        for token in tokens(&compact) {
            let Some(matched) = self.matched_chars(&token.folded) else {
                continue;
            };
            let end = if matched >= token.folded.chars().count() {
                token.end
            } else {
                prefix_end(&compact, token.start, token.end, matched)
            };
            matches.push((token.start, end));
        }
        let len = compact.len();
        let (first_start, first_end) = matches.first().copied().unwrap_or((0, 0));
        let mut start = first_start.saturating_sub(lead);
        if start > 0 && !compact[start - 1].is_whitespace() {
            // Start at a word, not inside one.
            start = compact[start..first_start]
                .iter()
                .position(|c| c.is_whitespace())
                .map_or(first_start, |space| start + space + 1);
        }
        let mut end = (start + width).max(first_end).min(len);
        if end < len && !compact[end].is_whitespace() {
            // End after a word, not inside one.
            let floor = first_end.max(start);
            if let Some(space) = compact[floor..end].iter().rposition(|c| c.is_whitespace()) {
                end = floor + space;
            }
        }
        let mut units = Vec::with_capacity(len + 1);
        let mut total = 0_u32;
        units.push(total);
        for c in &compact {
            total = total.saturating_add(u32::try_from(c.len_utf16()).unwrap_or(2));
            units.push(total);
        }
        let mut snippet = String::new();
        let mut lead_units = 0_u32;
        if start > 0 {
            snippet.push('…');
            lead_units = 1;
        }
        snippet.extend(&compact[start..end]);
        if end < len {
            snippet.push('…');
        }
        let mut ranges: Vec<MessageSearchRange> = Vec::new();
        for (match_start, match_end) in matches {
            if match_start < start || match_end > end {
                continue;
            }
            let range = MessageSearchRange {
                start: lead_units + units[match_start] - units[start],
                end: lead_units + units[match_end] - units[start],
            };
            match ranges.last_mut() {
                Some(previous) if previous.end >= range.start => {
                    previous.end = previous.end.max(range.end);
                }
                _ => ranges.push(range),
            }
        }
        Excerpt {
            text: snippet,
            ranges,
        }
    }
}

/// Where a prefix match of `matched` folded characters ends in the original
/// word `chars[start..end]`, keeping any combining marks on its last letter.
fn prefix_end(chars: &[char], start: usize, end: usize, matched: usize) -> usize {
    let mut folded = 0;
    let mut scratch = String::new();
    let mut cut = end;
    for (index, &c) in chars.iter().enumerate().take(end).skip(start) {
        scratch.clear();
        fold_into(c, &mut scratch);
        folded += scratch.chars().count();
        if folded >= matched {
            cut = index + 1;
            break;
        }
    }
    while cut < end && is_combining_mark(chars[cut]) {
        cut += 1;
    }
    cut
}

/// A snippet and the matched words in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    /// Plain text.
    pub text: String,
    /// The matched words, as UTF-16 code unit ranges into `text`.
    pub ranges: Vec<MessageSearchRange>,
}

/// What one code journal event puts in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEventText<'a> {
    /// Who wrote it.
    pub source: MessageSearchSource,
    /// The text to index and cut snippets from.
    pub text: &'a str,
    /// The tool call it describes. A call's completion restates what it
    /// acted on once its arguments are whole, and replaces what its start
    /// said.
    pub call_id: Option<&'a str>,
    /// Whether the engine did this outside the person's turn.
    pub background: bool,
}

/// What a tool call acted on: the command, path, or query, never its output.
fn tool_subject(detail: &ToolDetail) -> &str {
    match detail {
        ToolDetail::Command { cmd, .. } => cmd,
        ToolDetail::FileEdit { path } | ToolDetail::FileRead { path } => path,
        ToolDetail::Search { query } => query,
        ToolDetail::Other { summary } => summary,
    }
}

/// The searchable text one code journal event carries: the assistant's
/// message, the person's steer, or what a tool call acted on. `None` for
/// everything else, streamed deltas and reasoning included.
#[must_use]
pub fn code_event_text(event: &Event) -> Option<CodeEventText<'_>> {
    let (source, text, call_id): (MessageSearchSource, &str, Option<&str>) = match event {
        Event::AssistantMessage { text, .. } => (MessageSearchSource::Assistant, text, None),
        Event::UserSteered { text, .. } => (MessageSearchSource::User, text, None),
        Event::ToolStarted {
            call_id, detail, ..
        }
        | Event::ToolCompleted {
            call_id,
            detail: Some(detail),
            ..
        } => (
            MessageSearchSource::Tool,
            tool_subject(detail),
            Some(call_id),
        ),
        Event::BackgroundActivity { event } => {
            let inner = code_event_text(event)?;
            return Some(CodeEventText {
                background: true,
                ..inner
            });
        }
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(CodeEventText {
        source,
        text,
        call_id,
        background: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn excerpt(query: &str, text: &str) -> (String, Vec<String>) {
        let terms = SearchTerms::parse(query);
        let excerpt = terms.excerpt(text, SNIPPET_CHARS, SNIPPET_LEAD_CHARS);
        let units: Vec<u16> = excerpt.text.encode_utf16().collect();
        let matched = excerpt
            .ranges
            .iter()
            .map(|range| {
                String::from_utf16(&units[range.start as usize..range.end as usize]).unwrap()
            })
            .collect();
        (excerpt.text, matched)
    }

    #[test]
    fn terms_fold_case_and_diacritics() {
        assert_eq!(index_terms("Café NAÏVE Ångström"), "cafe naive angstrom");
        assert_eq!(index_terms("ﬁle ①"), "file 1");
        // A decomposed accent folds the same way a precomposed one does.
        assert_eq!(index_terms("cafe\u{301}"), "cafe");
        assert_eq!(index_terms("İstanbul"), "istanbul");
    }

    #[test]
    fn punctuation_separates_terms() {
        assert_eq!(
            index_terms("src/main.rs --flag=\"x\" (a*b) foo_bar"),
            "src main rs flag x a b foo bar"
        );
        assert_eq!(index_terms("  \n\t "), "");
        assert_eq!(index_terms("*** \"\" ()"), "");
    }

    #[test]
    fn han_and_kana_characters_are_terms_by_themselves() {
        assert_eq!(index_terms("東京タワーで"), "東 京 タ ワ ー で");
        assert_eq!(index_terms("see 東京 now"), "see 東 京 now");
    }

    #[test]
    fn a_long_run_is_indexed_as_its_prefix() {
        let long = "a".repeat(200);
        assert_eq!(index_terms(&long).chars().count(), MAX_TERM_CHARS);
    }

    #[test]
    fn query_syntax_is_literal_text() {
        for (query, fts5, tsquery) in [
            ("\"quoted\" words", "\"quoted\" \"words\"*", "'quoted' & 'words':*"),
            ("a AND b OR NOT c", "\"a\" \"and\" \"b\" \"or\" \"not\" \"c\"", "'a' & 'and' & 'b' & 'or' & 'not' & 'c'"),
            ("NEAR(one two)", "\"near\" \"one\" \"two\"*", "'near' & 'one' & 'two':*"),
            ("-exclude col:value", "\"exclude\" \"col\" \"value\"*", "'exclude' & 'col' & 'value':*"),
            ("wild* (paren", "\"wild\" \"paren\"*", "'wild' & 'paren':*"),
            ("it's \\ back'", "\"it\" \"s\" \"back\"*", "'it' & 's' & 'back':*"),
        ] {
            let terms = SearchTerms::parse(query);
            assert_eq!(terms.fts5_match(), fts5, "{query}");
            assert_eq!(terms.tsquery(), tsquery, "{query}");
        }
        assert!(SearchTerms::parse("*** \"\" () - :").is_empty());
    }

    #[test]
    fn only_a_last_word_of_two_letters_or_more_is_a_prefix() {
        let terms = SearchTerms::parse("invoice q");
        assert_eq!(
            terms.terms(),
            [
                SearchTerm {
                    text: "invoice".into(),
                    prefix: false
                },
                SearchTerm {
                    text: "q".into(),
                    prefix: false
                },
            ]
        );
        assert!(SearchTerms::parse("inv").terms()[0].prefix);
    }

    #[test]
    fn a_long_query_keeps_the_word_being_typed() {
        let query = (0..20).map(|n| format!("w{n}")).collect::<Vec<_>>().join(" ");
        let terms = SearchTerms::parse(&query);
        assert_eq!(terms.terms().len(), MAX_QUERY_TERMS);
        let last = terms.terms().last().unwrap();
        assert_eq!(last.text, "w19");
        assert!(last.prefix);
    }

    #[test]
    fn excerpts_mark_every_match_in_utf16_units() {
        let (text, matched) = excerpt("cafe inv", "Emoji 🎉 then the Café sent an Invoice.");
        assert_eq!(text, "Emoji 🎉 then the Café sent an Invoice.");
        assert_eq!(matched, ["Café", "Inv"]);
    }

    #[test]
    fn a_prefix_match_keeps_the_accent_on_its_last_letter() {
        let (_, matched) = excerpt("cafe\u{301}s", "the cafe\u{301}s are open");
        assert_eq!(matched, ["cafe\u{301}s"]);
        let (_, matched) = excerpt("caf", "the cafe\u{301} is open");
        assert_eq!(matched, ["caf"]);
        let (_, matched) = excerpt("cafe", "the cafe\u{301}teria");
        assert_eq!(matched, ["cafe\u{301}"]);
    }

    #[test]
    fn a_long_text_is_cut_around_its_first_match_at_spaces() {
        let before = "lorem ipsum ".repeat(30);
        let after = " dolor sit".repeat(60);
        let text = format!("{before}needle{after}");
        let (snippet, matched) = excerpt("needle", &text);
        assert!(snippet.starts_with('…'), "{snippet}");
        assert!(snippet.ends_with('…'), "{snippet}");
        assert_eq!(matched, ["needle"]);
        // Cut at spaces: the words on both sides are whole.
        let inner = snippet.trim_matches('…');
        assert!(inner.starts_with("lorem") || inner.starts_with("ipsum"), "{snippet}");
        assert!(inner.ends_with("dolor") || inner.ends_with("sit"), "{snippet}");
        assert!(inner.chars().count() <= SNIPPET_CHARS);
    }

    #[test]
    fn text_without_a_match_gives_its_start() {
        let (snippet, matched) = excerpt("absent", "first words of the message");
        assert_eq!(snippet, "first words of the message");
        assert!(matched.is_empty());
    }

    #[test]
    fn cursors_round_trip_and_reject_anything_else() {
        let cursor = MessageSearchCursor {
            created_at_micros: 1_758_000_000_000_000,
            row: 42,
        };
        assert_eq!(MessageSearchCursor::decode(&cursor.encode()), Some(cursor));
        for bad in ["", "1", "1.", ".1", "a.b", "1.2.3", "01.2", "1.+2"] {
            assert_eq!(MessageSearchCursor::decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn code_events_index_messages_steers_and_tool_subjects_only() {
        let message = Event::AssistantMessage {
            text: "done".into(),
            parent_call_id: None,
        };
        assert_eq!(
            code_event_text(&message).map(|text| (text.source, text.text)),
            Some((MessageSearchSource::Assistant, "done"))
        );
        let started = Event::ToolStarted {
            call_id: "c1".into(),
            name: "Bash".into(),
            detail: ToolDetail::Command {
                cmd: "cargo test".into(),
                cwd: "/repo".into(),
            },
            parent_call_id: None,
        };
        let text = code_event_text(&started).unwrap();
        assert_eq!(
            (text.source, text.text, text.call_id),
            (MessageSearchSource::Tool, "cargo test", Some("c1"))
        );
        let background = Event::BackgroundActivity {
            event: Box::new(message),
        };
        assert!(code_event_text(&background).unwrap().background);
        for skipped in [
            Event::AssistantDelta {
                text: "partial".into(),
            },
            Event::ReasoningDelta {
                text: "thinking".into(),
            },
            Event::AssistantMessage {
                text: "  ".into(),
                parent_call_id: None,
            },
        ] {
            assert_eq!(code_event_text(&skipped), None);
        }
    }
}
