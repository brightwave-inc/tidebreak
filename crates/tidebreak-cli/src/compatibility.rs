//! The 1.x promises in the headless guide, checked against the code.
//!
//! `docs-site/content/docs/headless.mdx` says which commands, flags, and exit
//! codes stay stable across 1.x. A promise nothing checks drifts the first
//! time someone renames a flag, so these tests read the guide itself and fail
//! when it names a command, a flag, or an exit code the CLI does not have.
//! `tests/json_documents.rs` pins the JSON half of the same promise.

use std::collections::BTreeSet;

/// The guide, read from the repository this crate builds in.
fn guide() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs-site/content/docs/headless.mdx");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// One usage line, continuation lines folded in.
#[derive(Debug)]
struct Usage {
    words: Vec<String>,
    flags: BTreeSet<String>,
    text: String,
}

impl Usage {
    fn parse(text: &str) -> Self {
        let mut tokens = text.split_whitespace();
        assert_eq!(tokens.next(), Some("tidebreak"), "not a usage line: {text}");
        let mut words = Vec::new();
        for token in tokens.by_ref() {
            let command_word = token == "-p"
                || (token.starts_with(|c: char| c.is_ascii_lowercase())
                    && token
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
            if !command_word {
                break;
            }
            words.push(token.to_owned());
        }
        let flags = text
            .split_whitespace()
            .skip(1)
            .map(|token| token.trim_start_matches(['[', '(']))
            .map(|token| token.trim_end_matches([']', ')', '…', ',']))
            .filter(|token| {
                let name = token.trim_start_matches('-');
                token.starts_with('-')
                    && token.len() - name.len() <= 2
                    && name.starts_with(|c: char| c.is_ascii_lowercase())
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            })
            .filter(|token| !words.iter().any(|word| word == token))
            .map(str::to_owned)
            .collect();
        Self {
            words,
            flags,
            text: text.to_owned(),
        }
    }
}

/// Fold a usage block into one entry per command: a line that starts with
/// `tidebreak` opens an entry, and an indented line continues it.
fn usages(lines: impl Iterator<Item = impl AsRef<str>>) -> Vec<Usage> {
    let mut entries: Vec<String> = Vec::new();
    for line in lines {
        let line = line.as_ref();
        let trimmed = line.trim_start().trim_start_matches("usage:").trim();
        if trimmed.starts_with("tidebreak ") || trimmed == "tidebreak" {
            entries.push(trimmed.to_owned());
        } else if !trimmed.is_empty() && line.starts_with(char::is_whitespace) {
            if let Some(last) = entries.last_mut() {
                last.push(' ');
                last.push_str(trimmed);
            }
        }
    }
    entries.iter().map(|entry| Usage::parse(entry)).collect()
}

/// The fenced block under `## Commands`.
fn documented_commands(guide: &str) -> Vec<Usage> {
    let section = guide
        .split_once("## Commands")
        .expect("the guide has a Commands section")
        .1;
    let block = section
        .split_once("```text")
        .expect("the Commands section opens a text block")
        .1
        .split_once("```")
        .expect("the text block closes")
        .0;
    usages(block.lines())
}

/// The usage lines the CLI prints, up to the notes after them.
fn offered_commands() -> Vec<Usage> {
    usages(
        crate::help::top_usage_text()
            .lines()
            .take_while(|line| !line.trim().is_empty()),
    )
}

#[test]
fn every_documented_command_and_flag_is_one_the_cli_offers() {
    let documented = documented_commands(&guide());
    assert!(
        documented.len() > 60,
        "the Commands block looks truncated: {} entries",
        documented.len()
    );
    let offered = offered_commands();
    for usage in &documented {
        let matching: Vec<&Usage> = offered
            .iter()
            .filter(|candidate| candidate.words == usage.words)
            .collect();
        assert!(
            !matching.is_empty(),
            "the guide documents `tidebreak {}`, which the CLI does not offer",
            usage.words.join(" ")
        );
        let offered_flags: BTreeSet<&String> =
            matching.iter().flat_map(|entry| &entry.flags).collect();
        for flag in &usage.flags {
            assert!(
                offered_flags.contains(flag),
                "the guide documents {flag} on `{}`, which the CLI does not offer",
                usage.text
            );
        }
    }
}

/// The codes in the markdown table that follows `after` in the guide.
fn documented_exit_codes(guide: &str, after: &str) -> BTreeSet<i32> {
    let rest = guide
        .split_once(after)
        .unwrap_or_else(|| panic!("the guide has `{after}`"))
        .1;
    rest.lines()
        .skip_while(|line| !line.starts_with("| `"))
        .take_while(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_start_matches("| `")
                .split('`')
                .next()
                .expect("a code cell")
                .parse()
                .unwrap_or_else(|error| panic!("an exit code in `{line}`: {error}"))
        })
        .collect()
}

#[test]
fn the_documented_exit_codes_are_the_ones_the_cli_returns() {
    use crate::print::protocol::HaltReason;

    let guide = guide();
    let print_codes = documented_exit_codes(&guide, "#### Exit codes");
    assert_eq!(
        print_codes,
        BTreeSet::from([
            0,
            crate::print::EXIT_TURN_UNSUCCESSFUL,
            crate::help::EXIT_USAGE,
            crate::print::EXIT_INTERACTION_UNDRIVEN,
            crate::print::EXIT_DECISION_FAILED,
            crate::print::EXIT_INTERRUPTED,
        ])
    );
    for reason in [
        HaltReason::ApprovalDriverUnavailable,
        HaltReason::PlanUndriven,
        HaltReason::QuestionsUndriven,
        HaltReason::DecisionFailed,
        HaltReason::PendingLookupFailed,
        HaltReason::FolderDeclineFailed,
        HaltReason::Interrupted,
    ] {
        assert!(
            print_codes.contains(&reason.exit_code()),
            "{reason:?} exits with an undocumented code"
        );
    }

    let code_codes = documented_exit_codes(&guide, "add two more codes");
    assert_eq!(
        code_codes,
        BTreeSet::from([
            crate::code::EXIT_APPROVAL_PARKED,
            crate::code::EXIT_TIMEOUT,
            crate::code::EXIT_INTERRUPTED,
        ])
    );
}

/// The parser the two tests above rely on, on the shapes the guide uses.
#[test]
fn a_usage_line_splits_into_command_words_and_flags() {
    let usage = Usage::parse(
        "tidebreak code run (--session <id> | --ws <id>) [<message>] [--on-approval wait|fail]",
    );
    assert_eq!(usage.words, ["code", "run"]);
    assert_eq!(
        usage.flags,
        BTreeSet::from(["--session", "--ws", "--on-approval"].map(str::to_owned))
    );
    let print = Usage::parse("tidebreak -p <prompt> [--chat <id>]");
    assert_eq!(print.words, ["-p"]);
    assert_eq!(print.flags, BTreeSet::from(["--chat".to_owned()]));
    let deny = Usage::parse("tidebreak code deny <approval-id> [-m <feedback>]");
    assert_eq!(deny.flags, BTreeSet::from(["-m".to_owned()]));
}
