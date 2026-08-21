//! Browser channel contracts exercised without a live runtime.
//!
//! The trusted browser runtime (issue #2342) mints a session-private
//! capability file whose path is injected through a harness-owned
//! environment key. This module declares the contract adapters rely on
//! and owns the single shared helper that composes an engine child's
//! environment. The tests exercise that production helper directly
//! against a Tokio command, so the Some/None/final-override contract is
//! verified on the real adapter path rather than a test-only seam.

use std::ffi::OsString;

use crate::{filter_child_env, BrowserChannelSpec};

/// Adapter must inject this exact key; engines must consume it.
///
/// Changing this constant affects every adapter and engine tool bridge,
/// so it lives here as the single source of truth.
pub const BROWSER_CAPFILE_ENV_KEY: &str = BrowserChannelSpec::ENV_KEY;

/// Apply the complete child environment to a Tokio engine command in the
/// single ordering every adapter must use:
///
/// 1. Clear the inherited process environment.
/// 2. Restore the probe snapshot through [`filter_child_env`], which strips
///    the reserved `TIDEBREAK_` namespace (case-insensitively).
/// 3. Apply the sanitized launch-plan environment. Settings already rejected
///    reserved keys via [`BrowserChannelSpec::is_reserved_env_key`], so this
///    step can carry no adapter-owned override.
/// 4. Inject the optional [`BrowserChannelSpec`] as the final value, making
///    the trusted capability-file path win over any earlier environment entry.
///
/// `browser` is read directly here so all four adapters share one
/// Some/None/final-override path. `None` adds no environment entry.
pub fn apply_child_env_tokio<I>(
    cmd: &mut tokio::process::Command,
    snapshot: I,
    plan_env: &[(String, String)],
    browser: Option<&BrowserChannelSpec>,
) where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    cmd.env_clear();
    for (key, value) in filter_child_env(snapshot) {
        cmd.env(key, value);
    }
    for (key, value) in plan_env {
        cmd.env(key, value);
    }
    if let Some(browser) = browser {
        browser.inject_env_tokio(cmd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Collect the final environment of a Tokio command into a sorted,
    /// deterministic map keyed by the uppercased name. The repo already
    /// uses `as_std().get_envs()` (see `tidebreak-server`'s mcp_config
    /// tests); this helper keeps the assertions below compact.
    fn final_env(
        snapshot: impl IntoIterator<Item = (OsString, OsString)>,
        plan_env: &[(String, String)],
        browser: Option<&BrowserChannelSpec>,
    ) -> std::collections::BTreeMap<String, String> {
        let mut cmd = tokio::process::Command::new("/bin/true");
        apply_child_env_tokio(&mut cmd, snapshot, plan_env, browser);
        cmd.as_std()
            .get_envs()
            .filter_map(|(name, value)| {
                let value = value?;
                Some((
                    name.to_string_lossy().to_ascii_uppercase(),
                    value.to_string_lossy().to_string(),
                ))
            })
            .collect()
    }

    #[test]
    fn capfile_key_is_tidebreak_prefixed() {
        assert!(
            BROWSER_CAPFILE_ENV_KEY.starts_with("TIDEBREAK_"),
            "the key must use the TIDEBREAK_ prefix so filter_child_env strips it"
        );
    }

    /// Fixture bridge command path used by every test spec construction.
    fn bridge_fixture() -> PathBuf {
        PathBuf::from("/usr/local/bin/tidebreak")
    }

    #[test]
    fn spec_new_roundtrips() {
        let path = PathBuf::from("/tmp/browser-cap.json");
        let spec = BrowserChannelSpec::new(path.clone(), bridge_fixture());
        assert_eq!(spec.capability_file, path);
        assert_eq!(spec.bridge_command, bridge_fixture());
    }

    #[test]
    fn apply_with_browser_injects_the_trusted_pair_last() {
        let trusted = PathBuf::from("/tmp/trusted-cap.json");
        let browser = BrowserChannelSpec::new(trusted.clone(), bridge_fixture());

        let env = final_env(Vec::new(), &[], Some(&browser));
        let (key, value) = env
            .iter()
            .find(|(name, _)| name.as_str() == BROWSER_CAPFILE_ENV_KEY)
            .expect("trusted pair was injected");
        assert_eq!(key, BROWSER_CAPFILE_ENV_KEY);
        assert_eq!(value.as_str(), trusted.to_string_lossy().as_ref());
        assert_eq!(
            env.len(),
            1,
            "no other entries when nothing else is supplied"
        );
    }

    #[test]
    fn apply_without_browser_injects_no_browser_entry() {
        let env = final_env(Vec::new(), &[], None);
        assert!(
            !env.contains_key(BROWSER_CAPFILE_ENV_KEY),
            "None must not add a browser capability entry"
        );
        assert!(env.is_empty(), "empty inputs yield an empty environment");
    }

    #[test]
    fn apply_clears_preconfigured_command_environment() {
        const AMBIENT_SENTINEL: &str = "TIDEBREAK_TEST_AMBIENT";

        let mut cmd = tokio::process::Command::new("/bin/true");
        cmd.env(AMBIENT_SENTINEL, "must-be-cleared");

        apply_child_env_tokio(&mut cmd, Vec::new(), &[], None);

        assert!(
            cmd.as_std()
                .get_envs()
                .all(|(name, _)| name != std::ffi::OsStr::new(AMBIENT_SENTINEL)),
            "the helper must clear command entries configured before its trusted environment is applied"
        );
    }

    #[test]
    fn reserved_snapshot_keys_are_stripped_case_insensitively() {
        // A lowercase reserved key in the probe snapshot must not survive,
        // mirroring the case-insensitive `filter_child_env` contract.
        let snapshot = vec![(
            OsString::from("tidebreak_browser_capfile"),
            OsString::from("/tmp/snapshot-cap.json"),
        )];
        let env = final_env(snapshot, &[], None);
        assert!(
            !env.contains_key(BROWSER_CAPFILE_ENV_KEY),
            "a reserved snapshot key must be stripped before the browser step"
        );
        assert!(env.is_empty(), "only the reserved key was supplied");
    }

    #[test]
    fn reserved_plan_keys_are_rejected_case_insensitively() {
        // Settings (plan.env) filtering is the adapter's responsibility, and
        // the `is_reserved_env_key` guard is what makes plan.env trusted by
        // the time it reaches the helper. A mixed-case reserved key must be
        // rejected just like the canonical uppercase form, so no settings
        // value can shadow the trusted browser injection.
        let lower = BROWSER_CAPFILE_ENV_KEY.to_ascii_lowercase();
        let mixed = String::from("Tidebreak_Browser_Capfile");
        for key in [BROWSER_CAPFILE_ENV_KEY, lower.as_str(), mixed.as_str()] {
            assert!(
                BrowserChannelSpec::is_reserved_env_key(key),
                "adapter must reject reserved settings key {key:?} before apply"
            );
        }
    }

    #[test]
    fn trusted_browser_overrides_a_conflicting_plan_value() {
        // The adapter sanitizes plan.env upstream via is_reserved_env_key,
        // so a reserved key never legitimately reaches this helper through
        // plan.env. The helper nonetheless defends ordering in depth by
        // applying the browser injection after plan.env: if a conflicting
        // value for the reserved key is present, the trusted path wins last.
        let trusted = PathBuf::from("/tmp/trusted-cap.json");
        let browser = BrowserChannelSpec::new(trusted.clone(), bridge_fixture());
        let plan_env = vec![(
            BROWSER_CAPFILE_ENV_KEY.to_owned(),
            "/tmp/plan-conflict-cap.json".to_owned(),
        )];
        let env = final_env(Vec::new(), &plan_env, Some(&browser));
        assert_eq!(
            env.get(BROWSER_CAPFILE_ENV_KEY).map(String::as_str),
            Some(trusted.to_string_lossy().as_ref()),
            "the trusted capability-file path is the final child value, even when plan.env carried the reserved key"
        );
        assert_eq!(env.len(), 1, "only the trusted pair survives the override");
    }
}
