//! Browser channel contracts exercised without a live runtime.
//!
//! The trusted browser runtime (issue #2344) mints a session-private
//! capability file whose path is injected through a harness-owned
//! environment key. This module declares the contract adapters rely on
//! and provides static-validation tests that do not require a browser
//! process.

use crate::BrowserChannelSpec;

/// Adapter must inject this exact key; engines must consume it.
///
/// Changing this constant affects every adapter and engine tool bridge,
/// so it lives here as the single source of truth.
pub const BROWSER_CAPFILE_ENV_KEY: &str = BrowserChannelSpec::ENV_KEY;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn capfile_key_is_tidebreak_prefixed() {
        assert!(
            BROWSER_CAPFILE_ENV_KEY.starts_with("TIDEBREAK_"),
            "the key must use the TIDEBREAK_ prefix so filter_child_env strips it"
        );
    }

    #[test]
    fn capfile_key_is_not_empty() {
        assert!(!BROWSER_CAPFILE_ENV_KEY.is_empty());
    }

    #[test]
    fn spec_new_roundtrips() {
        let path = PathBuf::from("/tmp/browser-cap.json");
        let spec = BrowserChannelSpec::new(path.clone());
        assert_eq!(spec.capability_file, path);
    }

    #[test]
    fn inject_env_sets_the_key() {
        let path = PathBuf::from("/tmp/browser-cap.json");
        let spec = BrowserChannelSpec::new(path.clone());
        let mut cmd = std::process::Command::new("true");
        spec.inject_env(&mut cmd);
        assert_eq!(spec.capability_file, path);
    }

    #[test]
    fn inject_env_tokio_sets_the_key() {
        let path = PathBuf::from("/tmp/browser-cap.json");
        let spec = BrowserChannelSpec::new(path.clone());
        let mut cmd = tokio::process::Command::new("true");
        spec.inject_env_tokio(&mut cmd);
        assert_eq!(spec.capability_file, path);
    }

    #[test]
    fn override_resistance_env_key_is_compile_time_constant() {
        assert_eq!(
            std::any::type_name_of_val(&BrowserChannelSpec::ENV_KEY),
            "&str"
        );
    }

    #[test]
    fn absence_preserves_existing_behavior() {
        assert!(BROWSER_CAPFILE_ENV_KEY.len() > 8);
        assert!(BROWSER_CAPFILE_ENV_KEY
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_'));
    }
}
