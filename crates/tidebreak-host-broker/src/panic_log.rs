//! Record a panic in the sidecar before the process dies.
//!
//! The broker is a sidecar with no log of its own. A panic writes a report —
//! the message, the location, the thread, and a backtrace — to stderr, which
//! the desktop forwards into its tracing log, and appends it to
//! [`BOOT_FAILURE_LOG`] in the private data directory it shares with the
//! desktop. The diagnostics export carries that file, so the message is
//! scrubbed first by the same rules as the server's log.

use std::io::Write as _;
use std::path::Path;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};

/// The desktop's boot failure log, in the data directory both processes use.
pub const BOOT_FAILURE_LOG: &str = "boot-failures.log";

/// Past this size a report is written without its backtrace, so a sidecar
/// that panics on every start cannot grow the log without bound.
const FULL_REPORT_LIMIT_BYTES: u64 = 1024 * 1024;

/// Replace the default panic hook with one that reports to stderr and to
/// [`BOOT_FAILURE_LOG`] under `data_dir`.
pub fn install_panic_hook(data_dir: &Path) {
    let data_dir = data_dir.to_path_buf();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let location = info.location().map_or_else(
            || "an unknown location".to_owned(),
            |location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            },
        );
        let headline = format!(
            "host broker panic in thread '{}' at {location}: {}",
            thread.name().unwrap_or("<unnamed>"),
            scrub_log_text(&payload_message(info.payload()), MAX_PANIC_MESSAGE_CHARS)
        );
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let report = full_report(&headline, &backtrace);
        eprintln!("tidebreak-host-broker: {report}");
        let _ = append(&data_dir, &headline, &report);
    }));
}

fn full_report(headline: &str, backtrace: &str) -> String {
    format!("{headline}\nbacktrace:\n{}", backtrace.trim_end())
}

/// Append the report, or only its headline once the log is large.
fn append(data_dir: &Path, headline: &str, report: &str) -> std::io::Result<()> {
    let directory = Dir::open_ambient_dir(data_dir, ambient_authority())?;
    let mut options = OpenOptions::new();
    options.create(true).append(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = directory.open_with(BOOT_FAILURE_LOG, &options)?;
    let entry = if file.metadata()?.len() < FULL_REPORT_LIMIT_BYTES {
        report
    } else {
        headline
    };
    let line = format!("{} {entry}\n", chrono::Local::now().to_rfc3339());
    file.write_all(line.as_bytes())
}

fn payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "a panic with a payload that is not text".to_owned()
    }
}

/// The most characters of a panic message a report keeps.
const MAX_PANIC_MESSAGE_CHARS: usize = 1_000;

// The scrub below follows the same rules as
// `tidebreak_server_core::logging::scrub_log_text`, which the sidecar cannot
// link. A desktop test holds the two to the same answers.

const REDACTED: &str = "[redacted]";

const SECRET_PREFIXES: [&str; 25] = [
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
    "tbreak_",
    "tidebreak-token.",
];

const MIN_SECRET_TAIL: usize = 8;

const SECRET_KEYS: [&str; 10] = [
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
    "cookie",
    "credential",
];

const AUTH_SCHEMES: [&str; 4] = ["bearer", "basic", "token", "digest"];

const BEARER: &str = "bearer";

/// Keep a panic message fit for the diagnostics export: no URL query
/// strings, fragments, or userinfo, no credential after a key or an
/// authorization scheme, no vendor, Tidebreak, or web tokens, and no more
/// than `limit` characters.
pub fn scrub_log_text(text: &str, limit: usize) -> String {
    let bounded: String = text
        .chars()
        .take(limit.saturating_mul(4).max(1024))
        .collect();
    let mut scrubbed = String::with_capacity(bounded.len());
    let mut redact_next = false;
    let mut rest = bounded.as_str();
    while !rest.is_empty() {
        let space = rest
            .find(|character: char| !character.is_whitespace())
            .unwrap_or(rest.len());
        scrubbed.push_str(&rest[..space]);
        rest = &rest[space..];
        if rest.is_empty() {
            break;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..end];
        rest = &rest[end..];
        if redact_next {
            if AUTH_SCHEMES.contains(&trim_word(word).to_ascii_lowercase().as_str()) {
                scrubbed.push_str(word);
            } else {
                scrubbed.push_str(REDACTED);
                redact_next = false;
            }
            continue;
        }
        let (word, next) = scrub_word(word);
        scrubbed.push_str(&word);
        redact_next = next;
    }
    let mut kept: String = scrubbed.chars().take(limit).collect();
    if scrubbed.chars().count() > limit {
        kept.push('…');
    }
    kept
}

fn scrub_word(word: &str) -> (String, bool) {
    if trim_word(word).eq_ignore_ascii_case(BEARER) {
        return (word.to_owned(), true);
    }
    if let Some(scheme) = word.find("://") {
        return (scrub_url(word, scheme + 3), false);
    }
    if let Some(query) = word.find('?') {
        if word[query..].contains('=') {
            return (format!("{}?{REDACTED}", &word[..query]), false);
        }
    }
    if let Some(split) = word.find(['=', ':']) {
        let key = trim_word(&word[..split]).to_ascii_lowercase();
        if SECRET_KEYS.iter().any(|secret| key.contains(secret)) {
            if trim_word(&word[split + 1..]).is_empty() {
                return (word.to_owned(), true);
            }
            return (format!("{}{REDACTED}", &word[..=split]), false);
        }
    }
    (redact_tokens(word), false)
}

fn scrub_url(word: &str, authority: usize) -> String {
    let (head, tail) = word.split_at(authority);
    let authority_end = tail.find(['/', '?', '#']).unwrap_or(tail.len());
    let (host, path) = tail.split_at(authority_end);
    let host = match host.rfind('@') {
        Some(at) => format!("{REDACTED}@{}", &host[at + 1..]),
        None => host.to_owned(),
    };
    let path = match path.find(['?', '#']) {
        Some(cut) => {
            let closing = path
                .trim_end_matches([')', ']', '}', '"', '\'', '>', ',', ';'])
                .len()
                .max(cut + 1);
            format!(
                "{}{}{REDACTED}{}",
                &path[..cut],
                &path[cut..=cut],
                &path[closing..]
            )
        }
        None => path.to_owned(),
    };
    format!("{head}{host}{path}")
}

fn redact_tokens(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut index = 0;
    let mut after_alphanumeric = false;
    while index < word.len() {
        if !after_alphanumeric {
            if let Some(length) = secret_at(&word[index..]) {
                out.push_str(REDACTED);
                index += length;
                after_alphanumeric = true;
                continue;
            }
        }
        let character = word[index..]
            .chars()
            .next()
            .expect("index stays on a character boundary");
        out.push(character);
        after_alphanumeric = character.is_alphanumeric();
        index += character.len_utf8();
    }
    out
}

fn secret_at(text: &str) -> Option<usize> {
    let run = text
        .find(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
        })
        .unwrap_or(text.len());
    let token = &text[..run];
    let prefixed = SECRET_PREFIXES
        .iter()
        .any(|prefix| token.starts_with(prefix) && token.len() >= prefix.len() + MIN_SECRET_TAIL);
    let web_token = token.starts_with("eyJ") && token.matches('.').count() >= 2;
    (prefixed || web_token).then_some(run)
}

fn trim_word(word: &str) -> &str {
    word.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set in the child copy of the test binary that
    /// `a_sidecar_panic_is_appended_to_the_boot_failure_log` starts.
    const CHILD_DIR: &str = "TIDEBREAK_TEST_BROKER_PANIC_DIR";

    /// A panic in the sidecar reaches the desktop's boot failure log with its
    /// scrubbed message, location, thread, and backtrace. The hook is
    /// process-global, so the panic runs in a child copy of this test binary.
    #[test]
    fn a_sidecar_panic_is_appended_to_the_boot_failure_log() {
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            install_panic_hook(Path::new(&dir));
            let panicked = std::thread::Builder::new()
                .name("broker-probe".to_owned())
                .spawn(|| panic!("sidecar probe panic at https://example.test/cb?code=abc"))
                .unwrap()
                .join();
            assert!(panicked.is_err());
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "panic_log::tests::a_sidecar_panic_is_appended_to_the_boot_failure_log",
                "--exact",
                "--test-threads=1",
                // The harness would otherwise capture what the hook prints.
                "--nocapture",
            ])
            .env(CHILD_DIR, dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "the child run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let log = std::fs::read_to_string(dir.path().join(BOOT_FAILURE_LOG)).unwrap();
        assert!(
            log.contains("host broker panic in thread 'broker-probe'"),
            "{log}"
        );
        assert!(
            log.contains(": sidecar probe panic at https://example.test/cb?[redacted]"),
            "the message is scrubbed like the server's log: {log}"
        );
        assert!(!log.contains("code=abc"), "{log}");
        assert!(log.contains("src/panic_log.rs:"), "{log}");
        assert!(log.contains("\nbacktrace:\n"), "{log}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("tidebreak-host-broker: host broker panic"),
            "the report also goes to stderr, which the desktop forwards: {stderr}"
        );
    }

    #[test]
    fn a_large_log_takes_only_the_headline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(BOOT_FAILURE_LOG),
            vec![b'x'; usize::try_from(FULL_REPORT_LIMIT_BYTES).unwrap()],
        )
        .unwrap();
        append(
            dir.path(),
            "headline only",
            "headline only\nbacktrace:\n  0: frame",
        )
        .unwrap();

        let log = std::fs::read_to_string(dir.path().join(BOOT_FAILURE_LOG)).unwrap();
        assert!(log.ends_with(" headline only\n"));
        assert!(!log.contains("backtrace:"));
    }
}
