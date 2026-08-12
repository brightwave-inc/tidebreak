//! Hard blocklist of applications the computer-use surface may never capture,
//! read, drive, or hold a grant over — independent of consent state.
//!
//! The list is broker-authoritative (the native helper carries a defensive
//! mirror): consent lives in the grant store, and a grant the user somehow
//! holds over one of these bundles must still not authorize anything. Two
//! entry shapes share one list: a plain bundle id matches exactly or as a
//! dotted prefix (`com.apple.Terminal` also blocks a hypothetical
//! `com.apple.Terminal.helper`), and an entry written with a trailing dot is
//! a pure prefix (`io.brightwave.` blocks the product's own bundle id and
//! anything under it, so the agent can never drive the app it lives in).

/// Blocked bundle ids and (trailing-dot) bundle-id prefixes.
pub const BLOCKED_CONTROL_BUNDLES: &[&str] = &[
    // OpenWave itself — the agent must never capture or drive the surface it
    // is being watched through.
    "io.brightwave.tidebreak",
    "io.brightwave.",
    // Terminals: shell access would bypass the sandboxed exec path entirely.
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    // OS security and credential surfaces.
    "com.apple.loginwindow",
    "com.apple.SecurityAgent",
    "com.apple.systempreferences",
    "com.apple.keychainaccess",
];

/// Whether `bundle_id` is blocked from every computer-use operation and from
/// grant creation. Matches each entry exactly or as a dotted prefix, so a
/// lookalike suffix (`xcom.apple.Terminal`) does not match while anything
/// nested under a listed id does.
pub fn is_blocked_control_bundle(bundle_id: &str) -> bool {
    BLOCKED_CONTROL_BUNDLES.iter().any(|entry| {
        let base = entry.strip_suffix('.').unwrap_or(entry);
        bundle_id == base
            || (bundle_id.len() > base.len()
                && bundle_id.starts_with(base)
                && bundle_id.as_bytes()[base.len()] == b'.')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_dotted_prefix_entries_match() {
        for blocked in [
            "io.brightwave.tidebreak",
            "io.brightwave.tidebreak.helper",
            "io.brightwave.anything",
            "com.apple.Terminal",
            "com.apple.Terminal.helper",
            "com.apple.SecurityAgent",
        ] {
            assert!(is_blocked_control_bundle(blocked), "{blocked}");
        }
    }

    #[test]
    fn lookalikes_and_unrelated_apps_are_not_blocked() {
        for allowed in [
            "xcom.apple.Terminal",
            "com.apple.Terminalized",
            "io.brightwavex.openwave",
            "com.apple.Notes",
            "com.example.Mail",
            "",
        ] {
            assert!(!is_blocked_control_bundle(allowed), "{allowed}");
        }
    }
}
