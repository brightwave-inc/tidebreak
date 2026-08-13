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
    // Terminals, IDEs, editors, and command launchers: a ControlApp grant
    // over any of these reaches unsandboxed local execution (focus the
    // integrated shell, type a command, press Return) and would bypass the
    // sandboxed exec path. A bundle-id list cannot enumerate every app that
    // embeds a shell; this is the common class. See decision record 0010.
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.",
    "net.kovidgoyal.kitty",
    "org.alacritty",
    "io.alacritty",
    "com.github.wez.wezterm",
    "com.mitchellh.ghostty",
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.visualstudio.code.oss",
    // Cursor's stable ToDesktop-issued bundle id (not a com.cursor.* id).
    "com.todesktop.230313mzl4w4u92",
    "com.exafunction.windsurf",
    "com.apple.dt.Xcode",
    "com.jetbrains.",
    "com.sublimetext.",
    "com.panic.Nova",
    "com.google.android.studio",
    "dev.zed.Zed",
    "org.gnu.Emacs",
    "org.vim.MacVim",
    "com.runningwithcrayons.Alfred",
    "com.raycast.macos",
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
            "dev.warp.Warp-Stable",
            "net.kovidgoyal.kitty",
            "org.alacritty",
            "io.alacritty",
            "com.github.wez.wezterm",
            "com.mitchellh.ghostty",
            "com.microsoft.VSCode",
            "com.microsoft.VSCodeInsiders",
            "com.visualstudio.code.oss",
            "com.todesktop.230313mzl4w4u92",
            "com.exafunction.windsurf",
            "com.apple.dt.Xcode",
            "com.jetbrains.intellij",
            "com.jetbrains.CLion",
            "com.sublimetext.4",
            "com.panic.Nova",
            "com.google.android.studio",
            "dev.zed.Zed",
            "org.gnu.Emacs",
            "org.vim.MacVim",
            "com.runningwithcrayons.Alfred",
            "com.raycast.macos",
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
            "com.microsoft.Word",
            "com.jetbrainsx.intellij",
            "com.example.Mail",
            "",
        ] {
            assert!(!is_blocked_control_bundle(allowed), "{allowed}");
        }
    }
}
