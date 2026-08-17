//! Launch-plan composition and the permission-bypass denylist.

use std::path::PathBuf;

/// A composed argv / cwd / env for one engine child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// Argument vector, including argv0.
    pub argv: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Environment additions (not the full user env).
    pub env: Vec<(String, String)>,
}

/// A launch plan contained a known permission-bypass flag.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("launch plan contains a permission-bypass flag: {0}")]
pub struct BypassFlagError(pub String);

/// Known bypass flags. Tidebreak never composes these as a default.
const DENIED_EXACT: &[&str] = &[
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "--always-approve",
    "--yolo",
];

/// Bypass mode values that can appear as a `--permission-mode` argument.
const DENIED_VALUES: &[&str] = &["bypassPermissions"];

/// Whether a composed launch plan may include the engine's bypass flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassPolicy {
    /// Plan / Ask / Auto: no known bypass flag may appear, including extras.
    Forbidden,
    /// Allow: the adapter may compose the engine's documented bypass.
    Permitted,
}

/// Reject a composed launch plan that includes a permission-bypass flag,
/// including user-supplied extras. Use [`validate_launch_plan_with`] when
/// the session is in Allow.
pub fn validate_launch_plan(plan: &LaunchPlan) -> Result<(), BypassFlagError> {
    validate_launch_plan_with(plan, BypassPolicy::Forbidden)
}

/// Reject a composed launch plan under the given bypass policy.
pub fn validate_launch_plan_with(
    plan: &LaunchPlan,
    policy: BypassPolicy,
) -> Result<(), BypassFlagError> {
    if policy == BypassPolicy::Permitted {
        return Ok(());
    }
    for arg in &plan.argv {
        if DENIED_VALUES.contains(&arg.as_str()) {
            return Err(BypassFlagError(arg.clone()));
        }
        if arg.starts_with('-') && is_bypass_flag(arg) {
            return Err(BypassFlagError(arg.clone()));
        }
    }
    Ok(())
}

fn is_bypass_flag(arg: &str) -> bool {
    let token = arg.split('=').next().unwrap_or(arg);
    if DENIED_EXACT.contains(&token) {
        return true;
    }
    if let Some((_, value)) = arg.split_once('=') {
        if DENIED_VALUES.contains(&value) {
            return true;
        }
    }
    // Conservative pattern for obvious equivalents we have not enumerated.
    let lower = token.to_ascii_lowercase();
    if !lower.contains("dangerous") {
        return false;
    }
    let skip_or_bypass = lower.contains("skip") || lower.contains("bypass");
    let target =
        lower.contains("permission") || lower.contains("approval") || lower.contains("sandbox");
    skip_or_bypass && target
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(args: &[&str]) -> LaunchPlan {
        LaunchPlan {
            argv: args.iter().map(|s| (*s).to_owned()).collect(),
            cwd: PathBuf::from("/tmp"),
            env: Vec::new(),
        }
    }

    #[test]
    fn default_print_mode_plan_is_clean() {
        validate_launch_plan(&plan(&[
            "claude",
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "default",
        ]))
        .unwrap();
    }

    #[test]
    fn known_bypass_flags_are_rejected_including_extras() {
        for flag in [
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
            "--dangerously-bypass-approvals-and-sandbox",
            "--dangerously-skip-permissions=true",
            "--always-approve",
            "--yolo",
            "bypassPermissions",
        ] {
            let err = validate_launch_plan(&plan(&["claude", flag])).unwrap_err();
            assert!(
                err.0.contains("dangerous")
                    || err.0.contains("always-approve")
                    || err.0.contains("yolo")
                    || err.0.contains("bypassPermissions"),
                "{flag} => {err}"
            );
        }
    }

    #[test]
    fn obvious_equivalents_are_rejected() {
        let err = validate_launch_plan(&plan(&["claude", "--dangerously-bypass-permissions"]))
            .unwrap_err();
        assert!(err.0.contains("dangerous"));
    }

    #[test]
    fn allow_mode_may_compose_a_bypass_flag() {
        validate_launch_plan_with(
            &plan(&[
                "claude",
                "--dangerously-skip-permissions",
                "--allow-dangerously-skip-permissions",
            ]),
            BypassPolicy::Permitted,
        )
        .unwrap();
    }

    #[test]
    fn prompt_text_is_not_scanned_as_a_flag() {
        validate_launch_plan(&plan(&[
            "claude",
            "-p",
            "this is dangerous, skip the permission for writes",
            "--output-format",
            "stream-json",
        ]))
        .unwrap();
    }
}
