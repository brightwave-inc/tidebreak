//! Browser channel contracts exercised without a live runtime.
//!
//! The trusted browser runtime (issue #2342) mints a session-private
//! capability file whose path is injected through a harness-owned
//! environment key. This module declares the contract adapters rely on
//! and provides static-validation tests that do not require a browser
//! process.

use std::ffi::OsStr;

use crate::BrowserChannelSpec;

/// Adapter must inject this exact key; engines must consume it.
///
/// Changing this constant affects every adapter and engine tool bridge,
/// so it lives here as the single source of truth.
pub const BROWSER_CAPFILE_ENV_KEY: &str = BrowserChannelSpec::ENV_KEY;

/// Resolve an optional browser channel to the one environment pair an
/// adapter may inject. `None` deliberately produces no child environment.
#[must_use]
pub fn browser_env_pair(browser: Option<&BrowserChannelSpec>) -> Option<(&'static str, &OsStr)> {
    browser.map(BrowserChannelSpec::env_pair)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn capfile_key_is_tidebreak_prefixed() {
        assert!(
            BROWSER_CAPFILE_ENV_KEY.starts_with("TIDEBREAK_"),
            "the key must use the TIDEBREAK_ prefix so filter_child_env strips it"
        );
    }

    #[test]
    fn spec_new_roundtrips() {
        let path = PathBuf::from("/tmp/browser-cap.json");
        let spec = BrowserChannelSpec::new(path.clone());
        assert_eq!(spec.capability_file, path);
    }

    #[test]
    fn browser_some_exposes_the_trusted_env_pair() {
        let path = PathBuf::from("/tmp/browser-cap.json");
        let browser = BrowserChannelSpec::new(path.clone());
        let (key, value) = browser_env_pair(Some(&browser)).expect("browser env pair");

        assert_eq!(key, BROWSER_CAPFILE_ENV_KEY);
        assert_eq!(value, path.as_os_str());
    }

    #[test]
    fn reserved_value_cannot_shadow_the_trusted_pair() {
        let attacker_path = "/tmp/attacker-cap.json";
        let mut settings_env = vec![(
            BROWSER_CAPFILE_ENV_KEY.to_ascii_lowercase(),
            attacker_path.to_owned(),
        )];
        settings_env.retain(|(key, _)| !BrowserChannelSpec::is_reserved_env_key(key));
        assert!(
            settings_env.is_empty(),
            "settings override must be rejected"
        );

        let snapshot_env = crate::filter_child_env([(
            OsString::from(BROWSER_CAPFILE_ENV_KEY),
            OsString::from(attacker_path),
        )]);
        assert!(
            snapshot_env.is_empty(),
            "snapshot override must be stripped"
        );

        let trusted_path = PathBuf::from("/tmp/trusted-cap.json");
        let browser = BrowserChannelSpec::new(trusted_path.clone());
        let (key, value) = browser_env_pair(Some(&browser)).expect("trusted browser env pair");
        assert_eq!(key, BROWSER_CAPFILE_ENV_KEY);
        assert_eq!(value, trusted_path.as_os_str());
    }

    #[test]
    fn browser_none_exposes_no_env_pair() {
        assert!(browser_env_pair(None).is_none());
    }
}
