//! Failing CI job logs, downloaded once and written where an agent can read
//! them.
//!
//! The fix-errors action used to hand an agent check names and URLs and let it
//! find the logs itself: two or three `gh` calls before it saw an error line,
//! and then a whole multi-megabyte job log in one read. This module does that
//! fetch on the agent's behalf and bounds it.
//!
//! Files land under the workspace's private root, beside fork transcripts, so
//! Git cannot index them and the session's `allowed_read_roots` already covers
//! them. Each fetch replaces the previous one: only the head being fixed right
//! now is worth keeping on disk.

use std::path::Path;
use std::time::Duration;

use futures::{stream, StreamExt};
use tidebreak_core::{PullRequestCheck, PullRequestCheckBucket};

use super::gh;

/// Directory holding downloaded job logs below the workspace's private root.
const CI_LOGS_DIR: &str = "ci-logs";

/// Failing jobs whose logs are downloaded in one request.
///
/// A run that fails wholesale fails every job in it, and the sixth log adds
/// nothing the first few did not already say. They all go out at once, so the
/// caller waits one request rather than one per job — which is what keeps the
/// whole fetch inside the client's own timeout.
const MAX_JOBS: usize = 6;

/// Largest log written per job, in bytes, header included.
///
/// A long build job produces megabytes. The failure and the step summary sit
/// at the end, so the tail is kept and the header says what was dropped.
const MAX_JOB_LOG_BYTES: usize = 512 * 1024;

/// Per-job deadline, and so the deadline for the whole fetch. Well inside the
/// 30 seconds the renderer gives a GitHub read before it gives up.
const GH_LOG_TIMEOUT: Duration = Duration::from_secs(20);

/// A failing check whose URL does not identify a GitHub Actions job cannot be
/// downloaded through the job-log endpoint. Report it instead of making an
/// empty result look like every failing log was handled.
const UNSUPPORTED_CHECK_LOG_MESSAGE: &str =
    "This failing check does not link to a supported GitHub Actions job, so Tidebreak could not download its log.";

/// One downloaded job log, as the route reports it.
pub(crate) struct WrittenCheckLog {
    /// Check name as the host reports it.
    pub(crate) check: String,
    /// Absolute path, in the form the prompt names and the engine opens.
    pub(crate) path: String,
    /// Bytes on disk.
    pub(crate) byte_len: u64,
    /// True when the file holds only the tail of the job log.
    pub(crate) truncated: bool,
    /// The job's host URL. A check without one has no log to download, so
    /// every written log has one.
    pub(crate) url: String,
}

/// One job whose log could not be read. Never fatal: the other logs still land.
pub(crate) struct CheckLogFailure {
    pub(crate) check: String,
    pub(crate) message: String,
}

pub(crate) struct WrittenCheckLogs {
    pub(crate) logs: Vec<WrittenCheckLog>,
    pub(crate) failures: Vec<CheckLogFailure>,
}

/// A GitHub Actions job, addressed the way its check URL spells it.
#[derive(Debug)]
struct JobRef {
    host: String,
    owner: String,
    repo: String,
    job_id: u64,
}

impl JobRef {
    fn endpoint(&self) -> String {
        format!(
            "repos/{}/{}/actions/jobs/{}/logs",
            self.owner, self.repo, self.job_id
        )
    }
}

/// Read the job id out of a check's URL.
///
/// `gh pr checks` reports an Actions check as
/// `https://<host>/<owner>/<repo>/actions/runs/<run>/job/<job>`, which carries
/// everything the REST log endpoint needs. Anything else — a bare check-run
/// URL, an external CI provider — has no job log to fetch and yields `None`.
fn job_ref_from_check_url(url: &str) -> Option<JobRef> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    if host.is_empty() {
        return None;
    }
    let segments = path
        .trim_end_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    // owner / repo / actions / runs / <run> / job / <job>
    let [owner, repo, "actions", "runs", _run, "job", job] = segments.as_slice() else {
        return None;
    };
    Some(JobRef {
        host: host.to_owned(),
        owner: (*owner).to_owned(),
        repo: (*repo).to_owned(),
        job_id: job.parse().ok()?,
    })
}

/// Download every failing check's job log and publish it under `private_root`.
///
/// Stale files from an earlier fetch are removed once the refresh completes,
/// including when the new head has no downloadable job logs. The directory
/// therefore only ever describes one head.
pub(crate) async fn write_failing_check_logs(
    private_root: &super::scratch::ScratchRoot,
    binary: &Path,
    checks: &[PullRequestCheck],
    head_sha: Option<&str>,
) -> std::io::Result<WrittenCheckLogs> {
    let mut targets = Vec::new();
    let mut failures = Vec::new();
    for check in checks
        .iter()
        .filter(|check| check.bucket == PullRequestCheckBucket::Fail)
    {
        let Some(url) = check.url.as_deref() else {
            failures.push(CheckLogFailure {
                check: check.name.clone(),
                message: UNSUPPORTED_CHECK_LOG_MESSAGE.to_owned(),
            });
            continue;
        };
        let Some(job) = job_ref_from_check_url(url) else {
            failures.push(CheckLogFailure {
                check: check.name.clone(),
                message: UNSUPPORTED_CHECK_LOG_MESSAGE.to_owned(),
            });
            continue;
        };
        if targets.len() < MAX_JOBS {
            // Owned, not borrowed: a `&PullRequestCheck` riding through the
            // stream ties the mapped future to one lifetime, and the handler
            // bound needs it general over any.
            targets.push((check.name.clone(), url.to_owned(), job));
        }
    }

    let fetched = stream::iter(targets)
        .map(|(check, url, job)| {
            let binary = binary.to_path_buf();
            async move {
                let raw = fetch_job_log(&binary, &job).await;
                (check, url, job, raw)
            }
        })
        // Ordered, not unordered: the prompt lists the logs, and a list that
        // reshuffles itself by whichever request finished first reads as noise.
        .buffered(MAX_JOBS)
        .collect::<Vec<_>>()
        .await;

    let dir = super::scratch::scratch_dir(private_root, CI_LOGS_DIR)?;
    let mut logs = Vec::new();
    let mut written_names = Vec::new();
    for (check, url, job, raw) in fetched {
        let raw = match raw {
            Ok(raw) => raw,
            Err(message) => {
                failures.push(CheckLogFailure { check, message });
                continue;
            }
        };
        let rendered = render_job_log(&check, &url, head_sha, &raw);
        let name = log_file_name(&check, job.job_id);
        dir.publish(std::ffi::OsStr::new(&name), rendered.text.as_bytes())
            .await?;
        logs.push(WrittenCheckLog {
            check,
            path: private_root
                .path()
                .join(CI_LOGS_DIR)
                .join(&name)
                .display()
                .to_string(),
            byte_len: rendered.text.len() as u64,
            truncated: rendered.truncated,
            url,
        });
        written_names.push(name);
    }
    prune_stale_logs(&dir, &written_names)?;
    Ok(WrittenCheckLogs { logs, failures })
}

async fn fetch_job_log(binary: &Path, job: &JobRef) -> Result<String, String> {
    let endpoint = job.endpoint();
    let mut args = vec!["api".to_owned(), endpoint];
    if job.host != "github.com" {
        args.extend(["--hostname".to_owned(), job.host.clone()]);
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    // `cwd` is unused by an absolute REST read, and this runs for a workspace
    // whose worktree may be mid-rebase.
    gh::run_gh(Path::new("."), binary, &borrowed, GH_LOG_TIMEOUT).await
}

struct RenderedLog {
    text: String,
    truncated: bool,
}

/// Fit one job log into [`MAX_JOB_LOG_BYTES`], keeping the end.
///
/// The header is written first so a reader opening the file sees which check
/// it belongs to and whether anything is missing before the first log line.
/// The 128 bytes held back cover the truncation notice, whose own length
/// depends on how much was dropped.
fn render_job_log(check: &str, url: &str, head_sha: Option<&str>, raw: &str) -> RenderedLog {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut header = format!("# Failing check: {check}\n# Job: {url}\n");
    if let Some(sha) = head_sha {
        header.push_str(&format!("# Head: {sha}\n"));
    }
    let budget = MAX_JOB_LOG_BYTES.saturating_sub(header.len() + 128);
    if raw.len() <= budget {
        header.push('\n');
        header.push_str(raw);
        return RenderedLog {
            text: header,
            truncated: false,
        };
    }
    // Cut on a character boundary first, then on the next line boundary, so
    // the first retained line is both valid UTF-8 and whole.
    let mut cut = raw.len() - budget;
    while cut < raw.len() && !raw.is_char_boundary(cut) {
        cut += 1;
    }
    let tail = &raw[cut..];
    let tail = tail.find('\n').map_or(tail, |index| &tail[index + 1..]);
    let dropped = raw.len() - tail.len();
    header.push_str(&format!(
        "# Truncated: the first {dropped} bytes were dropped. This is the tail \
         of the job log.\n\n"
    ));
    header.push_str(tail);
    RenderedLog {
        text: header,
        truncated: true,
    }
}

/// `clippy-97078611349.log` — the check name a reader recognizes, plus the job
/// id that makes it unique when one workflow runs a check name twice.
fn log_file_name(check: &str, job_id: u64) -> String {
    let mut slug = String::new();
    for character in check.chars() {
        if slug.len() >= 60 {
            break;
        }
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "check" } else { slug };
    format!("{slug}-{job_id}.log")
}

/// Drop files this fetch did not write, so the directory describes one head.
///
/// Only `.log` files are Tidebreak's to remove. Symlinks are unlinked rather
/// than followed, as they are everywhere else in private storage.
fn prune_stale_logs(dir: &super::scratch::ScratchDir, keep: &[String]) -> std::io::Result<()> {
    for entry in dir.read_dir()? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(text) = name.to_str() else { continue };
        if !text.ends_with(".log") || keep.iter().any(|kept| kept.as_str() == text) {
            continue;
        }
        let metadata = dir.symlink_metadata(&name)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            continue;
        }
        dir.remove_file(&name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_actions_check_url_carries_its_job() {
        let job = job_ref_from_check_url(
            "https://github.com/brightwave-inc/tidebreak/actions/runs/32664268801/job/97255126659",
        )
        .expect("actions job url");
        assert_eq!(job.host, "github.com");
        assert_eq!(job.owner, "brightwave-inc");
        assert_eq!(job.repo, "tidebreak");
        assert_eq!(job.job_id, 97_255_126_659);
        assert_eq!(
            job.endpoint(),
            "repos/brightwave-inc/tidebreak/actions/jobs/97255126659/logs"
        );
    }

    #[test]
    fn an_enterprise_host_keeps_its_hostname() {
        let job = job_ref_from_check_url(
            "https://github.example.com/acme/widgets/actions/runs/12/job/34",
        )
        .expect("enterprise job url");
        assert_eq!(job.host, "github.example.com");
        assert_eq!(job.job_id, 34);
    }

    /// A check-run URL and an external provider have no job log to fetch.
    #[test]
    fn a_url_without_a_job_segment_is_skipped() {
        assert!(job_ref_from_check_url(
            "https://github.com/brightwave-inc/tidebreak/runs/97078624591"
        )
        .is_none());
        assert!(job_ref_from_check_url(
            "https://github.com/acme/widgets/actions/runs/12/job/not-a-number"
        )
        .is_none());
        assert!(job_ref_from_check_url("https://buildkite.com/acme/widgets/builds/12").is_none());
        assert!(job_ref_from_check_url("not a url").is_none());
    }

    #[test]
    fn a_short_log_is_written_whole() {
        let rendered = render_job_log(
            "clippy",
            "https://example.test/job",
            Some("abc123"),
            "one\ntwo\n",
        );
        assert!(!rendered.truncated);
        assert!(rendered.text.contains("# Failing check: clippy"));
        assert!(rendered.text.contains("# Head: abc123"));
        assert!(rendered.text.ends_with("one\ntwo\n"));
        assert!(!rendered.text.contains("# Truncated"));
    }

    /// The failure is at the end of a build log, so the tail is what survives.
    #[test]
    fn a_long_log_keeps_its_tail() {
        let mut raw = "filler line that is here only to take up room\n".repeat(20_000);
        raw.push_str("##[error]the thing that actually broke\n");
        let rendered = render_job_log("Rust", "https://example.test/job", None, &raw);
        assert!(rendered.truncated);
        assert!(rendered.text.len() <= MAX_JOB_LOG_BYTES);
        assert!(rendered.text.contains("# Truncated: the first "));
        assert!(rendered
            .text
            .ends_with("##[error]the thing that actually broke\n"));
        // The tail starts on a line boundary rather than mid-word.
        let body = rendered.text.split("\n\n").nth(1).expect("log body");
        assert!(body.starts_with("filler line that is here"));
    }

    #[test]
    fn a_leading_byte_order_mark_is_dropped() {
        let rendered = render_job_log(
            "clippy",
            "https://example.test/job",
            None,
            "\u{feff}first\n",
        );
        assert!(rendered.text.ends_with("\nfirst\n"));
    }

    #[test]
    fn a_file_name_slugs_the_check_and_keeps_the_job_id() {
        assert_eq!(log_file_name("clippy", 12), "clippy-12.log");
        assert_eq!(log_file_name("Analyze (rust)", 12), "analyze-rust-12.log");
        assert_eq!(log_file_name("ci / ui", 12), "ci-ui-12.log");
        assert_eq!(log_file_name("///", 12), "check-12.log");
    }

    #[tokio::test]
    async fn a_second_fetch_prunes_the_previous_heads_files() {
        let directory = tempfile::tempdir().expect("temp root");
        let root = super::super::scratch::ScratchRoot::open_for_test(directory.path())
            .expect("scratch root");
        let dir = super::super::scratch::scratch_dir(&root, CI_LOGS_DIR).expect("dir");
        dir.publish(std::ffi::OsStr::new("old-1.log"), b"old")
            .await
            .expect("publish old");
        dir.publish(std::ffi::OsStr::new("new-2.log"), b"new")
            .await
            .expect("publish new");
        std::fs::write(root.path().join(CI_LOGS_DIR).join("notes.txt"), b"keep")
            .expect("unrelated file");

        prune_stale_logs(&dir, &["new-2.log".to_owned()]).expect("prune");

        let logs = root.path().join(CI_LOGS_DIR);
        assert!(!logs.join("old-1.log").exists());
        assert!(logs.join("new-2.log").exists());
        assert!(logs.join("notes.txt").exists());
    }

    #[tokio::test]
    async fn an_unsupported_head_prunes_the_previous_supported_heads_logs() {
        let directory = tempfile::tempdir().expect("temp root");
        let root = super::super::scratch::ScratchRoot::open_for_test(directory.path())
            .expect("scratch root");
        let dir = super::super::scratch::scratch_dir(&root, CI_LOGS_DIR).expect("dir");
        let old_name = log_file_name("clippy", 34);
        dir.publish(std::ffi::OsStr::new(&old_name), b"old supported log")
            .await
            .expect("publish old log");
        let checks = [PullRequestCheck {
            name: "external CI".to_owned(),
            bucket: PullRequestCheckBucket::Fail,
            detail: None,
            url: Some("https://buildkite.com/acme/widgets/builds/12".to_owned()),
        }];

        let written = write_failing_check_logs(
            &root,
            &directory.path().join("gh-must-not-run"),
            &checks,
            Some("new-head"),
        )
        .await
        .expect("refresh unsupported head");

        assert!(written.logs.is_empty());
        assert_eq!(written.failures.len(), 1);
        assert_eq!(written.failures[0].check, "external CI");
        assert_eq!(written.failures[0].message, UNSUPPORTED_CHECK_LOG_MESSAGE);
        assert!(!root.path().join(CI_LOGS_DIR).join(old_name).exists());
    }

    #[tokio::test]
    async fn a_head_without_failures_prunes_the_previous_supported_heads_logs() {
        let directory = tempfile::tempdir().expect("temp root");
        let root = super::super::scratch::ScratchRoot::open_for_test(directory.path())
            .expect("scratch root");
        let dir = super::super::scratch::scratch_dir(&root, CI_LOGS_DIR).expect("dir");
        let old_name = log_file_name("clippy", 34);
        dir.publish(std::ffi::OsStr::new(&old_name), b"old supported log")
            .await
            .expect("publish old log");
        let checks = [PullRequestCheck {
            name: "clippy".to_owned(),
            bucket: PullRequestCheckBucket::Pass,
            detail: None,
            url: Some("https://github.com/acme/widgets/actions/runs/12/job/34".to_owned()),
        }];

        let written = write_failing_check_logs(
            &root,
            &directory.path().join("gh-must-not-run"),
            &checks,
            Some("new-head"),
        )
        .await
        .expect("refresh passing head");

        assert!(written.logs.is_empty());
        assert!(written.failures.is_empty());
        assert!(!root.path().join(CI_LOGS_DIR).join(old_name).exists());
    }
}
